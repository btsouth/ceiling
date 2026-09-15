//! Windows Authenticode verification for downloaded installers.
//!
//! `WinVerifyTrust` establishes that the file is intact and chains to a trusted
//! code-signing certificate. We then pin the exact DER-encoded publisher name
//! from the signer selected by that same trust decision. Ceiling uses Azure
//! Trusted Signing, whose short-lived leaf certificate and public key rotate on
//! every signing run, so a leaf thumbprint/public-key pin would reject the next
//! legitimate release. The verified publisher identity is stable across those
//! rotations and still rejects a valid signature issued to any other publisher.

use sha2::{Digest, Sha256};
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::Foundation::{HWND, TRUST_E_NOSIGNATURE};
use windows::Win32::Security::WinTrust::{
    WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO,
    WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE,
    WTD_STATEACTION_VERIFY, WTD_UI_NONE, WTD_UICONTEXT_INSTALL, WTHelperGetProvCertFromChain,
    WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
};
use windows::core::PCWSTR;

const EXPECTED_PUBLISHER: &str = "Brandon South";

// SHA256 of the DER-encoded X.500 subject:
// CN=Brandon South, O=Brandon South, L=Wilmore, S=ky, C=US
// Verified against independently signed Ceiling 1.5.27, 1.5.29, and 1.5.30
// installers; their leaf keys differ while this publisher identity is stable.
const EXPECTED_PUBLISHER_SUBJECT_SHA256: &str =
    "00703c4003ef0772739245c13c1da19f5eb5caa2849f00f6ddfbe25b09e02d4d";

pub(super) fn verify(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("Installer not found: {}", path.display()));
    }

    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut file_info = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(wide_path.as_ptr()),
        ..Default::default()
    };
    let mut trust_data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &mut file_info,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        // Check revocation for the whole chain against locally cached data only,
        // so a revoked signing certificate is rejected without blocking the
        // launch path on revocation network access.
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL,
        dwUIContext: WTD_UICONTEXT_INSTALL,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;

    // SAFETY: all pointers in WINTRUST_DATA remain valid until the matching
    // WTD_STATEACTION_CLOSE call below. UI is disabled, and the path is
    // NUL-terminated for the duration of both calls.
    let status = unsafe {
        WinVerifyTrust(
            HWND::default(),
            &mut action,
            (&mut trust_data as *mut WINTRUST_DATA).cast::<c_void>(),
        )
    };

    let result = (|| {
        validate_wintrust_status(status)?;
        let publisher_fingerprint = publisher_subject_fingerprint(&trust_data)?;
        validate_publisher_fingerprint(&publisher_fingerprint)
    })();

    trust_data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: this releases the state allocated by the VERIFY call. Microsoft
    // requires CLOSE for every VERIFY invocation, including failed ones.
    unsafe {
        let _ = WinVerifyTrust(
            HWND::default(),
            &mut action,
            (&mut trust_data as *mut WINTRUST_DATA).cast::<c_void>(),
        );
    }

    result
}

fn validate_wintrust_status(status: i32) -> Result<(), String> {
    if status == 0 {
        return Ok(());
    }

    if status == TRUST_E_NOSIGNATURE.0 {
        return Err("The downloaded installer is unsigned. Ceiling rejected it.".to_string());
    }

    Err(format!(
        "The downloaded installer has an invalid or untrusted Windows signature (WinVerifyTrust 0x{:08X}). Ceiling rejected it.",
        status as u32
    ))
}

fn validate_publisher_fingerprint(actual: &str) -> Result<(), String> {
    if actual.eq_ignore_ascii_case(EXPECTED_PUBLISHER_SUBJECT_SHA256) {
        Ok(())
    } else {
        Err(format!(
            "The downloaded installer was not signed by the expected Ceiling publisher ({EXPECTED_PUBLISHER}). Ceiling rejected it."
        ))
    }
}

fn publisher_subject_fingerprint(trust_data: &WINTRUST_DATA) -> Result<String, String> {
    // SAFETY: these helper pointers belong to trust_data.hWVTStateData and are
    // read only before the matching WTD_STATEACTION_CLOSE call.
    unsafe {
        let provider_data = WTHelperProvDataFromStateData(trust_data.hWVTStateData);
        if provider_data.is_null() {
            return Err(
                "Windows verified the signature but returned no publisher data.".to_string(),
            );
        }

        let signer = WTHelperGetProvSignerFromChain(provider_data, 0, false, 0);
        if signer.is_null() {
            return Err(
                "Windows verified the signature but returned no publisher signer.".to_string(),
            );
        }

        let provider_cert = WTHelperGetProvCertFromChain(signer, 0);
        if provider_cert.is_null() || (*provider_cert).pCert.is_null() {
            return Err(
                "Windows verified the signature but returned no publisher certificate.".to_string(),
            );
        }

        let cert_info = (*(*provider_cert).pCert).pCertInfo;
        if cert_info.is_null() {
            return Err(
                "Windows verified the signature but returned incomplete publisher data."
                    .to_string(),
            );
        }

        let subject = &(*cert_info).Subject;
        if subject.cbData == 0 || subject.pbData.is_null() {
            return Err(
                "Windows verified the signature but returned an empty publisher identity."
                    .to_string(),
            );
        }

        let subject_der = std::slice::from_raw_parts(subject.pbData, subject.cbData as usize);
        Ok(format!("{:x}", Sha256::digest(subject_der)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_ceiling_publisher_is_accepted() {
        assert!(validate_wintrust_status(0).is_ok());
        assert!(validate_publisher_fingerprint(EXPECTED_PUBLISHER_SUBJECT_SHA256).is_ok());
    }

    #[test]
    fn unsigned_installer_is_rejected() {
        let error = validate_wintrust_status(TRUST_E_NOSIGNATURE.0).unwrap_err();
        assert!(error.contains("unsigned"));
        assert!(error.contains("rejected"));
    }

    #[test]
    fn unexpected_signer_is_rejected() {
        let error = validate_publisher_fingerprint(&"0".repeat(64)).unwrap_err();
        assert!(error.contains("expected Ceiling publisher"));
        assert!(error.contains(EXPECTED_PUBLISHER));
    }

    #[test]
    fn unsigned_file_fails_live_winverifytrust_check() {
        let temp = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(temp.path(), b"not a signed installer").expect("write fixture");

        let error = verify(temp.path()).unwrap_err();

        assert!(error.contains("unsigned") || error.contains("invalid or untrusted"));
    }

    #[test]
    fn valid_signature_from_another_publisher_is_rejected() {
        // GitHub CLI carries an embedded Authenticode signature, unlike many
        // Windows inbox binaries whose trust comes from a separate catalog.
        let Ok(other_publisher_binary) = which::which("gh.exe") else {
            eprintln!("skipped: gh.exe not found on PATH");
            return;
        };

        let error = verify(&other_publisher_binary).unwrap_err();

        assert!(error.contains("expected Ceiling publisher"), "{error}");
    }

    #[test]
    fn signed_ceiling_release_fixture_is_accepted_when_provided() {
        let Some(path) = std::env::var_os("CEILING_SIGNED_INSTALLER_TEST_PATH") else {
            eprintln!("skipped: CEILING_SIGNED_INSTALLER_TEST_PATH not set");
            return;
        };

        verify(Path::new(&path)).expect("signed Ceiling release fixture");
    }
}
