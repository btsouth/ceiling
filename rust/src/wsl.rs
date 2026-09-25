//! WSL (Windows Subsystem for Linux) detection and environment helpers
//!
//! Provides utilities to detect if CodexBar is running inside WSL,
//! and to resolve Windows filesystem paths from within the Linux environment.

use std::path::{Path, PathBuf};

/// Explicit override for the Windows profile used from WSL.
/// Accepts a username (`Alice`) or a profile path (`C:\Users\Alice`).
pub const WINDOWS_USERNAME_SETTING_ENV: &str = "CEILING_WINDOWS_USERNAME";

/// WSL distribution information
#[derive(Debug, Clone)]
pub struct WslInfo {
    pub distro_name: String,
    pub windows_username: Option<String>,
    pub drive_mount: PathBuf,
}

/// Detect if we are running inside WSL
pub fn is_wsl() -> bool {
    if let Ok(version) = std::fs::read_to_string("/proc/version") {
        let v = version.to_lowercase();
        if v.contains("microsoft") || v.contains("wsl") {
            return true;
        }
    }

    if std::env::var("WSL_DISTRO_NAME").is_ok() {
        return true;
    }

    if std::path::Path::new("/run/WSL").exists() {
        return true;
    }

    false
}

/// Get WSL environment information.
/// Returns None if not running inside WSL.
pub fn get_wsl_info() -> Option<WslInfo> {
    if !is_wsl() {
        return None;
    }

    let distro_name = std::env::var("WSL_DISTRO_NAME")
        .or_else(|_| {
            std::fs::read_to_string("/etc/os-release").map(|content| {
                content
                    .lines()
                    .find(|l| l.starts_with("NAME="))
                    .map(|l| l.trim_start_matches("NAME=").trim_matches('"').to_string())
                    .unwrap_or_else(|| "Unknown".to_string())
            })
        })
        .unwrap_or_else(|_| "Unknown".to_string());

    let drive_mount = drive_mount_for('c');
    let windows_username = resolve_windows_username(&drive_mount);

    Some(WslInfo {
        distro_name,
        windows_username,
        drive_mount,
    })
}

/// The mount point that serves Windows drive `letter`, honouring a custom
/// `[automount] root` in `/etc/wsl.conf`.
///
/// WSL defaults to mounting drives at `/mnt/<letter>`, but a distro can set
/// `root = /` (drives at `/c`) or any other prefix. Nothing else in this module
/// needs to know the root, because every Windows path is built from the
/// returned mount point.
fn drive_mount_for(letter: char) -> PathBuf {
    drive_mount_with_root(automount_root().as_deref(), letter)
}

/// Join a drive letter onto an explicit automount root, defaulting to `/mnt`.
/// Split from `drive_mount_for` so the join rule is testable without a real
/// `/etc/wsl.conf`.
fn drive_mount_with_root(root: Option<&Path>, letter: char) -> PathBuf {
    let letter = letter.to_ascii_lowercase().to_string();
    match root {
        Some(root) => root.join(letter),
        None => PathBuf::from(format!("/mnt/{letter}")),
    }
}

/// Parse `[automount] root` from `/etc/wsl.conf`, if it is set.
///
/// The value is a directory that replaces the `/mnt/` prefix; WSL writes `/mnt/`
/// itself when the setting is absent, so a missing key or file yields `None`.
/// An unreadable or malformed value also yields `None` so the default stands
/// rather than pointing every Windows probe at a nonexistent directory.
fn automount_root() -> Option<PathBuf> {
    let content = std::fs::read_to_string("/etc/wsl.conf").ok()?;
    parse_automount_root(&content)
}

/// Extract `[automount] root` from an `/etc/wsl.conf` body.
fn parse_automount_root(content: &str) -> Option<PathBuf> {
    let mut in_automount = false;
    for raw in content.lines() {
        let line = raw.split(['#', ';']).next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let section = line.trim_matches(['[', ']']).trim();
            in_automount = section.eq_ignore_ascii_case("automount");
            continue;
        }
        if !in_automount {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("root") {
            continue;
        }
        // WSL accepts an unquoted path; tolerate a quoted one defensively.
        let value = value.trim().trim_matches(['"', '\'']).trim();
        if value.is_empty() || !value.starts_with('/') {
            return None;
        }
        let mut root = PathBuf::from(value);
        // A `root = /` yields an empty prefix, so `/c` is the mount point.
        // Normalize `/mnt/` and `/` to the same shape so `join` does not
        // produce `/mnt//c`.
        if let Some(stripped) = value.strip_suffix('/')
            && !stripped.is_empty()
        {
            root = PathBuf::from(stripped);
        }
        return Some(root);
    }
    None
}

/// Resolve the Windows username by looking at `<drive_mount>/Users`.
///
/// Fail closed: never pick an arbitrary profile from readdir order. Require an
/// explicit match from `CEILING_WINDOWS_USERNAME`, `USERPROFILE`, or `USER`.
fn resolve_windows_username(drive_mount: &Path) -> Option<String> {
    resolve_windows_username_from(
        drive_mount,
        std::env::var(WINDOWS_USERNAME_SETTING_ENV).ok(),
        std::env::var("USERPROFILE").ok(),
        std::env::var("USER").ok(),
    )
}

/// Pick the Windows profile directory name for this session.
///
/// Every source has to name a directory that sits directly inside
/// `<drive_mount>/Users`; nothing else is accepted, and no profile is ever
/// guessed from directory order.
///
/// `CEILING_WINDOWS_USERNAME` is authoritative. If it is set but does not
/// resolve (typo, missing profile, reserved name) the answer is `None`: falling
/// through to `USERPROFILE` or `USER` would re-create the exact wrong-profile
/// binding the override exists to prevent.
fn resolve_windows_username_from(
    drive_mount: &Path,
    setting: Option<impl AsRef<str>>,
    userprofile: Option<impl AsRef<str>>,
    user: Option<impl AsRef<str>>,
) -> Option<String> {
    let users_dir = drive_mount.join("Users");
    if !users_dir.is_dir() {
        return None;
    }

    if let Some(setting) = non_empty(&setting) {
        // Authoritative: resolve it or give up.
        return if looks_like_path(setting) {
            profile_from_path(drive_mount, &users_dir, setting)
        } else {
            profile_from_username(&users_dir, setting)
        };
    }

    if let Some(userprofile) = non_empty(&userprofile)
        && let Some(name) = profile_from_path(drive_mount, &users_dir, userprofile)
    {
        return Some(name);
    }

    // `USER` is the Linux account name, so it is only ever a bare component.
    non_empty(&user).and_then(|user| profile_from_username(&users_dir, user))
}

fn non_empty<S: AsRef<str>>(value: &Option<S>) -> Option<&str> {
    value
        .as_ref()
        .map(|value| value.as_ref().trim())
        .filter(|value| !value.is_empty())
}

fn looks_like_path(raw: &str) -> bool {
    raw.contains(['/', '\\', ':'])
}

/// Accept a bare profile name (`Alice`) and return its on-disk spelling.
fn profile_from_username(users_dir: &Path, raw: &str) -> Option<String> {
    let name = raw.trim();
    if !is_profile_component(name) {
        return None;
    }
    existing_profile_name(users_dir, name)
}

/// Accept a profile *path* only when it identifies `<users_dir>/<profile>`.
///
/// `USERPROFILE=/tmp/Alice`, `D:\Profiles\Alice` and `C:\Temp\Alice` all get
/// rejected instead of binding `<users_dir>/Alice`, and traversal segments
/// never reach `users_dir.join(..)`.
fn profile_from_path(drive_mount: &Path, users_dir: &Path, raw: &str) -> Option<String> {
    let name = profile_name_under_users_dir(drive_mount, users_dir, raw)?;
    profile_from_username(users_dir, &name)
}

fn profile_name_under_users_dir(drive_mount: &Path, users_dir: &Path, raw: &str) -> Option<String> {
    let normalized = normalize_separators(raw);
    let normalized = normalized.trim();
    let trimmed = normalized.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    // Already mount-relative: `/mnt/c/Users/Alice`.
    if let Some(name) = name_directly_under(users_dir, trimmed) {
        return Some(name);
    }

    // Windows form: `C:\Users\Alice`. Only the drive this mount serves can be
    // used, because every path we build later hangs off `drive_mount`.
    let (letter, rest) = split_drive_prefix(trimmed)?;
    if Some(letter) != mount_drive_letter(drive_mount) {
        return None;
    }
    name_after_users_segment(rest)
}

/// Return `name` when `path` is exactly `<parent>/<name>`, comparing components.
fn name_directly_under(parent: &Path, path: &str) -> Option<String> {
    let parent = normalize_separators(&parent.to_string_lossy());
    let parent = parent.trim_end_matches('/');
    if parent.is_empty() {
        return None;
    }
    if path.starts_with('/') != parent.starts_with('/') {
        return None;
    }

    let mut actual = path.split('/').filter(|part| !part.is_empty());
    for expected in parent.split('/').filter(|part| !part.is_empty()) {
        if !actual.next()?.eq_ignore_ascii_case(expected) {
            return None;
        }
    }
    single_remaining(actual)
}

/// Return `name` for a mount-relative `Users/<name>` tail.
fn name_after_users_segment(rest: &str) -> Option<String> {
    let mut parts = rest.split('/').filter(|part| !part.is_empty());
    if !parts.next()?.eq_ignore_ascii_case("Users") {
        return None;
    }
    single_remaining(parts)
}

fn single_remaining<'a>(mut parts: impl Iterator<Item = &'a str>) -> Option<String> {
    let name = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    Some(name.to_string())
}

fn normalize_separators(raw: &str) -> String {
    raw.replace('\\', "/")
}

/// Split `C:/rest` into (`'c'`, `/rest`).
fn split_drive_prefix(path: &str) -> Option<(char, &str)> {
    let mut chars = path.char_indices();
    let (_, letter) = chars.next()?;
    let (colon_at, colon) = chars.next()?;
    if colon != ':' || !letter.is_ascii_alphabetic() {
        return None;
    }
    Some((letter.to_ascii_lowercase(), &path[colon_at + 1..]))
}

/// The drive letter a mount point serves: `/mnt/c` -> `'c'`.
fn mount_drive_letter(drive_mount: &Path) -> Option<char> {
    let name = drive_mount.file_name()?.to_str()?;
    let mut chars = name.chars();
    let letter = chars.next()?;
    (chars.next().is_none() && letter.is_ascii_alphabetic()).then(|| letter.to_ascii_lowercase())
}

/// A profile name must be exactly one ordinary path component.
fn is_profile_component(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && !name.contains(['/', '\\', ':'])
        // Rejects `.` and `..` without rejecting `.config`-style names.
        && name.chars().any(|c| c != '.')
        && !is_system_user_dir(name)
}

/// Look `name` up in `users_dir` and return the spelling that is on disk.
///
/// NTFS and the default DrvFs mount are case-insensitive, so `users_dir.join`
/// succeeds for the wrong casing and would hand back the probe string. Every
/// path we build later (`.../AppData/Local/...`) is derived from this value, so
/// it always comes from a directory entry.
///
/// An `AppData` sub-directory is a *preference*, not a requirement: it only
/// breaks ties between case variants. Requiring it would fail closed on valid
/// profiles where `AppData` is redirected, not created yet, or not visible on
/// the mount.
fn existing_profile_name(users_dir: &Path, name: &str) -> Option<String> {
    let entries = match std::fs::read_dir(users_dir) {
        Ok(entries) => entries,
        // Cannot enumerate (permissions, odd mount): probe directly and accept
        // the caller's spelling rather than losing the profile entirely.
        Err(_) => {
            return users_dir.join(name).is_dir().then(|| name.to_string());
        }
    };

    let wanted = name.to_lowercase();
    let mut with_appdata: Option<String> = None;
    let mut any_dir: Option<String> = None;

    for entry in entries.flatten() {
        let found = entry.file_name().to_string_lossy().into_owned();
        if found.to_lowercase() != wanted {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if found == name {
            return Some(found);
        }
        if is_windows_profile_dir(&path) {
            with_appdata.get_or_insert(found);
        } else {
            any_dir.get_or_insert(found);
        }
    }

    with_appdata.or(any_dir)
}

/// A directory that looks like a real Windows profile.
///
/// `AppData` has to be a directory: a file with that name means the tree is not
/// a usable profile.
fn is_windows_profile_dir(path: &Path) -> bool {
    path.is_dir() && path.join("AppData").is_dir()
}

fn is_system_user_dir(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "public"
            | "default"
            | "default user"
            | "all users"
            | "desktop"
            | "administrator"
            | "$recycle.bin"
            | "system volume information"
    )
}

/// Convert a Windows path to its WSL equivalent.
///
/// `C:\Users\John\AppData\Local` becomes `/mnt/c/Users/John/AppData/Local`, or
/// `/c/Users/John/AppData/Local` under a custom `[automount] root`.
#[allow(dead_code)]
pub fn windows_path_to_wsl(windows_path: &str) -> Option<PathBuf> {
    windows_path_to_wsl_with_root(windows_path, automount_root().as_deref())
}

/// `windows_path_to_wsl` against an explicit automount root. Split out so the
/// mapping is testable without a real `/etc/wsl.conf`.
fn windows_path_to_wsl_with_root(windows_path: &str, root: Option<&Path>) -> Option<PathBuf> {
    let path = windows_path.replace('\\', "/");

    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        let drive_letter = (path.as_bytes()[0] as char).to_lowercase().next()?;
        let rest = path[2..].trim_start_matches('/');
        let mount = drive_mount_with_root(root, drive_letter);
        return Some(if rest.is_empty() {
            mount
        } else {
            mount.join(rest)
        });
    }

    None
}

/// Get the Windows AppData/Local path from within WSL
pub fn windows_appdata_local() -> Option<PathBuf> {
    let info = get_wsl_info()?;
    let user = info.windows_username?;
    Some(
        info.drive_mount
            .join("Users")
            .join(user)
            .join("AppData")
            .join("Local"),
    )
}

/// Get the Windows AppData/Roaming path from within WSL
pub fn windows_appdata_roaming() -> Option<PathBuf> {
    let info = get_wsl_info()?;
    let user = info.windows_username?;
    Some(
        info.drive_mount
            .join("Users")
            .join(user)
            .join("AppData")
            .join("Roaming"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Build a stand-in for `/mnt/c`: the last component is the drive letter,
    /// so `C:\Users\Alice` maps into the fixture the same way it does in WSL.
    fn drive_fixture(names: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().expect("temp dir");
        let drive_mount = temp.path().join("c");
        let users_dir = drive_mount.join("Users");
        fs::create_dir_all(&users_dir).expect("users dir");
        for name in names {
            fs::create_dir_all(users_dir.join(name).join("AppData")).expect("profile dir");
        }
        (temp, drive_mount)
    }

    fn users_dir_of(drive_mount: &Path) -> PathBuf {
        drive_mount.join("Users")
    }

    fn wsl_style(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    fn resolve(
        drive_mount: &Path,
        setting: Option<&str>,
        userprofile: Option<&str>,
        user: Option<&str>,
    ) -> Option<String> {
        resolve_windows_username_from(drive_mount, setting, userprofile, user)
    }

    #[test]
    fn test_is_system_user_dir() {
        assert!(is_system_user_dir("Public"));
        assert!(is_system_user_dir("Default"));
        assert!(is_system_user_dir("Default User"));
        assert!(!is_system_user_dir("John"));
        assert!(!is_system_user_dir("alice"));
    }

    #[test]
    fn test_windows_path_to_wsl() {
        let root = None;
        assert_eq!(
            windows_path_to_wsl_with_root(r"C:\Users\John\AppData\Local", root),
            Some(PathBuf::from("/mnt/c/Users/John/AppData/Local"))
        );
        assert_eq!(
            windows_path_to_wsl_with_root("D:\\Games", root),
            Some(PathBuf::from("/mnt/d/Games"))
        );
        assert_eq!(windows_path_to_wsl_with_root("/home/user", root), None);
    }

    #[test]
    fn windows_path_to_wsl_honours_a_custom_root() {
        // `[automount] root = /` is the case that used to silently break every
        // Windows-side probe: paths live under `/c`, not `/mnt/c`.
        let root = Some(Path::new("/"));
        assert_eq!(
            windows_path_to_wsl_with_root(r"C:\Users\John\AppData\Local", root),
            Some(PathBuf::from("/c/Users/John/AppData/Local"))
        );
        assert_eq!(
            windows_path_to_wsl_with_root(r"C:\", root),
            Some(PathBuf::from("/c"))
        );
        assert_eq!(
            windows_path_to_wsl_with_root("D:\\Games", Some(Path::new("/windows"))),
            Some(PathBuf::from("/windows/d/Games"))
        );
    }

    #[test]
    fn parses_the_default_and_custom_automount_roots() {
        // No key, no section, a commented-out key, or a non-root key all mean
        // "use the /mnt default".
        assert_eq!(parse_automount_root(""), None);
        assert_eq!(
            parse_automount_root("[interop]\nappendWindowsPath = true"),
            None
        );
        assert_eq!(parse_automount_root("[automount]\n# root = /\n"), None);
        assert_eq!(
            parse_automount_root("[automount]\noptions = \"metadata\""),
            None
        );

        assert_eq!(
            parse_automount_root("[automount]\nroot = /mnt/"),
            Some(PathBuf::from("/mnt"))
        );
        assert_eq!(
            parse_automount_root("[automount]\nroot = /"),
            Some(PathBuf::from("/"))
        );
        assert_eq!(
            parse_automount_root("[automount]\nroot = /windows"),
            Some(PathBuf::from("/windows"))
        );
        // Quoted values, trailing comments, and casing all appear in real
        // wsl.conf files.
        assert_eq!(
            parse_automount_root("[automount]\nROOT = \"/\" # drives at /c"),
            Some(PathBuf::from("/"))
        );
    }

    #[test]
    fn rejects_malformed_automount_roots() {
        // A relative or empty value is not a mount point; fall back to /mnt.
        assert_eq!(parse_automount_root("[automount]\nroot = mnt"), None);
        assert_eq!(parse_automount_root("[automount]\nroot ="), None);
    }

    #[test]
    fn drive_mount_joins_the_root_without_doubling_slashes() {
        // The join shape is what every Windows path in this module hangs off,
        // so a `root = /` must still produce `/c`, not `//c`.
        assert_eq!(
            drive_mount_with_root(Some(Path::new("/")), 'c'),
            PathBuf::from("/c")
        );
        assert_eq!(
            drive_mount_with_root(Some(Path::new("/mnt")), 'c'),
            PathBuf::from("/mnt/c")
        );
        assert_eq!(
            drive_mount_with_root(Some(Path::new("/windows")), 'D'),
            PathBuf::from("/windows/d")
        );
        assert_eq!(drive_mount_with_root(None, 'c'), PathBuf::from("/mnt/c"));
    }

    #[test]
    fn resolve_matches_user_when_profile_exists() {
        let (_temp, mount) = drive_fixture(&["Alice", "Bob"]);
        assert_eq!(
            resolve(&mount, None, None, Some("Alice")),
            Some("Alice".into())
        );
    }

    #[test]
    fn resolve_fails_closed_when_user_does_not_match() {
        let (_temp, mount) = drive_fixture(&["Alice", "Bob"]);
        assert_eq!(resolve(&mount, None, None, Some("wsluser")), None);
    }

    #[test]
    fn resolve_matches_userprofile_windows_path() {
        let (_temp, mount) = drive_fixture(&["Alice", "Bob"]);
        assert_eq!(
            resolve(&mount, None, Some(r"C:\Users\Bob"), Some("wsluser")),
            Some("Bob".into())
        );
    }

    #[test]
    fn resolve_matches_userprofile_wsl_path() {
        let (_temp, mount) = drive_fixture(&["Alice", "Bob"]);
        let profile = format!("{}/Alice/", wsl_style(&users_dir_of(&mount)));
        assert_eq!(
            resolve(&mount, None, Some(profile.as_str()), Some("wsluser")),
            Some("Alice".into())
        );
    }

    #[test]
    fn resolve_prefers_explicit_setting_over_userprofile() {
        let (_temp, mount) = drive_fixture(&["Alice", "Bob"]);
        assert_eq!(
            resolve(&mount, Some("Alice"), Some(r"C:\Users\Bob"), Some("Bob")),
            Some("Alice".into())
        );
    }

    #[test]
    fn resolve_matches_setting_profile_path() {
        let (_temp, mount) = drive_fixture(&["Alice", "Bob"]);
        assert_eq!(
            resolve(&mount, Some(r"C:\Users\Bob"), None, Some("wsluser")),
            Some("Bob".into())
        );
    }

    #[test]
    fn resolve_rejects_system_and_unknown_setting() {
        let (_temp, mount) = drive_fixture(&["Alice", "Public"]);
        assert_eq!(resolve(&mount, Some("Public"), None, None), None);
        assert_eq!(resolve(&mount, Some("Missing"), None, None), None);
    }

    #[test]
    fn resolve_matches_user_case_insensitively() {
        let (_temp, mount) = drive_fixture(&["Alice"]);
        assert_eq!(
            resolve(&mount, None, None, Some("alice")),
            Some("Alice".into())
        );
    }

    #[test]
    fn resolve_does_not_guess_first_profile() {
        let (_temp, mount) = drive_fixture(&["Alice", "Bob"]);
        assert_eq!(resolve(&mount, None, None, None), None);
    }

    #[test]
    fn resolve_ignores_blank_setting() {
        let (_temp, mount) = drive_fixture(&["Alice"]);
        assert_eq!(
            resolve(&mount, Some("   "), None, Some("Alice")),
            Some("Alice".into())
        );
    }

    /// An override that fails to resolve must not hand the session to
    /// `USERPROFILE` or `USER`.
    #[test]
    fn resolve_does_not_fall_back_when_setting_is_invalid() {
        let (_temp, mount) = drive_fixture(&["Alice", "Bob"]);
        assert_eq!(
            resolve(&mount, Some("Missing"), Some(r"C:\Users\Bob"), Some("Bob")),
            None
        );
        assert_eq!(
            resolve(&mount, Some(r"C:\Temp\Alice"), None, Some("Alice")),
            None
        );
    }

    /// A path that is not `<users_dir>/<profile>` must never bind a profile
    /// just because its last component happens to match one.
    #[test]
    fn resolve_rejects_profile_paths_outside_users_dir() {
        let (_temp, mount) = drive_fixture(&["Alice"]);
        for outside in [
            "/tmp/Alice",
            r"C:\Temp\Alice",
            "D:/Profiles/Alice",
            "D:/Users/Alice",
            r"\\server\share\Alice",
            "/mnt/d/Users/Alice",
        ] {
            assert_eq!(
                resolve(&mount, Some(outside), None, None),
                None,
                "setting {outside} should not resolve"
            );
            assert_eq!(
                resolve(&mount, None, Some(outside), None),
                None,
                "userprofile {outside} should not resolve"
            );
        }
    }

    /// `..` must never reach `users_dir.join`.
    #[test]
    fn resolve_rejects_traversal_components() {
        let (_temp, mount) = drive_fixture(&["Alice"]);
        let users = wsl_style(&users_dir_of(&mount));
        let traversals = [
            ".".to_string(),
            "..".to_string(),
            "../Alice".to_string(),
            r"..\Alice".to_string(),
            r"C:\Users\..\Users\Alice".to_string(),
            format!("{users}/.."),
            format!("{users}/../Users/Alice"),
            format!("{users}/./Alice"),
        ];
        for traversal in traversals {
            let traversal = traversal.as_str();
            assert_eq!(
                resolve(&mount, Some(traversal), None, None),
                None,
                "setting {traversal} should not resolve"
            );
            assert_eq!(
                resolve(&mount, None, Some(traversal), None),
                None,
                "userprofile {traversal} should not resolve"
            );
            assert_eq!(
                resolve(&mount, None, None, Some(traversal)),
                None,
                "user {traversal} should not resolve"
            );
        }
    }

    /// `USERPROFILE` is inherited, not chosen, so an unusable value still falls
    /// through to `USER`.
    #[test]
    fn resolve_falls_back_to_user_when_userprofile_is_unusable() {
        let (_temp, mount) = drive_fixture(&["Alice", "Bob"]);
        assert_eq!(
            resolve(&mount, None, Some("D:/Profiles/Alice"), Some("Bob")),
            Some("Bob".into())
        );
    }

    /// `AppData` is a tie-breaker, not a gate: a profile without it still
    /// resolves, exactly as it did before this change.
    #[test]
    fn resolve_accepts_profile_without_appdata() {
        let (_temp, mount) = drive_fixture(&[]);
        let users_dir = users_dir_of(&mount);
        fs::create_dir_all(users_dir.join("Alice")).expect("profile dir");
        fs::create_dir_all(users_dir.join("Bob")).expect("profile dir");
        fs::write(users_dir.join("Bob").join("AppData"), b"not a directory").expect("appdata file");

        assert_eq!(
            resolve(&mount, None, None, Some("Alice")),
            Some("Alice".into())
        );
        assert_eq!(resolve(&mount, Some("Bob"), None, None), Some("Bob".into()));
        assert_eq!(
            resolve(&mount, None, Some(r"C:\Users\Alice"), None),
            Some("Alice".into())
        );
    }

    /// A *file* named `AppData` does not make a directory a Windows profile.
    #[test]
    fn windows_profile_dir_requires_appdata_directory() {
        let temp = tempfile::tempdir().expect("temp dir");
        let real = temp.path().join("Alice");
        fs::create_dir_all(real.join("AppData")).expect("profile dir");
        let malformed = temp.path().join("Bob");
        fs::create_dir_all(&malformed).expect("profile dir");
        fs::write(malformed.join("AppData"), b"not a directory").expect("appdata file");

        assert!(is_windows_profile_dir(&real));
        assert!(!is_windows_profile_dir(&malformed));
        assert!(!is_windows_profile_dir(&temp.path().join("Missing")));
    }

    #[test]
    fn resolve_returns_none_without_users_directory() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mount = temp.path().join("c");
        fs::create_dir_all(&mount).expect("mount dir");
        assert_eq!(resolve(&mount, Some("Alice"), None, Some("Alice")), None);
    }

    #[test]
    fn mount_drive_letter_reads_the_last_component() {
        assert_eq!(mount_drive_letter(Path::new("/mnt/c")), Some('c'));
        assert_eq!(mount_drive_letter(Path::new("/mnt/D")), Some('d'));
        assert_eq!(mount_drive_letter(Path::new("/mnt/wsl")), None);
        assert_eq!(mount_drive_letter(Path::new("/")), None);
    }

    #[test]
    fn profile_components_reject_non_simple_names() {
        assert!(is_profile_component("Alice"));
        assert!(is_profile_component(".config"));
        assert!(!is_profile_component(""));
        assert!(!is_profile_component("."));
        assert!(!is_profile_component(".."));
        assert!(!is_profile_component("a/b"));
        assert!(!is_profile_component("a\\b"));
        assert!(!is_profile_component("C:"));
        assert!(!is_profile_component(" Alice"));
        assert!(!is_profile_component("Public"));
    }
}
