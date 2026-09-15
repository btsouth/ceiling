//! Windows access-control helpers for user-scoped local IPC and secret files.

use std::io;
use std::mem::{size_of, size_of_val};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, GENERIC_READ, HANDLE};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, ACL_SIZE_INFORMATION, AclSizeInformation,
    AddAccessAllowedAce, CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    GetAclInformation, GetFileSecurityW, GetLengthSid, GetSecurityDescriptorDacl,
    GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
    SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetFileSecurityW, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser, WELL_KNOWN_SID_TYPE,
    WinBuiltinAdministratorsSid, WinLocalSystemSid,
};
use windows::Win32::Storage::FileSystem::{FILE_ALL_ACCESS, FILE_READ_DATA};
use windows::Win32::System::SystemServices::{
    ACCESS_ALLOWED_ACE_TYPE, SECURITY_DESCRIPTOR_REVISION,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{BOOL, PCWSTR};

fn windows_error(context: &str, error: impl std::fmt::Debug) -> io::Error {
    io::Error::other(format!("{context}: {error:?}"))
}

/// Owns a protected DACL containing one full-control ACE for the current user.
///
/// The descriptor points into `acl_buffer`, so callers must keep this value alive
/// until the Win32 operation consuming the descriptor has returned.
pub struct CurrentUserOnlySecurityDescriptor {
    acl_buffer: Vec<usize>,
    descriptor: SECURITY_DESCRIPTOR,
}

/// Owns a Win32 `SECURITY_ATTRIBUTES` value while hiding the Windows crate type
/// from callers that only need to pass its raw pointer to an OS-backed API.
pub struct CurrentUserOnlySecurityAttributes<'a> {
    raw: SECURITY_ATTRIBUTES,
    _descriptor: std::marker::PhantomData<&'a mut CurrentUserOnlySecurityDescriptor>,
}

impl CurrentUserOnlySecurityAttributes<'_> {
    pub fn as_mut_ptr(&mut self) -> *mut core::ffi::c_void {
        (&mut self.raw as *mut SECURITY_ATTRIBUTES).cast()
    }
}

impl CurrentUserOnlySecurityDescriptor {
    pub fn new() -> io::Result<Self> {
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
                .map_err(|error| windows_error("OpenProcessToken failed", error))?;

            let result = Self::from_token(token);
            let _ = CloseHandle(token);
            result
        }
    }

    unsafe fn from_token(token: HANDLE) -> io::Result<Self> {
        let mut token_bytes = 0u32;
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut token_bytes) };
        if token_bytes < size_of::<TOKEN_USER>() as u32 {
            return Err(io::Error::other(
                "GetTokenInformation returned an invalid TOKEN_USER size",
            ));
        }

        // Allocate in machine words so TOKEN_USER and its trailing SID stay aligned.
        let token_words = (token_bytes as usize).div_ceil(size_of::<usize>());
        let mut token_buffer = vec![0usize; token_words];
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(token_buffer.as_mut_ptr().cast()),
                token_bytes,
                &mut token_bytes,
            )
        }
        .map_err(|error| windows_error("GetTokenInformation failed", error))?;

        let token_user = unsafe { &*(token_buffer.as_ptr().cast::<TOKEN_USER>()) };
        let sid = token_user.User.Sid;
        let sid_bytes = unsafe { GetLengthSid(sid) } as usize;
        if sid_bytes == 0 {
            return Err(io::Error::other("GetLengthSid returned zero"));
        }

        let acl_bytes = size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() + sid_bytes
            - size_of_val(&ACCESS_ALLOWED_ACE::default().SidStart);
        let acl_words = acl_bytes.div_ceil(size_of::<usize>());
        let mut acl_buffer = vec![0usize; acl_words];
        let acl = acl_buffer.as_mut_ptr().cast::<ACL>();
        unsafe {
            InitializeAcl(acl, acl_bytes as u32, ACL_REVISION)
                .map_err(|error| windows_error("InitializeAcl failed", error))?;
            AddAccessAllowedAce(acl, ACL_REVISION, FILE_ALL_ACCESS.0, sid)
                .map_err(|error| windows_error("AddAccessAllowedAce failed", error))?;
        }

        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let descriptor_ptr =
            PSECURITY_DESCRIPTOR((&mut descriptor as *mut SECURITY_DESCRIPTOR).cast());
        unsafe {
            InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION)
                .map_err(|error| windows_error("InitializeSecurityDescriptor failed", error))?;
            SetSecurityDescriptorDacl(descriptor_ptr, true, Some(acl), false)
                .map_err(|error| windows_error("SetSecurityDescriptorDacl failed", error))?;
            SetSecurityDescriptorControl(descriptor_ptr, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
                .map_err(|error| windows_error("SetSecurityDescriptorControl failed", error))?;
        }

        Ok(Self {
            acl_buffer,
            descriptor,
        })
    }

    fn descriptor_ptr(&mut self) -> PSECURITY_DESCRIPTOR {
        debug_assert!(!self.acl_buffer.is_empty());
        PSECURITY_DESCRIPTOR((&mut self.descriptor as *mut SECURITY_DESCRIPTOR).cast())
    }

    /// Build security attributes suitable for CreateNamedPipeW and similar APIs.
    pub fn security_attributes(&mut self) -> CurrentUserOnlySecurityAttributes<'_> {
        CurrentUserOnlySecurityAttributes {
            raw: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.descriptor_ptr().0,
                bInheritHandle: false.into(),
            },
            _descriptor: std::marker::PhantomData,
        }
    }
}

/// Replace a file or directory DACL with a protected current-user-only ACL.
pub fn restrict_path_to_current_user(path: &Path) -> io::Result<()> {
    let mut security = CurrentUserOnlySecurityDescriptor::new()?;
    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        SetFileSecurityW(
            PCWSTR(wide_path.as_ptr()),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            security.descriptor_ptr(),
        )
        .ok()
        .map_err(|error| windows_error("SetFileSecurityW failed", error))
    }
}

/// True when the DACL grants read to someone other than the current user.
///
/// SYSTEM and Administrators are ignored the way root is on Unix: they can
/// read a 0600 file anyway. `Users`, `Everyone`, `Authenticated Users`, or a
/// specific other account is a leak. Tightening the ACL cannot un-expose that
/// token, so callers should rotate instead of reuse.
pub fn path_dacl_is_readable_by_others(path: &Path) -> io::Result<bool> {
    let (_user_buffer, user_sid) = current_process_user_sid()?;
    let system = well_known_sid(WinLocalSystemSid)?;
    let administrators = well_known_sid(WinBuiltinAdministratorsSid)?;
    let wide_path = wide_path(path);
    let mut descriptor_bytes = 0u32;
    unsafe {
        let _ = GetFileSecurityW(
            PCWSTR(wide_path.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            None,
            0,
            &mut descriptor_bytes,
        );
    }
    if descriptor_bytes == 0 {
        return Err(io::Error::last_os_error());
    }

    let descriptor_words = (descriptor_bytes as usize).div_ceil(size_of::<usize>());
    let mut descriptor_buffer = vec![0usize; descriptor_words];
    let descriptor = PSECURITY_DESCRIPTOR(descriptor_buffer.as_mut_ptr().cast());
    unsafe {
        GetFileSecurityW(
            PCWSTR(wide_path.as_ptr()),
            DACL_SECURITY_INFORMATION.0,
            Some(descriptor),
            descriptor_bytes,
            &mut descriptor_bytes,
        )
        .ok()
        .map_err(|error| windows_error("GetFileSecurityW failed", error))?;

        let mut present = BOOL::default();
        let mut defaulted = BOOL::default();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted)
            .map_err(|error| windows_error("GetSecurityDescriptorDacl failed", error))?;
        // A missing or NULL DACL is "everyone". An empty DACL is deny-all.
        let Some(dacl) = dacl.as_ref().filter(|_| present.as_bool()) else {
            return Ok(true);
        };

        let mut info = ACL_SIZE_INFORMATION::default();
        GetAclInformation(
            dacl,
            (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
        .map_err(|error| windows_error("GetAclInformation failed", error))?;

        for index in 0..info.AceCount {
            let mut ace_ptr = std::ptr::null_mut();
            GetAce(dacl, index, &mut ace_ptr)
                .map_err(|error| windows_error("GetAce failed", error))?;
            let Some(ace) = ace_ptr.cast::<ACCESS_ALLOWED_ACE>().as_ref() else {
                return Err(io::Error::other("GetAce returned a null ACE"));
            };
            if u32::from(ace.Header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
                continue;
            }
            if !ace_grants_read(ace.Mask) {
                continue;
            }
            let ace_sid = PSID(
                std::ptr::from_ref(&ace.SidStart)
                    .cast::<u32>()
                    .cast_mut()
                    .cast(),
            );
            if EqualSid(ace_sid, user_sid).is_ok()
                || EqualSid(ace_sid, system.as_psid()).is_ok()
                || EqualSid(ace_sid, administrators.as_psid()).is_ok()
            {
                continue;
            }
            return Ok(true);
        }
    }
    Ok(false)
}

fn ace_grants_read(mask: u32) -> bool {
    mask & FILE_READ_DATA.0 != 0 || mask & GENERIC_READ.0 != 0 || mask & GENERIC_ALL.0 != 0
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn current_process_user_sid() -> io::Result<(Vec<usize>, PSID)> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|error| windows_error("OpenProcessToken failed", error))?;
        let result = token_user_sid(token);
        let _ = CloseHandle(token);
        result
    }
}

unsafe fn token_user_sid(token: HANDLE) -> io::Result<(Vec<usize>, PSID)> {
    let mut token_bytes = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut token_bytes) };
    if token_bytes < size_of::<TOKEN_USER>() as u32 {
        return Err(io::Error::other(
            "GetTokenInformation returned an invalid TOKEN_USER size",
        ));
    }
    let token_words = (token_bytes as usize).div_ceil(size_of::<usize>());
    let mut token_buffer = vec![0usize; token_words];
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(token_buffer.as_mut_ptr().cast()),
            token_bytes,
            &mut token_bytes,
        )
    }
    .map_err(|error| windows_error("GetTokenInformation failed", error))?;
    let token_user = unsafe { &*token_buffer.as_ptr().cast::<TOKEN_USER>() };
    let sid = token_user.User.Sid;
    Ok((token_buffer, sid))
}

struct OwnedWellKnownSid {
    buffer: Vec<u8>,
}

impl OwnedWellKnownSid {
    fn as_psid(&self) -> PSID {
        PSID(self.buffer.as_ptr().cast_mut().cast())
    }
}

fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> io::Result<OwnedWellKnownSid> {
    let mut sid_bytes = 0u32;
    let _ = unsafe { CreateWellKnownSid(kind, None, None, &mut sid_bytes) };
    if sid_bytes == 0 {
        return Err(io::Error::other("CreateWellKnownSid returned an empty SID"));
    }
    let mut buffer = vec![0u8; sid_bytes as usize];
    unsafe {
        CreateWellKnownSid(
            kind,
            None,
            Some(PSID(buffer.as_mut_ptr().cast())),
            &mut sid_bytes,
        )
    }
    .map_err(|error| windows_error("CreateWellKnownSid failed", error))?;
    Ok(OwnedWellKnownSid { buffer })
}

/// Test helper: stamp a current-user + Everyone DACL so the file is leaked.
#[cfg(test)]
pub(crate) fn grant_everyone_file_access(path: &Path) -> io::Result<()> {
    use windows::Win32::Security::WinWorldSid;

    let (_user_buffer, user_sid) = current_process_user_sid()?;
    let everyone = well_known_sid(WinWorldSid)?;
    let user_sid_bytes = unsafe { GetLengthSid(user_sid) } as usize;
    let everyone_sid_bytes = unsafe { GetLengthSid(everyone.as_psid()) } as usize;
    if user_sid_bytes == 0 || everyone_sid_bytes == 0 {
        return Err(io::Error::other("GetLengthSid returned zero"));
    }

    let ace_sid_offset = size_of_val(&ACCESS_ALLOWED_ACE::default().SidStart);
    let acl_bytes = size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() + user_sid_bytes
        - ace_sid_offset
        + size_of::<ACCESS_ALLOWED_ACE>()
        + everyone_sid_bytes
        - ace_sid_offset;
    let acl_words = acl_bytes.div_ceil(size_of::<usize>());
    let mut acl_buffer = vec![0usize; acl_words];
    let acl = acl_buffer.as_mut_ptr().cast::<ACL>();
    unsafe {
        InitializeAcl(acl, acl_bytes as u32, ACL_REVISION)
            .map_err(|error| windows_error("InitializeAcl failed", error))?;
        AddAccessAllowedAce(acl, ACL_REVISION, FILE_ALL_ACCESS.0, user_sid)
            .map_err(|error| windows_error("AddAccessAllowedAce failed", error))?;
        AddAccessAllowedAce(acl, ACL_REVISION, FILE_ALL_ACCESS.0, everyone.as_psid())
            .map_err(|error| windows_error("AddAccessAllowedAce failed", error))?;
    }

    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let descriptor_ptr = PSECURITY_DESCRIPTOR((&mut descriptor as *mut SECURITY_DESCRIPTOR).cast());
    unsafe {
        InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION)
            .map_err(|error| windows_error("InitializeSecurityDescriptor failed", error))?;
        SetSecurityDescriptorDacl(descriptor_ptr, true, Some(acl), false)
            .map_err(|error| windows_error("SetSecurityDescriptorDacl failed", error))?;
        SetSecurityDescriptorControl(descriptor_ptr, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
            .map_err(|error| windows_error("SetSecurityDescriptorControl failed", error))?;
    }

    let wide = wide_path(path);
    unsafe {
        SetFileSecurityW(
            PCWSTR(wide.as_ptr()),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor_ptr,
        )
        .ok()
        .map_err(|error| windows_error("SetFileSecurityW failed", error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_temp_file_is_not_world_readable_after_restrict() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("serve.token");
        std::fs::write(&path, b"token\n").unwrap();
        restrict_path_to_current_user(&path).unwrap();
        assert!(
            !path_dacl_is_readable_by_others(&path).unwrap(),
            "current-user-only DACL must not count as leaked"
        );
    }

    #[test]
    fn everyone_ace_is_readable_by_others_until_restricted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("serve.token");
        std::fs::write(&path, b"leaked\n").unwrap();
        grant_everyone_file_access(&path).unwrap();
        assert!(
            path_dacl_is_readable_by_others(&path).unwrap(),
            "Everyone ACE must count as a leaked token file"
        );
        restrict_path_to_current_user(&path).unwrap();
        assert!(
            !path_dacl_is_readable_by_others(&path).unwrap(),
            "tightening must drop the Everyone ACE"
        );
    }
}
