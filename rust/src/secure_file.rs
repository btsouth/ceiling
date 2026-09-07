//! Small helper for storing local secret-bearing JSON files.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine;
use serde::{Deserialize, Serialize};

const FORMAT: &str = "codexbar.secure-file";
const VERSION: u32 = 1;
const WINDOWS_DPAPI_USER: &str = "windows-dpapi-user";
const WINDOWS_DPAPI_MACHINE: &str = "windows-dpapi-machine";
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// How long an acquirer waits for a live holder before giving up.
const STATE_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const STATE_LOCK_RETRY: std::time::Duration = std::time::Duration::from_millis(20);
/// How old an exclusive-create sibling must be before it is treated as a crash
/// leftover rather than a live holder.
///
/// Deliberately much longer than `STATE_LOCK_TIMEOUT`: the sibling has no
/// kernel-backed release, so staleness is the only crash recovery there is, but
/// a holder that is merely slow must never be mistaken for a dead one. Because
/// this exceeds the acquire timeout, a waiter that arrives while a holder is
/// working can never outlast it and steal the lock (SBS-947).
const STATE_LOCK_STALE: std::time::Duration = std::time::Duration::from_secs(120);
/// How long to keep retrying a name whose deletion has begun.
///
/// Windows reports a file that is unlinked but still has a handle closing as
/// `ERROR_ACCESS_DENIED`, not `ERROR_FILE_EXISTS`, so a waiter that races the
/// holder's release sees a name that is neither takeable nor gone. That clears
/// in well under a millisecond. A real permission problem does not, so the
/// spin is bounded and the original error is reported after it.
///
/// Unix unlinks by name and never reports this state, so the retry is compiled
/// in everywhere but only enabled where it can happen.
const DELETE_PENDING_GRACE: std::time::Duration = std::time::Duration::from_millis(250);
const RETRIES_DELETE_PENDING: bool = cfg!(windows);

/// Serialize read-modify-write transactions across Ceiling processes.
///
/// Every settings and credential store shares one lock so operations spanning
/// multiple files cannot interleave with a writer for any one of those files.
///
/// `flock` is preferred because the kernel drops it if the process dies. When
/// the mount cannot flock (NFS without lockd, some FUSE/SMB), writers serialize
/// with an exclusive-create sibling and a staleness timeout. A lock file this
/// user cannot open fails the write and names the path: a live holder still has
/// that inode open, so unlinking it would put two writers on two inodes.
/// Unknown lock errors fail the write too; they are not treated as "no lock
/// needed" (SBS-947).
pub fn with_state_write_lock<T>(operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let lock_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Ceiling")
        .join("state-write.lock");
    with_state_write_lock_at(&lock_path, operation)
}

pub(crate) fn with_state_write_lock_at<T>(
    lock_path: &Path,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _lock = StateWriteLock::acquire(lock_path)?;
    operation()
}

/// Serialize writes to a third-party state file without folding unrelated
/// credential paths into Ceiling's global state lock.
pub(crate) fn with_file_write_lock<T>(
    file_path: &Path,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let parent = file_path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "state file path has no parent")
    })?;
    let file_name = file_path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "state file path has no file name",
        )
    })?;
    let mut lock_name = OsString::from(".");
    lock_name.push(file_name);
    lock_name.push(".ceiling-write.lock");
    with_state_write_lock_at(&parent.join(lock_name), operation)
}

/// Sibling used when `flock` is unsupported. `create_new` is atomic on NFSv3+,
/// SMB, and FUSE even when `flock` returns `ENOLCK` / `ENOTSUP`.
fn exclusive_lock_path(lock_path: &Path) -> PathBuf {
    let mut name = lock_path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("state-write.lock"))
        .to_os_string();
    name.push(".excl");
    match lock_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(name),
        _ => PathBuf::from(name),
    }
}

struct StateWriteLock {
    /// `None` when this holder used exclusive-create or the test helper.
    #[cfg(windows)]
    handle: Option<windows::Win32::Foundation::HANDLE>,
    /// Open lock file whose exclusive flock is released when this is dropped.
    /// `None` when this holder used exclusive-create or the test helper.
    #[cfg(not(windows))]
    _file: Option<std::fs::File>,
    /// Set when this holder created the exclusive-create sibling. Unlinked on drop.
    exclusive_path: Option<PathBuf>,
    /// Identity of the sibling this holder created, so drop can tell its own
    /// file from a replacement made after a stale takeover.
    #[cfg(unix)]
    exclusive_id: Option<(u64, u64)>,
}

/// Outcome of one attempt to take the state-write lock.
enum LockAttempt {
    Acquired(StateWriteLock),
    /// Another live holder has the lock, so the attempt is worth repeating.
    Contended,
    /// The mount does not implement `flock`. Exclusive-create is the fallback.
    ///
    /// Only `classify_lock_failure` builds this, and Windows locks through
    /// `CreateFileW` instead, so there it is matched but never constructed.
    #[cfg_attr(windows, allow(dead_code))]
    FlockUnsupported(io::Error),
    /// The lock file exists but this process cannot open it.
    Unopenable(io::Error),
    /// Something unrelated to locking went wrong, or a lock error we do not
    /// know how to interpret. The caller sees the error; the write does not
    /// proceed unserialized.
    Failed(io::Error),
}

enum LockFileAge {
    Missing,
    Fresh,
    Stale,
    Unknown(io::Error),
}

enum Repair {
    /// The lock file is gone, so the open failure was transient. Try again.
    Done,
    Failed(io::Error),
}

impl StateWriteLock {
    fn acquire(path: &Path) -> io::Result<Self> {
        Self::acquire_with(path, Self::try_acquire)
    }

    fn acquire_with(
        path: &Path,
        mut attempt: impl FnMut(&Path) -> LockAttempt,
    ) -> io::Result<Self> {
        let deadline = std::time::Instant::now() + STATE_LOCK_TIMEOUT;
        loop {
            match attempt(path) {
                LockAttempt::Acquired(lock) => return Ok(lock),
                LockAttempt::Failed(error) => return Err(error),
                LockAttempt::FlockUnsupported(error) => {
                    // `attempt` is usually `try_acquire`, which already falls
                    // back. A custom attempt that reports this still serializes
                    // rather than writing unserialized.
                    match Self::try_exclusive_create(path) {
                        LockAttempt::Acquired(lock) => {
                            tracing::info!(
                                lock_path = %path.display(),
                                %error,
                                "flock is unsupported here; serializing the write with exclusive-create"
                            );
                            return Ok(lock);
                        }
                        LockAttempt::Contended => {}
                        LockAttempt::Failed(fallback) => {
                            return Err(io::Error::new(
                                fallback.kind(),
                                format!(
                                    "flock is unsupported ({error}) and exclusive-create failed: {fallback}"
                                ),
                            ));
                        }
                        other => {
                            return Err(lock_attempt_unexpected(
                                other,
                                "exclusive-create fallback",
                            ));
                        }
                    }
                }
                // Falls through to the deadline check and the sleep rather
                // than retrying straight away: a lock file that keeps
                // appearing and vanishing must still time out.
                LockAttempt::Unopenable(error) => match try_repair_unopenable(path, &error) {
                    Repair::Done => {}
                    Repair::Failed(repair) => return Err(repair),
                },
                LockAttempt::Contended => {}
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "state store is locked",
                ));
            }
            std::thread::sleep(STATE_LOCK_RETRY);
        }
    }

    /// A lock object that owns nothing, for tests that need an `Acquired` value.
    #[cfg(test)]
    fn empty() -> Self {
        Self {
            #[cfg(windows)]
            handle: None,
            #[cfg(not(windows))]
            _file: None,
            exclusive_path: None,
            #[cfg(unix)]
            exclusive_id: None,
        }
    }

    fn try_acquire(path: &Path) -> LockAttempt {
        match Self::try_primary(path) {
            LockAttempt::FlockUnsupported(_) => Self::try_exclusive_create(path),
            LockAttempt::Unopenable(error) => Self::after_unopenable(path, error),
            other => other,
        }
    }

    fn after_unopenable(path: &Path, error: io::Error) -> LockAttempt {
        match try_repair_unopenable(path, &error) {
            Repair::Done => match Self::try_primary(path) {
                LockAttempt::FlockUnsupported(_) => Self::try_exclusive_create(path),
                LockAttempt::Unopenable(still) => LockAttempt::Failed(still),
                other => other,
            },
            Repair::Failed(error) => LockAttempt::Failed(error),
        }
    }

    fn try_exclusive_create(path: &Path) -> LockAttempt {
        let exclusive = exclusive_lock_path(path);
        let deadline = std::time::Instant::now() + DELETE_PENDING_GRACE;
        loop {
            let attempt = Self::try_exclusive_create_once(&exclusive);
            if !RETRIES_DELETE_PENDING {
                return attempt;
            }
            // A directory sitting on the sibling path is also access-denied on
            // Windows, and unlike a closing handle it never clears. Retrying
            // that only delays a failure the caller needs now.
            let denied = matches!(&attempt, LockAttempt::Failed(error)
                if error.kind() == io::ErrorKind::PermissionDenied)
                && !exclusive.is_dir();
            if !denied || std::time::Instant::now() >= deadline {
                return attempt;
            }
            std::thread::sleep(STATE_LOCK_RETRY);
        }
    }

    fn try_exclusive_create_once(exclusive: &Path) -> LockAttempt {
        let exclusive = exclusive.to_path_buf();
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&exclusive) {
            Ok(file) => {
                #[cfg(unix)]
                let exclusive_id = file_identity(&file);
                #[cfg(windows)]
                let _ = file;
                LockAttempt::Acquired(Self {
                    #[cfg(windows)]
                    handle: None,
                    #[cfg(not(windows))]
                    _file: Some(file),
                    exclusive_path: Some(exclusive),
                    #[cfg(unix)]
                    exclusive_id,
                })
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                match lock_file_age(&exclusive) {
                    LockFileAge::Stale | LockFileAge::Missing => {
                        match std::fs::remove_file(&exclusive) {
                            Ok(()) => LockAttempt::Contended,
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                                LockAttempt::Contended
                            }
                            Err(error) => LockAttempt::Failed(io::Error::new(
                                error.kind(),
                                format!(
                                    "could not remove a stale exclusive-create lock file: {error}"
                                ),
                            )),
                        }
                    }
                    LockFileAge::Fresh => LockAttempt::Contended,
                    LockFileAge::Unknown(error) => LockAttempt::Failed(io::Error::new(
                        error.kind(),
                        format!(
                            "could not tell whether the exclusive-create lock file is stale: {error}"
                        ),
                    )),
                }
            }
            Err(error) => LockAttempt::Failed(io::Error::new(
                error.kind(),
                format!("could not create the exclusive-create lock file: {error}"),
            )),
        }
    }

    #[cfg(windows)]
    fn try_primary(path: &Path) -> LockAttempt {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, OPEN_ALWAYS,
        };
        use windows::core::PCWSTR;

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        match unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        } {
            Ok(handle) => LockAttempt::Acquired(Self {
                handle: Some(handle),
                exclusive_path: None,
                #[cfg(unix)]
                exclusive_id: None,
            }),
            Err(error) => classify_open_failure(&error),
        }
    }

    #[cfg(not(windows))]
    fn try_primary(path: &Path) -> LockAttempt {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = match options.open(path) {
            Ok(file) => file,
            Err(error) => return classify_open_failure(&error),
        };
        match file.try_lock() {
            Ok(()) => LockAttempt::Acquired(Self {
                _file: Some(file),
                exclusive_path: None,
                #[cfg(unix)]
                exclusive_id: None,
            }),
            Err(error) => classify_lock_failure(error),
        }
    }
}

fn lock_attempt_unexpected(attempt: LockAttempt, context: &str) -> io::Error {
    let detail = match attempt {
        LockAttempt::Acquired(_) => "acquired".to_string(),
        LockAttempt::Contended => "contended".to_string(),
        LockAttempt::FlockUnsupported(error) => format!("flock-unsupported: {error}"),
        LockAttempt::Unopenable(error) => format!("unopenable: {error}"),
        LockAttempt::Failed(error) => format!("failed: {error}"),
    };
    io::Error::other(format!(
        "internal lock state {detail} is not valid during {context}"
    ))
}

/// `(device, inode)` of an open file, so a later `stat` of the same path can
/// tell whether it is still the same file.
#[cfg(unix)]
fn file_identity(file: &std::fs::File) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().ok()?;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(unix)]
fn path_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

fn lock_file_age(path: &Path) -> LockFileAge {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => LockFileAge::Missing,
        Err(error) => LockFileAge::Unknown(error),
        Ok(metadata) if metadata.is_dir() => LockFileAge::Unknown(io::Error::new(
            io::ErrorKind::IsADirectory,
            format!("state lock path is a directory: {}", path.display()),
        )),
        Ok(metadata) => match metadata.modified() {
            Err(error) => LockFileAge::Unknown(error),
            Ok(modified) => match std::time::SystemTime::now().duration_since(modified) {
                Ok(age) if age >= STATE_LOCK_STALE => LockFileAge::Stale,
                Ok(_) => LockFileAge::Fresh,
                // The mtime is in the future, so the age is not measurable:
                // NFS server skew, a restored VM, or a clock set forward. Stay
                // on the safe side and treat it as a live holder, but say so,
                // because until the clock catches up this never ages out.
                Err(error) => {
                    tracing::warn!(
                        lock_path = %path.display(),
                        %error,
                        "state lock file is stamped in the future; treating it as held until the clock catches up"
                    );
                    LockFileAge::Fresh
                }
            },
        },
    }
}

/// Decide what to do about a lock file this process cannot open.
///
/// Unlinking is never one of the options. `Drop` deliberately leaves the flock
/// lock file in place, so a live holder still has this inode open; removing it
/// and creating a replacement would put two writers on two different inodes and
/// let both writes run. The flock file's mtime is stamped at first create and
/// never refreshed, so age cannot distinguish a dead leftover from a running
/// holder either. A file this user cannot open is a leftover from a privileged
/// or different-user run, so fail the write and name the path (SBS-947).
fn try_repair_unopenable(path: &Path, open_error: &io::Error) -> Repair {
    if path.is_dir() {
        return Repair::Failed(io::Error::new(
            io::ErrorKind::IsADirectory,
            format!("state lock path is a directory: {}", path.display()),
        ));
    }
    match lock_file_age(path) {
        // Gone between the failed open and now, so nothing is holding it.
        LockFileAge::Missing => Repair::Done,
        _ => Repair::Failed(io::Error::new(
            open_error.kind(),
            format!(
                "could not open the state lock file ({}): {open_error}. \
                 Remove it if no other Ceiling process is running.",
                path.display()
            ),
        )),
    }
}

/// Classify a Win32 failure to open the lock file.
///
/// A sharing or lock violation is a live holder. `ERROR_ACCESS_DENIED` is not:
/// the lock file exists but this user can never open it. That is unopenable,
/// not "no lock needed".
#[cfg(windows)]
fn classify_open_failure(error: &windows::core::Error) -> LockAttempt {
    let io_error = || io::Error::other(format!("could not acquire state lock: {error}"));
    match win32_error_code(error) {
        WIN32_ERROR_SHARING_VIOLATION | WIN32_ERROR_LOCK_VIOLATION => LockAttempt::Contended,
        WIN32_ERROR_ACCESS_DENIED => LockAttempt::Unopenable(io_error()),
        _ => LockAttempt::Failed(io_error()),
    }
}

/// Classify a failure to open the lock file.
///
/// Nothing here means "someone else holds the lock" - `open` does not block on
/// `flock`. A lock file this process can never open is unopenable, not a reason
/// to write unserialized.
#[cfg(not(windows))]
fn classify_open_failure(error: &io::Error) -> LockAttempt {
    let unopenable = matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied
            | io::ErrorKind::IsADirectory
            | io::ErrorKind::ReadOnlyFilesystem
    );
    let error = io::Error::new(
        error.kind(),
        format!("could not open the state lock file: {error}"),
    );
    if unopenable {
        LockAttempt::Unopenable(error)
    } else {
        LockAttempt::Failed(error)
    }
}

/// Classify a `flock` failure on the opened lock file.
///
/// `WouldBlock` is the only answer that means another live holder has the lock.
/// A signal is worth another attempt. `ENOTSUP` means this filesystem cannot
/// flock at all; exclusive-create is the fallback. Every other errno is unknown
/// and fails the write (SBS-947).
///
/// `ENOLCK` is deliberately not a fallback trigger. It also means the kernel
/// lock table is full or `lockd` failed for this one call, and in that case
/// another process can still hold a real flock on the lock file. Falling back
/// would serialize on the sibling instead, which flock holders never take, so
/// both writers would proceed and last-write-wins the credential files.
#[cfg(not(windows))]
fn classify_lock_failure(error: std::fs::TryLockError) -> LockAttempt {
    match error {
        std::fs::TryLockError::WouldBlock => LockAttempt::Contended,
        std::fs::TryLockError::Error(error) if error.kind() == io::ErrorKind::Interrupted => {
            LockAttempt::Contended
        }
        std::fs::TryLockError::Error(error) if is_flock_unsupported(&error) => {
            LockAttempt::FlockUnsupported(io::Error::new(
                error.kind(),
                format!("this filesystem cannot lock the state lock file: {error}"),
            ))
        }
        std::fs::TryLockError::Error(error) => LockAttempt::Failed(io::Error::new(
            error.kind(),
            format!("could not lock the state lock file: {error}"),
        )),
    }
}

#[cfg(not(windows))]
fn is_flock_unsupported(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    // `ENOTSUP` and `EOPNOTSUPP` are one value on Linux but distinct on macOS
    // and the BSDs, where `ENOTSUP` decoded as `Uncategorized` until a recent
    // std change. Read the errno directly so which toolchain built this does
    // not decide whether a mount gets the fallback.
    #[cfg(unix)]
    {
        let raw = error.raw_os_error();
        raw == Some(libc::ENOTSUP) || raw == Some(libc::EOPNOTSUPP)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

impl Drop for StateWriteLock {
    fn drop(&mut self) {
        if let Some(path) = self.exclusive_path.take() {
            // Unlink the sibling only while it is still the file this holder
            // created. If a stale takeover replaced it, the path now belongs to
            // another live writer and removing it would hand the lock to a
            // third (SBS-947).
            #[cfg(unix)]
            let ours = match self.exclusive_id.take() {
                // Anything other than the exact file this holder created is
                // somebody else's lock, including a path that no longer
                // resolves: a takeover may have removed the original and be
                // about to create its own.
                Some(created) => path_identity(&path) == Some(created),
                // Nothing was recorded, so there is nothing to compare against.
                None => true,
            };
            #[cfg(not(unix))]
            let ours = true;
            if ours {
                let _ = std::fs::remove_file(&path);
            }
        }
        #[cfg(windows)]
        if let Some(handle) = self.handle.take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
        }
        // Non-Windows flock: closing `_file` releases the lock. Leave the flock
        // lock file so a leftover after crash is not treated as a live holder,
        // and so unlinking cannot create a second lock inode while another
        // holder still has the original file open.
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ProtectedFile {
    format: String,
    version: u32,
    protection: String,
    payload: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecureFileStatus {
    Missing,
    Plaintext,
    Protected(String),
    Unreadable(String),
}

/// Return a non-secret storage status for diagnostics/UI surfaces.
pub fn status(path: &Path) -> SecureFileStatus {
    if !path.exists() {
        return SecureFileStatus::Missing;
    }

    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) => return SecureFileStatus::Unreadable(e.to_string()),
    };

    let Ok(file) = serde_json::from_str::<ProtectedFile>(&raw) else {
        return SecureFileStatus::Plaintext;
    };

    if file.format != FORMAT {
        return SecureFileStatus::Plaintext;
    }
    if file.version != VERSION {
        return SecureFileStatus::Unreadable(format!(
            "unsupported secure file version {}",
            file.version
        ));
    }

    match file.protection.as_str() {
        WINDOWS_DPAPI_USER | WINDOWS_DPAPI_MACHINE => SecureFileStatus::Protected(file.protection),
        other => {
            SecureFileStatus::Unreadable(format!("unsupported secure file protection {other}"))
        }
    }
}

/// Read a UTF-8 file that may be protected by this module.
pub fn read_string(path: &Path) -> io::Result<String> {
    let raw = std::fs::read_to_string(path)?;
    let Ok(file) = serde_json::from_str::<ProtectedFile>(&raw) else {
        return Ok(raw);
    };

    if file.format != FORMAT {
        return Ok(raw);
    }
    if file.version != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported secure file version {}", file.version),
        ));
    }

    match file.protection.as_str() {
        WINDOWS_DPAPI_USER | WINDOWS_DPAPI_MACHINE => {
            let encrypted = base64::engine::general_purpose::STANDARD
                .decode(file.payload.as_bytes())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let plain = unprotect(&encrypted)?;
            String::from_utf8(plain).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported secure file protection {other}"),
        )),
    }
}

/// Write a UTF-8 file, protecting it with Windows DPAPI when available.
pub fn write_string(path: &Path, contents: &str) -> io::Result<()> {
    let bytes = protected_file_bytes(contents)?;
    atomic_write(path, &bytes)
}

/// Replace a local state file atomically with private permissions.
///
/// The temporary file lives beside the destination so the final rename stays
/// on one filesystem. A crash can leave a harmless temp file, but it cannot
/// truncate the last known-good settings or credential file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write_with_permissions(path, bytes, AtomicWritePermissions::Private)
}

/// Atomically replace a file owned by another application while retaining its
/// existing permission boundary.
pub(crate) fn atomic_write_preserving_permissions(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Follow a symlinked credential path (dotfile managers, WSL shared targets)
    // so the atomic replace updates the target instead of replacing the link.
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    atomic_write_with_permissions(&resolved, bytes, AtomicWritePermissions::PreserveExisting)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AtomicWritePermissions {
    Private,
    PreserveExisting,
}

fn atomic_write_with_permissions(
    path: &Path,
    bytes: &[u8],
    permission_mode: AtomicWritePermissions,
) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "state file path has no parent")
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "state file path has no file name",
        )
    })?;

    let mut temp_path = None;
    let mut temp_file = None;
    for _ in 0..16 {
        let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(".ceiling-tmp-{}-{sequence}", std::process::id()));
        let candidate = parent.join(temp_name);

        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => {
                temp_path = Some(candidate);
                temp_file = Some(file);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    let temp_path = temp_path.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique state-file temp path",
        )
    })?;
    let mut temp_file = temp_file.expect("temp path and file are assigned together");

    let existing_permissions = if permission_mode == AtomicWritePermissions::PreserveExisting {
        std::fs::metadata(path)
            .ok()
            .map(|metadata| metadata.permissions())
    } else {
        None
    };

    let result = (|| {
        prepare_temp_permissions(&temp_path, permission_mode, existing_permissions.as_ref())?;
        #[cfg(unix)]
        if permission_mode == AtomicWritePermissions::PreserveExisting {
            copy_unix_owner(path, &temp_path)?;
        }
        temp_file.write_all(bytes)?;
        temp_file.sync_all()?;
        drop(temp_file);
        atomic_replace(&temp_path, path, permission_mode)?;
        sync_parent_directory(parent)?;
        Ok(())
    })();

    if result.is_err() && !keep_replacement_temp(&result, path) {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// `ERROR_UNABLE_TO_MOVE_REPLACEMENT` (1176). `ReplaceFileW` has already
/// removed the destination; the new bytes exist only at the replacement path.
#[cfg(any(windows, test))]
const WIN32_ERROR_UNABLE_TO_MOVE_REPLACEMENT: u32 = 0x498;
#[cfg(test)]
const WIN32_ERROR_UNABLE_TO_MOVE_REPLACEMENT_2: u32 = 0x499;
#[cfg(any(windows, test))]
const WIN32_ERROR_ACCESS_DENIED: u32 = 5;
#[cfg(any(windows, test))]
const WIN32_ERROR_SHARING_VIOLATION: u32 = 32;
#[cfg(any(windows, test))]
const WIN32_ERROR_LOCK_VIOLATION: u32 = 33;

/// Whether the temp file is the only remaining copy of the new bytes.
///
/// `ERROR_UNABLE_TO_MOVE_REPLACEMENT` means the destination is already gone,
/// so deleting the temp would destroy the credential file. Trust that error
/// code even if a later `exists()` check races. Any other failure that leaves
/// no destination also keeps the temp.
#[cfg(any(windows, test))]
fn keep_temp_after_replace_failure(win32_code: u32, destination_still_exists: bool) -> bool {
    win32_code == WIN32_ERROR_UNABLE_TO_MOVE_REPLACEMENT || !destination_still_exists
}

#[cfg(any(windows, test))]
fn is_sharing_or_access_failure(win32_code: u32) -> bool {
    matches!(
        win32_code,
        WIN32_ERROR_ACCESS_DENIED | WIN32_ERROR_SHARING_VIOLATION | WIN32_ERROR_LOCK_VIOLATION
    )
}

fn keep_replacement_temp(result: &io::Result<()>, dest: &Path) -> bool {
    let Err(_error) = result else {
        return false;
    };
    #[cfg(windows)]
    {
        if let Some(raw) = _error.raw_os_error() {
            return keep_temp_after_replace_failure(raw as u32, dest.exists());
        }
        !dest.exists()
    }
    #[cfg(not(windows))]
    {
        let _ = dest;
        false
    }
}

#[cfg(windows)]
fn win32_error_code(error: &windows::core::Error) -> u32 {
    (error.code().0 as u32) & 0xffff
}

/// Build an `io::Error` that still reports the Win32 code via `raw_os_error()`.
///
/// `Error::new` drops that code, which would make `keep_replacement_temp` miss
/// `ERROR_UNABLE_TO_MOVE_REPLACEMENT` if the dest name is recreated.
#[cfg(any(windows, test))]
fn io_error_from_win32_code(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

#[cfg(windows)]
fn win32_io_error(_context: &str, error: windows::core::Error) -> io::Error {
    io_error_from_win32_code(win32_error_code(&error))
}

#[cfg(windows)]
struct CapturedFileSecurity {
    descriptor: Vec<usize>,
    control: u16,
}

#[cfg(windows)]
fn capture_file_security(path: &Path) -> io::Result<CapturedFileSecurity> {
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, GROUP_SECURITY_INFORMATION, GetFileSecurityW,
        GetSecurityDescriptorControl, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };
    use windows::core::PCWSTR;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let requested =
        OWNER_SECURITY_INFORMATION.0 | GROUP_SECURITY_INFORMATION.0 | DACL_SECURITY_INFORMATION.0;
    let mut needed = 0u32;
    unsafe {
        let _ = GetFileSecurityW(
            PCWSTR(wide.as_ptr()),
            requested,
            PSECURITY_DESCRIPTOR(std::ptr::null_mut()),
            0,
            &mut needed,
        );
    }
    if needed == 0 {
        return Err(io::Error::other(
            "GetFileSecurityW returned an empty descriptor",
        ));
    }

    let words = (needed as usize).div_ceil(size_of::<usize>());
    let mut descriptor = vec![0usize; words];
    unsafe {
        GetFileSecurityW(
            PCWSTR(wide.as_ptr()),
            requested,
            PSECURITY_DESCRIPTOR(descriptor.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
        .ok()
        .map_err(|error| io::Error::other(format!("GetFileSecurityW failed: {error}")))?;

        let mut control = 0u16;
        let mut revision = 0u32;
        GetSecurityDescriptorControl(
            PSECURITY_DESCRIPTOR(descriptor.as_mut_ptr().cast()),
            &mut control,
            &mut revision,
        )
        .map_err(|error| {
            io::Error::other(format!("GetSecurityDescriptorControl failed: {error}"))
        })?;
        Ok(CapturedFileSecurity {
            descriptor,
            control,
        })
    }
}

#[cfg(windows)]
fn captured_descriptor(
    captured: &CapturedFileSecurity,
) -> windows::Win32::Security::PSECURITY_DESCRIPTOR {
    windows::Win32::Security::PSECURITY_DESCRIPTOR(captured.descriptor.as_ptr() as *mut _)
}

#[cfg(windows)]
fn dacl_security_info(
    captured: &CapturedFileSecurity,
) -> windows::Win32::Security::OBJECT_SECURITY_INFORMATION {
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, OBJECT_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, SE_DACL_PROTECTED,
        UNPROTECTED_DACL_SECURITY_INFORMATION,
    };

    let mut info = DACL_SECURITY_INFORMATION.0;
    if captured.control & SE_DACL_PROTECTED.0 != 0 {
        info |= PROTECTED_DACL_SECURITY_INFORMATION.0;
    } else {
        info |= UNPROTECTED_DACL_SECURITY_INFORMATION.0;
    }
    OBJECT_SECURITY_INFORMATION(info)
}

#[cfg(windows)]
fn apply_captured_dacl(path: &Path, captured: &CapturedFileSecurity) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Foundation::BOOL;
    use windows::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
    use windows::Win32::Security::{ACL, GetSecurityDescriptorDacl, PSID};
    use windows::core::PCWSTR;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let descriptor = captured_descriptor(captured);
    let mut present = BOOL::default();
    let mut defaulted = BOOL::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    unsafe {
        GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted).map_err(
            |error| io::Error::other(format!("GetSecurityDescriptorDacl failed: {error}")),
        )?;
        if !present.as_bool() {
            return Ok(());
        }
        let status = SetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            dacl_security_info(captured),
            PSID::default(),
            PSID::default(),
            (!dacl.is_null()).then_some(dacl.cast_const()),
            None,
        );
        if status.is_err() {
            return Err(io::Error::other(format!(
                "SetNamedSecurityInfoW DACL failed: {status:?}"
            )));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn apply_captured_owner(path: &Path, captured: &CapturedFileSecurity) -> io::Result<()> {
    use std::mem::ManuallyDrop;
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Foundation::BOOL;
    use windows::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
    use windows::Win32::Security::{
        GetSecurityDescriptorOwner, OBJECT_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSID,
    };
    use windows::core::PCWSTR;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let descriptor = captured_descriptor(captured);
    let mut owner = ManuallyDrop::new(PSID::default());
    let mut defaulted = BOOL::default();
    unsafe {
        GetSecurityDescriptorOwner(descriptor, &mut *owner, &mut defaulted).map_err(|error| {
            io::Error::other(format!("GetSecurityDescriptorOwner failed: {error}"))
        })?;
        if owner.is_invalid() {
            return Ok(());
        }
        let status = SetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            OBJECT_SECURITY_INFORMATION(OWNER_SECURITY_INFORMATION.0),
            *owner,
            PSID::default(),
            None,
            None,
        );
        if status.is_err() {
            return Err(io::Error::other(format!(
                "SetNamedSecurityInfoW owner failed: {status:?}"
            )));
        }
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn finish_preserved_security_restore(
    dacl: io::Result<()>,
    owner: io::Result<()>,
) -> io::Result<()> {
    // WRITE_OWNER / SE_RESTORE is not guaranteed. Keep the replaced bytes
    // and the restored DACL if only the owner rewrite is denied.
    let _ = owner;
    dacl
}

#[cfg(windows)]
fn apply_file_security(path: &Path, captured: &CapturedFileSecurity) -> io::Result<()> {
    finish_preserved_security_restore(
        apply_captured_dacl(path, captured),
        apply_captured_owner(path, captured),
    )
}

#[cfg(windows)]
fn restore_captured_security(
    path: &Path,
    captured: Option<&CapturedFileSecurity>,
) -> io::Result<()> {
    match captured {
        Some(security) => apply_file_security(path, security),
        None => Ok(()),
    }
}

#[cfg(windows)]
fn move_file_replace(from: &Path, to: &Path) -> Result<(), windows::core::Error> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let wide_from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let wide_to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        MoveFileExW(
            PCWSTR(wide_from.as_ptr()),
            PCWSTR(wide_to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
}

#[cfg(windows)]
fn retry_move_file_replace(from: &Path, to: &Path) -> Result<(), windows::core::Error> {
    const ATTEMPTS: u32 = 5;
    const RETRY: std::time::Duration = std::time::Duration::from_millis(20);
    let mut last = None;
    for attempt in 0..ATTEMPTS {
        match move_file_replace(from, to) {
            Ok(()) => return Ok(()),
            Err(error)
                if is_sharing_or_access_failure(win32_error_code(&error))
                    && attempt + 1 < ATTEMPTS =>
            {
                last = Some(error);
                std::thread::sleep(RETRY);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last.expect("sharing retry records the last error"))
}

#[cfg(windows)]
fn sharing_replace_error(error: windows::core::Error) -> io::Error {
    let os = io_error_from_win32_code(win32_error_code(&error));
    io::Error::new(
        os.kind(),
        format!(
            "could not replace the file because another process has it open without delete sharing: {os}"
        ),
    )
}

#[cfg(windows)]
fn atomic_replace(
    from: &Path,
    to: &Path,
    permission_mode: AtomicWritePermissions,
) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{REPLACE_FILE_FLAGS, ReplaceFileW};
    use windows::core::PCWSTR;

    let destination_exists = to.exists();
    if permission_mode != AtomicWritePermissions::PreserveExisting || !destination_exists {
        return move_file_replace(from, to)
            .map_err(|error| win32_io_error("atomic file replacement failed", error));
    }

    let captured = capture_file_security(to).ok();
    let wide_from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let wide_to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let replace_result = unsafe {
        ReplaceFileW(
            PCWSTR(wide_to.as_ptr()),
            PCWSTR(wide_from.as_ptr()),
            PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
    };
    match replace_result {
        Ok(()) => restore_captured_security(to, captured.as_ref()),
        Err(error) if win32_error_code(&error) == WIN32_ERROR_UNABLE_TO_MOVE_REPLACEMENT => {
            // Destination is already gone; new bytes live only at `from`.
            match retry_move_file_replace(from, to) {
                Ok(()) => restore_captured_security(to, captured.as_ref()),
                Err(_move_error) => {
                    // Keep the original 0x498 so cleanup will not delete the
                    // temp even if something recreates the dest name, and log
                    // the temp path so the remaining copy of the tokens stays
                    // recoverable.
                    let code = win32_error_code(&error);
                    tracing::warn!(
                        temp_path = %from.display(),
                        win32_code = code,
                        "metadata-preserving replacement removed the destination and the recovery move failed"
                    );
                    Err(io_error_from_win32_code(code))
                }
            }
        }
        Err(error) if is_sharing_or_access_failure(win32_error_code(&error)) => {
            match retry_move_file_replace(from, to) {
                Ok(()) => restore_captured_security(to, captured.as_ref()),
                Err(move_error) => Err(sharing_replace_error(move_error)),
            }
        }
        Err(error) => Err(win32_io_error(
            "metadata-preserving file replacement failed",
            error,
        )),
    }
}

#[cfg(not(windows))]
fn atomic_replace(
    from: &Path,
    to: &Path,
    _permission_mode: AtomicWritePermissions,
) -> io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(windows)]
fn prepare_temp_permissions(
    path: &Path,
    _permission_mode: AtomicWritePermissions,
    _existing_permissions: Option<&std::fs::Permissions>,
) -> io::Result<()> {
    // Keep the temporary secret current-user-only until the swap. Destination
    // owner/DACL are copied back onto the replaced file afterwards.
    restrict_file_permissions(path)
}

#[cfg(not(windows))]
fn prepare_temp_permissions(
    path: &Path,
    permission_mode: AtomicWritePermissions,
    existing_permissions: Option<&std::fs::Permissions>,
) -> io::Result<()> {
    if permission_mode == AtomicWritePermissions::PreserveExisting
        && let Some(permissions) = existing_permissions
    {
        return std::fs::set_permissions(path, permissions.clone());
    }
    restrict_file_permissions(path)
}

#[cfg(unix)]
fn copy_unix_owner(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, chown};

    let metadata = match std::fs::metadata(from) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    chown(to, Some(metadata.uid()), Some(metadata.gid()))
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn protected_file_bytes(contents: &str) -> io::Result<Vec<u8>> {
    let (protection, encrypted) = protect(contents.as_bytes())?;
    let file = ProtectedFile {
        format: FORMAT.to_string(),
        version: VERSION,
        protection: protection.to_string(),
        payload: base64::engine::general_purpose::STANDARD.encode(encrypted),
    };
    serde_json::to_vec_pretty(&file).map_err(io::Error::other)
}

#[cfg(not(windows))]
fn protected_file_bytes(contents: &str) -> io::Result<Vec<u8>> {
    Ok(contents.as_bytes().to_vec())
}

#[cfg(windows)]
fn protect(plain: &[u8]) -> io::Result<(&'static str, Vec<u8>)> {
    use windows::Win32::Security::Cryptography::CRYPTPROTECT_UI_FORBIDDEN;

    protect_with_flags(plain, CRYPTPROTECT_UI_FORBIDDEN)
        .map(|encrypted| (WINDOWS_DPAPI_USER, encrypted))
        .map_err(|error| io::Error::other(format!("user-scoped DPAPI protection failed: {error}")))
}

#[cfg(windows)]
fn protect_with_flags(plain: &[u8], flags: u32) -> io::Result<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData};

    unsafe {
        let input_blob = CRYPT_INTEGER_BLOB {
            cbData: plain.len() as u32,
            pbData: plain.as_ptr() as *mut u8,
        };
        let mut output_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        CryptProtectData(&input_blob, None, None, None, None, flags, &mut output_blob)
            .map_err(|e| io::Error::other(format!("CryptProtectData failed: {e:?}")))?;

        if output_blob.pbData.is_null() {
            return Err(io::Error::other("CryptProtectData returned null output"));
        }

        let encrypted =
            std::slice::from_raw_parts(output_blob.pbData, output_blob.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output_blob.pbData as *mut _));
        Ok(encrypted)
    }
}

#[cfg(windows)]
fn unprotect(encrypted: &[u8]) -> io::Result<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
    };

    unsafe {
        let input_blob = CRYPT_INTEGER_BLOB {
            cbData: encrypted.len() as u32,
            pbData: encrypted.as_ptr() as *mut u8,
        };
        let mut output_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };

        CryptUnprotectData(
            &input_blob,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output_blob,
        )
        .map_err(|e| io::Error::other(format!("CryptUnprotectData failed: {e:?}")))?;

        if output_blob.pbData.is_null() {
            return Err(io::Error::other("CryptUnprotectData returned null output"));
        }

        let plain =
            std::slice::from_raw_parts(output_blob.pbData, output_blob.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output_blob.pbData as *mut _));
        Ok(plain)
    }
}

#[cfg(not(windows))]
fn unprotect(_encrypted: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows DPAPI-protected files can only be read on Windows by the same user",
    ))
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(windows)]
fn restrict_file_permissions(path: &Path) -> io::Result<()> {
    crate::windows_security::restrict_path_to_current_user(path)
}

#[cfg(not(any(unix, windows)))]
fn restrict_file_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_plaintext_json_without_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.json");
        std::fs::write(&path, r#"{"hello":"world"}"#).unwrap();

        assert_eq!(read_string(&path).unwrap(), r#"{"hello":"world"}"#);
    }

    #[test]
    fn write_roundtrips_on_this_platform() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secure.json");
        write_string(&path, r#"{"secret":"value"}"#).unwrap();

        assert_eq!(read_string(&path).unwrap(), r#"{"secret":"value"}"#);
    }

    #[test]
    fn write_atomically_replaces_an_existing_file_without_temp_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secure.json");
        write_string(&path, r#"{"secret":"first"}"#).unwrap();
        write_string(&path, r#"{"secret":"second"}"#).unwrap();

        assert_eq!(read_string(&path).unwrap(), r#"{"secret":"second"}"#);
        let names = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![OsString::from("secure.json")]);
    }

    #[test]
    fn failed_metadata_preserving_replacement_keeps_destination_and_cleans_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::create_dir(&path).unwrap();

        assert!(atomic_write_preserving_permissions(&path, b"replacement").is_err());
        assert!(path.is_dir());
        let temp_artifacts = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("ceiling-tmp"))
            .count();
        assert_eq!(temp_artifacts, 0);
    }

    #[cfg(unix)]
    #[test]
    fn metadata_preserving_write_keeps_unix_file_mode() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, b"original").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let before = std::fs::metadata(&path).unwrap();
        chown(&path, Some(before.uid()), Some(before.gid())).unwrap();

        atomic_write_preserving_permissions(&path, b"replacement").unwrap();

        let after = std::fs::metadata(&path).unwrap();
        assert_eq!(after.permissions().mode() & 0o777, 0o640);
        assert_eq!(after.uid(), before.uid());
        assert_eq!(after.gid(), before.gid());
    }

    #[cfg(unix)]
    #[test]
    fn metadata_preserving_write_follows_a_symlinked_credential_path() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.json");
        std::fs::write(&target, b"original").unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        atomic_write_preserving_permissions(&link, b"replacement").unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"replacement");
    }

    #[test]
    fn keep_temp_when_replace_already_removed_destination() {
        assert!(keep_temp_after_replace_failure(
            WIN32_ERROR_UNABLE_TO_MOVE_REPLACEMENT,
            false
        ));
        assert!(keep_temp_after_replace_failure(
            WIN32_ERROR_UNABLE_TO_MOVE_REPLACEMENT,
            true
        ));
        assert!(!keep_temp_after_replace_failure(
            WIN32_ERROR_UNABLE_TO_MOVE_REPLACEMENT_2,
            true
        ));
        assert!(keep_temp_after_replace_failure(
            WIN32_ERROR_UNABLE_TO_MOVE_REPLACEMENT_2,
            false
        ));
        assert!(!keep_temp_after_replace_failure(
            WIN32_ERROR_SHARING_VIOLATION,
            true
        ));
        assert!(keep_temp_after_replace_failure(
            WIN32_ERROR_SHARING_VIOLATION,
            false
        ));
        assert!(is_sharing_or_access_failure(WIN32_ERROR_ACCESS_DENIED));
        assert!(is_sharing_or_access_failure(WIN32_ERROR_SHARING_VIOLATION));
        assert!(is_sharing_or_access_failure(WIN32_ERROR_LOCK_VIOLATION));
        assert!(!is_sharing_or_access_failure(
            WIN32_ERROR_UNABLE_TO_MOVE_REPLACEMENT
        ));
    }

    #[test]
    fn win32_replace_error_keeps_unable_to_move_replacement_os_code() {
        let error = io_error_from_win32_code(WIN32_ERROR_UNABLE_TO_MOVE_REPLACEMENT);
        assert_eq!(
            error.raw_os_error(),
            Some(WIN32_ERROR_UNABLE_TO_MOVE_REPLACEMENT as i32)
        );

        #[cfg(windows)]
        {
            let dir = tempfile::tempdir().unwrap();
            let dest = dir.path().join("credentials.json");
            std::fs::write(&dest, b"reappeared").unwrap();
            let result: io::Result<()> = Err(error);
            assert!(
                keep_replacement_temp(&result, &dest),
                "0x498 must keep the temp even if the dest name was recreated"
            );
        }
    }

    #[cfg(windows)]
    fn current_user_sid() -> (Vec<usize>, windows::Win32::Security::PSID) {
        use std::mem::size_of;

        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).unwrap();
            let mut token_bytes = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut token_bytes);
            assert!(token_bytes >= size_of::<TOKEN_USER>() as u32);
            let token_words = (token_bytes as usize).div_ceil(size_of::<usize>());
            let mut token_buffer = vec![0usize; token_words];
            GetTokenInformation(
                token,
                TokenUser,
                Some(token_buffer.as_mut_ptr().cast()),
                token_bytes,
                &mut token_bytes,
            )
            .unwrap();
            CloseHandle(token).unwrap();
            let token_user = &*token_buffer.as_ptr().cast::<TOKEN_USER>();
            let sid = token_user.User.Sid;
            (token_buffer, sid)
        }
    }

    #[cfg(windows)]
    fn dacl_has_noninherited_user_full_control(path: &Path) -> bool {
        use std::mem::size_of;
        use std::os::windows::ffi::OsStrExt;

        use windows::Win32::Foundation::BOOL;
        use windows::Win32::Security::{
            ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
            DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetFileSecurityW,
            GetSecurityDescriptorControl, GetSecurityDescriptorDacl, INHERITED_ACE,
            PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
        };
        use windows::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
        use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
        use windows::core::PCWSTR;

        let (_token_buffer, user_sid) = current_user_sid();
        let wide_path: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut descriptor_bytes = 0u32;
        unsafe {
            let _ = GetFileSecurityW(
                PCWSTR(wide_path.as_ptr()),
                DACL_SECURITY_INFORMATION.0,
                PSECURITY_DESCRIPTOR(std::ptr::null_mut()),
                0,
                &mut descriptor_bytes,
            );
        }
        assert!(descriptor_bytes > 0);

        let descriptor_words = (descriptor_bytes as usize).div_ceil(size_of::<usize>());
        let mut descriptor_buffer = vec![0usize; descriptor_words];
        let descriptor = PSECURITY_DESCRIPTOR(descriptor_buffer.as_mut_ptr().cast());
        unsafe {
            GetFileSecurityW(
                PCWSTR(wide_path.as_ptr()),
                DACL_SECURITY_INFORMATION.0,
                descriptor,
                descriptor_bytes,
                &mut descriptor_bytes,
            )
            .ok()
            .unwrap();

            let mut control = 0u16;
            let mut revision = 0u32;
            GetSecurityDescriptorControl(descriptor, &mut control, &mut revision).unwrap();
            if control & SE_DACL_PROTECTED.0 == 0 {
                return false;
            }

            let mut present = BOOL::default();
            let mut defaulted = BOOL::default();
            let mut dacl: *mut ACL = std::ptr::null_mut();
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted).unwrap();
            if !present.as_bool() || dacl.is_null() {
                return false;
            }

            let mut info = ACL_SIZE_INFORMATION::default();
            GetAclInformation(
                dacl,
                (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
            .unwrap();

            for index in 0..info.AceCount {
                let mut ace_ptr = std::ptr::null_mut();
                GetAce(dacl, index, &mut ace_ptr).unwrap();
                let ace = &*ace_ptr.cast::<ACCESS_ALLOWED_ACE>();
                if u32::from(ace.Header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
                    continue;
                }
                if u32::from(ace.Header.AceFlags) & INHERITED_ACE.0 != 0 {
                    continue;
                }
                if ace.Mask != FILE_ALL_ACCESS.0 {
                    continue;
                }
                let ace_sid = PSID((&ace.SidStart as *const u32).cast_mut().cast());
                if EqualSid(ace_sid, user_sid).is_ok() {
                    return true;
                }
            }
            false
        }
    }

    #[cfg(windows)]
    #[test]
    fn metadata_preserving_write_keeps_windows_dacl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, b"original").unwrap();
        crate::windows_security::restrict_path_to_current_user(&path).unwrap();
        assert!(dacl_has_noninherited_user_full_control(&path));

        atomic_write_preserving_permissions(&path, b"replacement").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        assert!(
            dacl_has_noninherited_user_full_control(&path),
            "destination DACL lost the explicit current-user ACE"
        );
    }

    #[test]
    fn metadata_preserving_write_keeps_dacl_when_owner_cannot_be_rewritten() {
        let dacl_ok: io::Result<()> = Ok(());
        let owner_denied = Err(io::Error::from_raw_os_error(
            WIN32_ERROR_ACCESS_DENIED as i32,
        ));
        finish_preserved_security_restore(dacl_ok, owner_denied)
            .expect("owner restore denial must not fail a successful DACL restore");

        let dacl_err = Err(io::Error::other("SetNamedSecurityInfoW DACL failed"));
        assert!(
            finish_preserved_security_restore(dacl_err, Ok(())).is_err(),
            "a failed DACL restore must still fail the persist"
        );
    }

    #[cfg(windows)]
    #[test]
    fn metadata_preserving_write_errors_when_destination_has_no_delete_share() {
        use std::os::windows::ffi::OsStrExt;

        use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
        use windows::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };
        use windows::core::PCWSTR;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, b"original").unwrap();

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .expect("open dest without delete sharing");

        let error = atomic_write_preserving_permissions(&path, b"replacement").unwrap_err();
        unsafe {
            CloseHandle(handle).ok();
        }

        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        let message = error.to_string();
        assert!(
            message.contains("delete sharing"),
            "expected a clear sharing error, got {message}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_write_uses_user_dpapi_and_a_protected_single_entry_dacl() {
        use std::mem::size_of;
        use std::os::windows::ffi::OsStrExt;

        use windows::Win32::Foundation::{BOOL, CloseHandle, HANDLE};
        use windows::Win32::Security::{
            ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
            DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetFileSecurityW,
            GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetTokenInformation,
            PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER, TokenUser,
        };
        use windows::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
        use windows::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        use windows::core::PCWSTR;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secure.json");
        write_string(&path, r#"{"secret":"value"}"#).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let protected: ProtectedFile = serde_json::from_str(&raw).unwrap();
        assert_eq!(protected.protection, WINDOWS_DPAPI_USER);

        let wide_path: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut descriptor_bytes = 0u32;
        unsafe {
            let _ = GetFileSecurityW(
                PCWSTR(wide_path.as_ptr()),
                DACL_SECURITY_INFORMATION.0,
                PSECURITY_DESCRIPTOR(std::ptr::null_mut()),
                0,
                &mut descriptor_bytes,
            );
        }
        assert!(descriptor_bytes > 0);

        let descriptor_words = (descriptor_bytes as usize).div_ceil(size_of::<usize>());
        let mut descriptor_buffer = vec![0usize; descriptor_words];
        let descriptor = PSECURITY_DESCRIPTOR(descriptor_buffer.as_mut_ptr().cast());

        unsafe {
            GetFileSecurityW(
                PCWSTR(wide_path.as_ptr()),
                DACL_SECURITY_INFORMATION.0,
                descriptor,
                descriptor_bytes,
                &mut descriptor_bytes,
            )
            .ok()
            .unwrap();

            let mut control = 0u16;
            let mut revision = 0u32;
            GetSecurityDescriptorControl(descriptor, &mut control, &mut revision).unwrap();
            assert_ne!(control & SE_DACL_PROTECTED.0, 0);

            let mut present = BOOL::default();
            let mut defaulted = BOOL::default();
            let mut dacl: *mut ACL = std::ptr::null_mut();
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted).unwrap();
            assert!(present.as_bool());
            assert!(!dacl.is_null());
            assert!(!defaulted.as_bool());

            let mut info = ACL_SIZE_INFORMATION::default();
            GetAclInformation(
                dacl,
                (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
            .unwrap();
            assert_eq!(info.AceCount, 1);

            let mut ace_ptr = std::ptr::null_mut();
            GetAce(dacl, 0, &mut ace_ptr).unwrap();
            let ace = &*ace_ptr.cast::<ACCESS_ALLOWED_ACE>();
            assert_eq!(u32::from(ace.Header.AceType), ACCESS_ALLOWED_ACE_TYPE);
            assert_eq!(ace.Mask, FILE_ALL_ACCESS.0);

            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).unwrap();
            let mut token_bytes = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut token_bytes);
            assert!(token_bytes >= size_of::<TOKEN_USER>() as u32);
            let token_words = (token_bytes as usize).div_ceil(size_of::<usize>());
            let mut token_buffer = vec![0usize; token_words];
            GetTokenInformation(
                token,
                TokenUser,
                Some(token_buffer.as_mut_ptr().cast()),
                token_bytes,
                &mut token_bytes,
            )
            .unwrap();
            CloseHandle(token).unwrap();

            let token_user = &*token_buffer.as_ptr().cast::<TOKEN_USER>();
            let ace_sid = PSID((&ace.SidStart as *const u32).cast_mut().cast());
            EqualSid(ace_sid, token_user.User.Sid).unwrap();
        }
    }

    #[test]
    fn status_reports_missing_plaintext_and_protected_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        assert_eq!(status(&missing), SecureFileStatus::Missing);

        let plain = dir.path().join("plain.json");
        std::fs::write(&plain, r#"{"secret":"value"}"#).unwrap();
        assert_eq!(status(&plain), SecureFileStatus::Plaintext);

        let protected = dir.path().join("protected.json");
        std::fs::write(
            &protected,
            serde_json::to_string(&ProtectedFile {
                format: FORMAT.to_string(),
                version: VERSION,
                protection: WINDOWS_DPAPI_USER.to_string(),
                payload: "AA==".to_string(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            status(&protected),
            SecureFileStatus::Protected(WINDOWS_DPAPI_USER.to_string())
        );
    }

    #[test]
    fn status_reports_unsupported_wrappers_as_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("protected.json");
        std::fs::write(
            &path,
            serde_json::to_string(&ProtectedFile {
                format: FORMAT.to_string(),
                version: VERSION + 1,
                protection: WINDOWS_DPAPI_USER.to_string(),
                payload: "AA==".to_string(),
            })
            .unwrap(),
        )
        .unwrap();

        assert!(matches!(status(&path), SecureFileStatus::Unreadable(_)));
    }

    #[cfg(windows)]
    #[test]
    fn windows_write_uses_protected_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secure.json");
        write_string(&path, r#"{"secret":"value"}"#).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let file: ProtectedFile = serde_json::from_str(&raw).unwrap();

        assert_eq!(file.format, FORMAT);
        assert_eq!(file.version, VERSION);
        assert!(matches!(
            file.protection.as_str(),
            WINDOWS_DPAPI_USER | WINDOWS_DPAPI_MACHINE
        ));
        assert!(
            !raw.contains("secret") && !raw.contains("value"),
            "protected Windows file must not contain plaintext JSON"
        );
    }

    fn make_stale(path: &Path) {
        set_age(path, STATE_LOCK_STALE + std::time::Duration::from_secs(1));
    }

    fn set_age(path: &Path, age: std::time::Duration) {
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(std::time::SystemTime::now() - age)
            .unwrap();
    }

    #[cfg(unix)]
    fn take_lock(path: &Path) -> StateWriteLock {
        match StateWriteLock::try_acquire(path) {
            LockAttempt::Acquired(lock) => lock,
            LockAttempt::Contended => panic!("the lock must be free (contended)"),
            LockAttempt::FlockUnsupported(error) => {
                panic!("the lock must be free (flock unsupported: {error})")
            }
            LockAttempt::Unopenable(error) => panic!("the lock must be free (unopenable: {error})"),
            LockAttempt::Failed(error) => panic!("the lock must be free (failed: {error})"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn state_write_lock_try_acquire_reports_contention_while_held() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state-write.lock");

        let held = take_lock(&path);
        assert!(
            matches!(StateWriteLock::try_acquire(&path), LockAttempt::Contended),
            "second acquire must report contention while the first holder is alive"
        );
        assert!(path.exists(), "lock file stays while a holder is alive");

        drop(held);
        assert!(
            matches!(StateWriteLock::try_acquire(&path), LockAttempt::Acquired(_)),
            "a dropped holder must release the flock for the next acquirer"
        );
    }

    #[cfg(unix)]
    #[test]
    fn state_write_lock_recovers_stale_lock_file_after_crash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state-write.lock");
        std::fs::write(&path, b"leftover after SIGKILL").unwrap();

        let started = std::time::Instant::now();
        let mut ran = false;
        with_state_write_lock_at(&path, || {
            ran = true;
            Ok(())
        })
        .unwrap();

        assert!(ran);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "a leftover lock file with no live holder must not wait out the acquire timeout"
        );
    }

    #[cfg(unix)]
    #[test]
    fn state_write_lock_retries_until_the_live_holder_releases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state-write.lock");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();

        let held = take_lock(&path);
        let waiter_path = path.clone();
        std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let result = with_state_write_lock_at(&waiter_path, || Ok(42));
            let _ = done_tx.send(result);
        });
        ready_rx.recv().unwrap();

        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(80))
                .is_err(),
            "a live holder must not have its lock stolen"
        );

        drop(held);
        let result = done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("waiter should acquire after the holder drops");
        assert_eq!(result.unwrap(), 42);
    }

    /// Pins SBS-947: an unenforceable report is no longer a success path that
    /// writes with no cross-process exclusion.
    #[test]
    fn an_unenforceable_lock_fails_closed_instead_of_writing_unserialized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state-write.lock");
        // Make exclusive-create fail too: put a directory on the sibling path.
        std::fs::create_dir(exclusive_lock_path(&path)).unwrap();

        let mut attempts = 0;
        let started = std::time::Instant::now();
        let result = StateWriteLock::acquire_with(&path, |_| {
            attempts += 1;
            LockAttempt::FlockUnsupported(io::Error::from(io::ErrorKind::Unsupported))
        });

        match result {
            Ok(_) => panic!("flock-unsupported plus a broken exclusive-create must fail closed"),
            Err(error) => {
                let message = error.to_string();
                assert!(
                    message.contains("exclusive-create failed")
                        || message.contains("could not create"),
                    "the error must name the failed fallback, got {message}"
                );
            }
        }
        assert_eq!(
            attempts, 1,
            "a failed fallback must not be retried as success"
        );
        // A directory on the sibling path is access-denied on Windows too, but
        // it never clears. The delete-pending retry must not sit on it: this
        // used to spend the whole grace period before failing.
        assert!(
            started.elapsed() < DELETE_PENDING_GRACE,
            "an unfixable fallback must fail without waiting out the retry grace"
        );
    }

    #[test]
    fn flock_unsupported_serializes_through_exclusive_create() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state-write.lock");

        let lock = StateWriteLock::acquire_with(&path, |_| {
            LockAttempt::FlockUnsupported(io::Error::from(io::ErrorKind::Unsupported))
        })
        .expect("flock-unsupported must fall back to exclusive-create, not skip the lock");

        assert!(
            exclusive_lock_path(&path).exists(),
            "the exclusive-create sibling must exist while held"
        );

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let waiter_path = path.clone();
        std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let result = StateWriteLock::acquire_with(&waiter_path, |_| {
                LockAttempt::FlockUnsupported(io::Error::from(io::ErrorKind::Unsupported))
            });
            let _ = done_tx.send(result.map(|_| ()));
        });
        ready_rx.recv().unwrap();
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(80))
                .is_err(),
            "exclusive-create must serialize a second flock-unsupported writer"
        );

        drop(lock);
        let result = done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("waiter should acquire after the exclusive-create holder drops");
        result.unwrap();
        assert!(
            !exclusive_lock_path(&path).exists(),
            "dropping the exclusive-create holder must unlink the sibling"
        );
    }

    /// Regression for a Windows-only flake in CI: the waiter's `create_new`
    /// raced the holder's unlink and got `ERROR_ACCESS_DENIED` rather than
    /// `ERROR_FILE_EXISTS`, because the name was mid-delete. That surfaced as
    /// a hard "Access is denied. (os error 5)" instead of one more retry, so a
    /// release could hand the next writer a failure rather than the lock.
    ///
    /// Runs the handoff repeatedly to widen the window.
    #[test]
    fn releasing_the_sibling_hands_the_lock_to_the_next_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state-write.lock");
        let unsupported =
            |_: &Path| LockAttempt::FlockUnsupported(io::Error::from(io::ErrorKind::Unsupported));

        for round in 0..25 {
            let held = StateWriteLock::acquire_with(&path, unsupported)
                .unwrap_or_else(|error| panic!("round {round} could not take the lock: {error}"));

            let waiter_path = path.clone();
            let waiter = std::thread::spawn(move || {
                StateWriteLock::acquire_with(&waiter_path, unsupported).map(|_| ())
            });

            // Release into the waiter's retry loop rather than before it.
            drop(held);
            waiter
                .join()
                .expect("waiter thread")
                .unwrap_or_else(|error| {
                    panic!("round {round} waiter did not get the released lock: {error}")
                });
            assert!(
                !exclusive_lock_path(&path).exists(),
                "round {round} left the sibling behind"
            );
        }
    }

    #[test]
    fn exclusive_create_recovers_a_stale_sibling_after_crash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state-write.lock");
        let exclusive = exclusive_lock_path(&path);
        std::fs::write(&exclusive, b"leftover after SIGKILL").unwrap();
        make_stale(&exclusive);

        let started = std::time::Instant::now();
        let lock = StateWriteLock::acquire_with(&path, |_| {
            LockAttempt::FlockUnsupported(io::Error::from(io::ErrorKind::Unsupported))
        })
        .expect("a stale exclusive-create leftover must be taken, not waited out");
        drop(lock);

        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "a stale exclusive-create leftover must not wait out the acquire timeout"
        );
        assert!(
            !exclusive.exists(),
            "the recovered holder must unlink the sibling on drop"
        );
    }

    #[test]
    fn a_contended_lock_is_retried_and_a_failed_attempt_is_reported() {
        let mut attempts = 0;
        StateWriteLock::acquire_with(Path::new("state-write.lock"), |_| {
            attempts += 1;
            if attempts < 3 {
                LockAttempt::Contended
            } else {
                LockAttempt::Acquired(StateWriteLock::empty())
            }
        })
        .expect("contention must be retried until the holder releases");
        assert_eq!(attempts, 3);

        let failure = StateWriteLock::acquire_with(Path::new("state-write.lock"), |_| {
            LockAttempt::Failed(io::Error::from(io::ErrorKind::NotFound))
        });
        match failure {
            Ok(_) => panic!("an unrelated failure must reach the caller"),
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::NotFound),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_filesystem_without_flock_support_is_classified_for_exclusive_create() {
        assert!(
            matches!(
                classify_lock_failure(std::fs::TryLockError::WouldBlock),
                LockAttempt::Contended
            ),
            "a live holder must still be waited for"
        );
        assert!(
            matches!(
                classify_lock_failure(std::fs::TryLockError::Error(io::Error::from(
                    io::ErrorKind::Interrupted
                ))),
                LockAttempt::Contended
            ),
            "a signal must be retried, not treated as a broken filesystem"
        );

        match classify_lock_failure(std::fs::TryLockError::Error(io::Error::from(
            io::ErrorKind::Unsupported,
        ))) {
            LockAttempt::FlockUnsupported(error) => assert!(
                error.to_string().contains("cannot lock"),
                "the flock-unsupported path must name the reason, got {error}"
            ),
            _ => panic!("ENOTSUP must be flock-unsupported, not a silent degrade or a hard fail"),
        }

        // On macOS and the BSDs this errno is distinct from EOPNOTSUPP and
        // older toolchains decoded it as Uncategorized, so the classification
        // must not rest on `kind()` alone.
        match classify_lock_failure(std::fs::TryLockError::Error(io::Error::from_raw_os_error(
            libc::ENOTSUP,
        ))) {
            LockAttempt::FlockUnsupported(_) => {}
            _ => panic!("ENOTSUP must be flock-unsupported whatever std decodes it as"),
        }

        match classify_lock_failure(std::fs::TryLockError::Error(io::Error::from_raw_os_error(
            libc::ENOLCK,
        ))) {
            LockAttempt::Failed(error) => assert!(
                error.to_string().contains("could not lock"),
                "ENOLCK must fail the write, got {error}"
            ),
            LockAttempt::FlockUnsupported(_) => panic!(
                "ENOLCK also means the lock table is full or lockd failed for this call, while \
                 another process still holds a real flock. Falling back to the sibling would let \
                 both writers run."
            ),
            _ => panic!("ENOLCK must fail closed"),
        }
    }

    /// Unknown flock errnos are not "the filesystem cannot lock".
    #[cfg(unix)]
    #[test]
    fn an_unknown_flock_error_fails_the_write() {
        match classify_lock_failure(std::fs::TryLockError::Error(io::Error::from(
            io::ErrorKind::InvalidInput,
        ))) {
            LockAttempt::Failed(error) => assert!(
                error.to_string().contains("could not lock"),
                "unknown must stay unknown, got {error}"
            ),
            LockAttempt::FlockUnsupported(_) => {
                panic!("an unknown errno must not be collapsed into flock-unsupported")
            }
            LockAttempt::Contended => panic!("an unknown errno is not contention"),
            LockAttempt::Unopenable(_) => panic!("an unknown flock errno is not an open failure"),
            LockAttempt::Acquired(_) => panic!("an unknown errno must not acquire"),
        }
    }

    #[test]
    fn a_lock_path_that_is_a_directory_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state-write.lock");
        std::fs::create_dir(&path).unwrap();

        let started = std::time::Instant::now();
        let mut ran = false;
        let error = with_state_write_lock_at(&path, || {
            ran = true;
            Ok(())
        })
        .expect_err("a directory lock path must fail the write, not skip the lock");

        assert!(!ran, "the write must not run without a lock");
        assert!(
            error.to_string().contains("directory"),
            "the error must name the directory, got {error}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "a directory lock path must not wait out the acquire timeout"
        );
    }

    /// Pins SBS-947: unlinking an unopenable lock file used to "repair" it, but
    /// a live holder still has that inode open, so the replacement put two
    /// writers on two different inodes.
    #[cfg(unix)]
    #[test]
    fn an_unopenable_lock_file_fails_closed_instead_of_being_unlinked() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state-write.lock");
        std::fs::write(&path, b"leftover from a privileged run").unwrap();
        make_stale(&path);
        let before = path_identity(&path).expect("the leftover must exist");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .is_ok()
        {
            // Running as root, where no file is unopenable.
            return;
        }

        let started = std::time::Instant::now();
        let mut ran = false;
        let error = with_state_write_lock_at(&path, || {
            ran = true;
            Ok(())
        })
        .expect_err("an unopenable lock file must fail the write, not be unlinked");

        assert!(!ran, "the write must not run without a lock");
        let message = error.to_string();
        assert!(
            message.contains(&path.display().to_string()),
            "the error must name the lock file so the user can remove it, got {message}"
        );
        assert_eq!(
            path_identity(&path),
            Some(before),
            "the lock file must not be unlinked and recreated under a live holder"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "an unopenable lock file must fail fast, not be masked by the acquire timeout"
        );
    }

    /// Pins SBS-947: a holder that is merely slow is not a crash leftover. The
    /// stale threshold is longer than the acquire timeout precisely so a waiter
    /// can never outlast a live holder and steal the sibling.
    #[test]
    fn an_exclusive_create_sibling_is_not_stolen_before_the_stale_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state-write.lock");
        let exclusive = exclusive_lock_path(&path);

        let held = StateWriteLock::acquire_with(&path, |_| {
            LockAttempt::FlockUnsupported(io::Error::from(io::ErrorKind::Unsupported))
        })
        .expect("the first holder must take the sibling");

        // Older than any waiter's acquire timeout, but still short of the
        // crash-recovery threshold.
        set_age(
            &exclusive,
            STATE_LOCK_TIMEOUT + std::time::Duration::from_secs(5),
        );

        assert!(
            matches!(
                StateWriteLock::try_exclusive_create(&path),
                LockAttempt::Contended
            ),
            "a holder older than the acquire timeout must still read as live"
        );
        assert!(
            exclusive.exists(),
            "a live holder's sibling must not be removed"
        );

        drop(held);
        assert!(
            matches!(
                StateWriteLock::try_exclusive_create(&path),
                LockAttempt::Acquired(_)
            ),
            "the sibling must be free once the holder drops"
        );
    }

    /// Pins SBS-947: after a stale takeover replaced the sibling, the original
    /// holder's drop must not delete the new holder's file.
    #[cfg(unix)]
    #[test]
    fn dropping_a_holder_does_not_unlink_a_sibling_it_did_not_create() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state-write.lock");
        let exclusive = exclusive_lock_path(&path);

        let first = StateWriteLock::acquire_with(&path, |_| {
            LockAttempt::FlockUnsupported(io::Error::from(io::ErrorKind::Unsupported))
        })
        .expect("the first holder must take the sibling");

        // Stand in for a takeover: the sibling at this path is now a different
        // file belonging to somebody else.
        std::fs::remove_file(&exclusive).unwrap();
        std::fs::write(&exclusive, b"a later holder's sibling").unwrap();
        let replacement = path_identity(&exclusive).unwrap();

        drop(first);

        assert_eq!(
            path_identity(&exclusive),
            Some(replacement),
            "the first holder must not unlink a sibling it did not create"
        );
    }

    /// Pins SBS-947: a lock file stamped in the future has no measurable age.
    /// Treating that as stale would hand a live holder's lock away on nothing
    /// worse than NFS clock skew.
    #[test]
    fn a_lock_file_stamped_in_the_future_is_not_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state-write.lock");
        std::fs::write(&path, b"held").unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
            .unwrap();

        assert!(
            matches!(lock_file_age(&path), LockFileAge::Fresh),
            "a future mtime must read as a live holder, not as a stale leftover"
        );
    }
}
