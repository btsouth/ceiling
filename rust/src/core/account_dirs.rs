//! Directory-backed provider accounts.
//!
//! Codex and Claude both authenticate through a CLI that owns a config
//! directory (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`) holding exactly one signed-in
//! credential, and both rotate that credential in place. So Ceiling models an
//! account as a *directory* rather than a stored token: the pointer stays valid
//! where a copy would go stale within hours, and Ceiling never has to hold a
//! provider credential of its own.
//!
//! Nothing here reads token material. Identities carry only what is needed to
//! label an account and to key usage by it.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashSet;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::{fs, io};
use uuid::Uuid;

/// Provider-specific behavior for a directory-backed account.
pub trait AccountIdentity: Clone + PartialEq + Serialize + DeserializeOwned {
    /// The directory the CLI itself would use when the user has configured no
    /// accounts explicitly. `None` when that directory cannot be resolved
    /// (no provider env var and no home). The working directory is not a
    /// substitute (SBS-1021 / SBS-950).
    fn ambient_dir() -> Option<PathBuf>;

    /// File name this provider's accounts are persisted under.
    fn store_file_name() -> &'static str;

    /// Read identity for a config directory, or `None` when it holds no
    /// resolvable signed-in account.
    fn read(config_dir: &Path) -> Option<Self>;

    /// Whether this directory currently holds a usable sign-in. Distinct from
    /// [`Self::read`]: a directory can be signed in while carrying no readable
    /// identity, and can carry a cached identity while signed out.
    fn is_signed_in(config_dir: &Path) -> bool;

    /// Best-effort human label for this identity.
    fn suggested_label(&self) -> Option<String>;
}

/// A single directory-backed account.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DirectoryAccount<I> {
    /// Unique identifier
    pub id: Uuid,
    /// User-facing label, seeded from the directory's own identity when available
    pub label: String,
    /// The config directory backing this account
    pub config_dir: PathBuf,
    /// Optional accent color so multiple accounts stay distinguishable at a glance
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<String>,
    /// Identity last resolved from the directory, cached so the UI keeps a label
    /// even while the credential file is mid-rotation
    #[serde(default = "none_identity", skip_serializing_if = "Option::is_none")]
    pub identity: Option<I>,
    /// When this account was added (Unix timestamp in seconds)
    pub added_at: i64,
    /// When this account was last fetched (Unix timestamp in seconds)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<i64>,
}

fn none_identity<I>() -> Option<I> {
    None
}

impl<I: AccountIdentity> DirectoryAccount<I> {
    /// Create an account for a directory, labeling it from the directory's own
    /// identity when the caller did not supply a label.
    pub fn new(label: Option<String>, config_dir: PathBuf) -> Self {
        let identity = I::read(&config_dir);
        let label = label
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| identity.as_ref().and_then(I::suggested_label))
            .unwrap_or_else(|| fallback_label(&config_dir));

        Self {
            id: Uuid::new_v4(),
            label,
            config_dir,
            tint: None,
            identity,
            added_at: Utc::now().timestamp(),
            last_used: None,
        }
    }

    /// Re-read the directory's identity, keeping the previous one when the
    /// credential file is currently unreadable. A rotating credential should not
    /// blank the label.
    pub fn refresh_identity(&mut self) {
        if let Some(identity) = I::read(&self.config_dir) {
            self.identity = Some(identity);
        }
    }

    /// Mark this account as used
    pub fn mark_used(&mut self) {
        self.last_used = Some(Utc::now().timestamp());
    }

    /// Get display name
    pub fn display_name(&self) -> &str {
        &self.label
    }

    /// Get added_at as DateTime
    pub fn added_at_datetime(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.added_at, 0).unwrap_or_else(Utc::now)
    }

    /// Get last_used as DateTime
    pub fn last_used_datetime(&self) -> Option<DateTime<Utc>> {
        self.last_used
            .and_then(|ts| DateTime::from_timestamp(ts, 0))
    }
}

/// Label used when a directory has no readable identity: its own name, which is
/// what the user chose when they created it.
fn fallback_label(config_dir: &Path) -> String {
    config_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| config_dir.display().to_string())
}

/// The set of accounts configured for one provider, and which is active.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DirectoryAccountData<I> {
    /// File format version
    #[serde(default = "default_version")]
    pub version: u32,
    /// Configured accounts, in display order
    #[serde(default = "Vec::new")]
    pub accounts: Vec<DirectoryAccount<I>>,
    /// Index of the active account
    #[serde(default)]
    pub active_index: usize,
}

fn default_version() -> u32 {
    1
}

impl<I> Default for DirectoryAccountData<I> {
    fn default() -> Self {
        Self {
            version: default_version(),
            accounts: Vec::new(),
            active_index: 0,
        }
    }
}

impl<I: AccountIdentity> DirectoryAccountData<I> {
    /// Create empty account data
    pub fn new() -> Self {
        Self::default()
    }

    /// Active index clamped into range, so a stale index can never panic
    pub fn clamped_active_index(&self) -> usize {
        if self.accounts.is_empty() {
            0
        } else {
            self.active_index.min(self.accounts.len() - 1)
        }
    }

    /// The active account, if any are configured
    pub fn active_account(&self) -> Option<&DirectoryAccount<I>> {
        self.accounts.get(self.clamped_active_index())
    }

    /// Mutable access to the active account
    pub fn active_account_mut(&mut self) -> Option<&mut DirectoryAccount<I>> {
        let index = self.clamped_active_index();
        self.accounts.get_mut(index)
    }

    /// The config directory Ceiling should read for this provider.
    ///
    /// Falls back to the ambient directory when no accounts are configured, so
    /// users who never open the Accounts UI keep today's behavior exactly: the
    /// CLI stays the source of truth and switching there is still picked up
    /// automatically. `None` when there is no configured account and no
    /// resolvable ambient directory — never the working directory (SBS-1021).
    pub fn active_dir(&self) -> Option<PathBuf> {
        self.active_account()
            .map(|account| account.config_dir.clone())
            .or_else(I::ambient_dir)
    }

    /// Whether Ceiling is tracking accounts explicitly rather than following
    /// whichever account the CLI is currently signed in as.
    pub fn is_explicit(&self) -> bool {
        !self.accounts.is_empty()
    }

    /// Add an account, keeping it active so the user sees the one they just added.
    ///
    /// Directories are unique: re-adding a configured directory updates that
    /// entry rather than creating a duplicate that would double-count it.
    pub fn add_account(&mut self, account: DirectoryAccount<I>) -> Uuid {
        if let Some(existing) = self
            .accounts
            .iter_mut()
            .find(|candidate| same_dir(&candidate.config_dir, &account.config_dir))
        {
            existing.label = account.label;
            existing.identity = account.identity;
            if account.tint.is_some() {
                existing.tint = account.tint;
            }
            let id = existing.id;
            self.set_active_by_id(id);
            return id;
        }

        let id = account.id;
        self.accounts.push(account);
        self.active_index = self.accounts.len() - 1;
        id
    }

    /// Remove an account, keeping the previously active account active when it
    /// survives the removal.
    pub fn remove_account(&mut self, id: Uuid) -> Option<DirectoryAccount<I>> {
        let index = self.accounts.iter().position(|account| account.id == id)?;
        let active_id = self.active_account().map(|account| account.id);
        let removed = self.accounts.remove(index);

        self.active_index = match active_id {
            Some(active_id) if active_id != removed.id => self
                .accounts
                .iter()
                .position(|account| account.id == active_id)
                .unwrap_or(0),
            // The active account itself went away: fall back to its neighbor.
            _ => index.min(self.accounts.len().saturating_sub(1)),
        };

        Some(removed)
    }

    /// Set the active account by index
    pub fn set_active(&mut self, index: usize) {
        if index < self.accounts.len() {
            self.active_index = index;
        }
    }

    /// Set the active account by id, returning whether it was found
    pub fn set_active_by_id(&mut self, id: Uuid) -> bool {
        match self.accounts.iter().position(|account| account.id == id) {
            Some(index) => {
                self.active_index = index;
                true
            }
            None => false,
        }
    }

    /// Whether more than one account is configured
    pub fn has_multiple(&self) -> bool {
        self.accounts.len() > 1
    }

    /// Number of configured accounts
    pub fn count(&self) -> usize {
        self.accounts.len()
    }

    /// Every distinct directory Ceiling should scan for local activity. Includes
    /// the ambient directory when no accounts are configured and one can be
    /// resolved. A missing home yields no scan roots, not cwd (SBS-1021).
    pub fn all_dirs(&self) -> Vec<PathBuf> {
        if self.accounts.is_empty() {
            return I::ambient_dir().into_iter().collect();
        }
        let mut seen = HashSet::new();
        self.accounts
            .iter()
            .map(|account| account.config_dir.clone())
            .filter(|dir| seen.insert(dir_key(dir)))
            .collect()
    }
}

/// Compare two config directories for identity. Paths are normalized only by
/// trimming trailing separators and, on Windows, by case: the filesystem there
/// is case-insensitive, so `C:\Codex` and `c:\codex` are one account.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    dir_key(a) == dir_key(b)
}

/// Canonical key for a config directory. Same identity as [`same_dir`]:
/// trailing separators stripped, and lowercased on Windows.
pub(crate) fn dir_key(dir: &Path) -> String {
    let raw = dir.to_string_lossy();
    let trimmed = raw.trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        trimmed.to_lowercase()
    } else {
        trimmed.to_string()
    }
}

/// Decode the payload segment of a JWT. **No signature verification**: callers
/// use this only on local files the user already controls, for display values.
pub fn decode_jwt_claims(token: &str) -> Option<serde_json::Value> {
    use base64::Engine;

    let payload = token.split('.').nth(1)?;
    let mut padded = payload.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(&padded)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(&padded))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Read a non-empty trimmed string field from a JSON object.
pub(crate) fn json_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|field| field.as_str())
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(str::to_string)
}

/// Persists [`DirectoryAccountData`] to disk.
pub struct DirectoryAccountStore<I> {
    file_path: PathBuf,
    _identity: PhantomData<I>,
}

/// Errors that can occur with account storage
#[derive(Debug, thiserror::Error)]
pub enum AccountStoreError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl<I: AccountIdentity> DirectoryAccountStore<I> {
    /// Create a new store with the default path
    pub fn new() -> Self {
        Self::with_path(Self::default_path())
    }

    /// Create a store with a custom path
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            file_path: path,
            _identity: PhantomData,
        }
    }

    /// Get the default storage path, alongside `token-accounts.json`
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .map(|dir| dir.join("CodexBar"))
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".codexbar")
            })
            .join(I::store_file_name())
    }

    /// Load accounts from disk, returning empty data when nothing is configured.
    ///
    /// Read-only. Mutations must go through [`Self::try_update`]; `load` then
    /// [`Self::save`] can overwrite a concurrent edit from a stale snapshot.
    pub fn load(&self) -> Result<DirectoryAccountData<I>, AccountStoreError> {
        if !self.file_path.exists() {
            return Ok(DirectoryAccountData::new());
        }
        let data = crate::secure_file::read_string(&self.file_path)?;
        Ok(serde_json::from_str(&data)?)
    }

    /// Apply a mutation to the latest on-disk snapshot while holding the shared
    /// cross-process state lock for the complete read-modify-write cycle.
    ///
    /// Callers should perform provider or network I/O before entering this
    /// closure. Only the local account mutation and its atomic save belong in
    /// the transaction.
    pub fn try_update<T>(
        &self,
        operation: impl FnOnce(&mut DirectoryAccountData<I>) -> Result<T, String>,
    ) -> anyhow::Result<(DirectoryAccountData<I>, T)> {
        with_store_lock(|| {
            let mut data = self.load()?;
            let original = data.clone();
            let result = operation(&mut data).map_err(io::Error::other)?;
            // A no-op must not create a missing file or rewrite one and drop
            // unknown fields the deserialized snapshot never had.
            if data != original {
                self.save_unlocked(&data)?;
            }
            Ok((data, result))
        })
        .map_err(Into::into)
    }

    /// Replace the on-disk snapshot. For a read-modify-write, use
    /// [`Self::try_update`] so the load stays under the same lock as the save.
    pub fn save(&self, data: &DirectoryAccountData<I>) -> Result<(), AccountStoreError> {
        with_store_lock(|| self.save_unlocked(data))
    }

    fn save_unlocked(&self, data: &DirectoryAccountData<I>) -> Result<(), AccountStoreError> {
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(data)?;
        crate::secure_file::write_string(&self.file_path, &json)?;
        Ok(())
    }
}

fn with_store_lock<T>(
    operation: impl FnOnce() -> Result<T, AccountStoreError>,
) -> Result<T, AccountStoreError> {
    let mut op_err = None;
    crate::secure_file::with_state_write_lock(|| match operation() {
        Ok(value) => Ok(value),
        Err(err) => {
            op_err = Some(err);
            Err(io::Error::other("account store operation failed"))
        }
    })
    .map_err(|lock_err| op_err.unwrap_or(AccountStoreError::Io(lock_err)))
}

impl<I: AccountIdentity> Default for DirectoryAccountStore<I> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal identity so the shared machinery is tested without depending on
    /// either provider's credential format.
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct TestIdentity {
        name: String,
    }

    impl AccountIdentity for TestIdentity {
        fn ambient_dir() -> Option<PathBuf> {
            Some(PathBuf::from("/ambient"))
        }
        fn store_file_name() -> &'static str {
            "test-accounts.json"
        }
        fn read(_config_dir: &Path) -> Option<Self> {
            None
        }
        fn is_signed_in(_config_dir: &Path) -> bool {
            false
        }
        fn suggested_label(&self) -> Option<String> {
            Some(self.name.clone())
        }
    }

    type Data = DirectoryAccountData<TestIdentity>;

    fn account(label: &str, dir: &str) -> DirectoryAccount<TestIdentity> {
        DirectoryAccount {
            id: Uuid::new_v4(),
            label: label.to_string(),
            config_dir: PathBuf::from(dir),
            tint: None,
            identity: None,
            added_at: 0,
            last_used: None,
        }
    }

    #[test]
    fn no_accounts_follows_the_ambient_directory() {
        let data = Data::new();
        assert!(data.active_account().is_none());
        assert!(!data.is_explicit());
        assert_eq!(data.active_dir(), Some(PathBuf::from("/ambient")));
        assert_eq!(data.all_dirs(), vec![PathBuf::from("/ambient")]);
    }

    /// Pins SBS-1021: following the CLI with no resolvable ambient directory
    /// must not invent `.` as the config/session root.
    #[test]
    fn missing_ambient_dir_is_not_the_working_directory() {
        #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        struct NoHomeIdentity;

        impl AccountIdentity for NoHomeIdentity {
            fn ambient_dir() -> Option<PathBuf> {
                None
            }
            fn store_file_name() -> &'static str {
                "no-home-accounts.json"
            }
            fn read(_config_dir: &Path) -> Option<Self> {
                None
            }
            fn is_signed_in(_config_dir: &Path) -> bool {
                false
            }
            fn suggested_label(&self) -> Option<String> {
                None
            }
        }

        let data = DirectoryAccountData::<NoHomeIdentity>::new();
        assert_eq!(data.active_dir(), None);
        assert!(data.all_dirs().is_empty());
    }

    #[test]
    fn adding_an_account_makes_it_active() {
        let mut data = Data::new();
        data.add_account(account("personal", "/dirs/personal"));
        let id = data.add_account(account("work", "/dirs/work"));

        assert_eq!(data.count(), 2);
        assert!(data.has_multiple());
        assert!(data.is_explicit());
        assert_eq!(data.active_account().map(|a| a.id), Some(id));
        assert_eq!(data.active_dir(), Some(PathBuf::from("/dirs/work")));
    }

    #[test]
    fn re_adding_a_directory_updates_in_place_instead_of_duplicating() {
        let mut data = Data::new();
        let first = data.add_account(account("personal", "/dirs/personal"));
        data.add_account(account("work", "/dirs/work"));

        let again = data.add_account(account("renamed", "/dirs/personal"));

        assert_eq!(data.count(), 2);
        assert_eq!(again, first, "the existing entry should be reused");
        assert_eq!(
            data.active_account().map(|a| a.label.as_str()),
            Some("renamed")
        );
    }

    #[test]
    fn removing_an_inactive_account_keeps_the_active_one() {
        let mut data = Data::new();
        let first = data.add_account(account("personal", "/dirs/personal"));
        let second = data.add_account(account("work", "/dirs/work"));
        data.add_account(account("school", "/dirs/school"));
        data.set_active_by_id(second);

        data.remove_account(first);

        assert_eq!(data.active_account().map(|a| a.id), Some(second));
    }

    #[test]
    fn removing_the_active_account_falls_back_to_a_neighbor() {
        let mut data = Data::new();
        let first = data.add_account(account("personal", "/dirs/personal"));
        let second = data.add_account(account("work", "/dirs/work"));
        data.set_active_by_id(second);

        data.remove_account(second);

        assert_eq!(data.active_account().map(|a| a.id), Some(first));
        assert_eq!(data.active_dir(), Some(PathBuf::from("/dirs/personal")));
    }

    #[test]
    fn removing_the_last_account_returns_to_following_the_cli() {
        let mut data = Data::new();
        let only = data.add_account(account("personal", "/dirs/personal"));

        data.remove_account(only);

        assert_eq!(data.count(), 0);
        assert!(!data.is_explicit());
        assert_eq!(data.active_dir(), Some(PathBuf::from("/ambient")));
    }

    #[test]
    fn a_stale_active_index_is_clamped_rather_than_panicking() {
        let mut data = Data::new();
        data.add_account(account("personal", "/dirs/personal"));
        data.active_index = 17;

        assert_eq!(data.clamped_active_index(), 0);
        assert_eq!(data.active_dir(), Some(PathBuf::from("/dirs/personal")));
    }

    #[test]
    fn all_dirs_deduplicates_trailing_separators() {
        let mut data = Data::new();
        data.accounts.push(account("a", "/dirs/shared"));
        data.accounts.push(account("b", "/dirs/shared/"));
        data.accounts.push(account("c", "/dirs/other"));

        assert_eq!(
            data.all_dirs(),
            vec![PathBuf::from("/dirs/shared"), PathBuf::from("/dirs/other")]
        );
    }

    #[test]
    fn round_trips_through_the_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: DirectoryAccountStore<TestIdentity> =
            DirectoryAccountStore::with_path(dir.path().join("test-accounts.json"));

        assert_eq!(store.load().expect("empty load").count(), 0);

        let mut data = Data::new();
        data.add_account(account("personal", "/dirs/personal"));
        let work = data.add_account(account("work", "/dirs/work"));
        store.save(&data).expect("save");

        let loaded = store.load().expect("load");
        assert_eq!(loaded.count(), 2);
        assert_eq!(loaded.active_account().map(|a| a.id), Some(work));
    }

    #[test]
    fn transactional_updates_preserve_two_concurrent_directory_account_adds() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test-accounts.json");
        let barrier = Arc::new(Barrier::new(3));
        let writers: Vec<_> = ["personal", "work"]
            .into_iter()
            .map(|label| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    DirectoryAccountStore::<TestIdentity>::with_path(path)
                        .try_update(|data| {
                            data.add_account(account(label, &format!("/dirs/{label}")));
                            Ok(())
                        })
                        .expect("transactional directory account update");
                })
            })
            .collect();

        barrier.wait();
        for writer in writers {
            writer.join().expect("writer thread");
        }

        let stored = DirectoryAccountStore::<TestIdentity>::with_path(path)
            .load()
            .expect("reload");
        assert_eq!(stored.count(), 2);
        assert!(
            stored
                .accounts
                .iter()
                .any(|entry| entry.label == "personal")
        );
        assert!(stored.accounts.iter().any(|entry| entry.label == "work"));
    }

    #[test]
    fn no_op_try_update_leaves_a_missing_store_file_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test-accounts.json");
        let store: DirectoryAccountStore<TestIdentity> =
            DirectoryAccountStore::with_path(path.clone());

        store
            .try_update(|_| Ok(()))
            .expect("no-op directory account update");

        assert!(
            !path.exists(),
            "a no-op must not create an empty accounts file"
        );
    }

    #[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
    struct UnserializableIdentity;

    impl serde::Serialize for UnserializableIdentity {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::Error;
            Err(S::Error::custom("identity cannot be serialized"))
        }
    }

    impl AccountIdentity for UnserializableIdentity {
        fn ambient_dir() -> Option<PathBuf> {
            Some(PathBuf::from("/ambient"))
        }
        fn store_file_name() -> &'static str {
            "unserializable-accounts.json"
        }
        fn read(_config_dir: &Path) -> Option<Self> {
            None
        }
        fn is_signed_in(_config_dir: &Path) -> bool {
            false
        }
        fn suggested_label(&self) -> Option<String> {
            None
        }
    }

    #[test]
    fn save_keeps_json_errors_as_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: DirectoryAccountStore<UnserializableIdentity> =
            DirectoryAccountStore::with_path(dir.path().join("test-accounts.json"));

        let mut data = DirectoryAccountData::<UnserializableIdentity>::new();
        data.accounts.push(DirectoryAccount {
            id: Uuid::new_v4(),
            label: "broken".to_string(),
            config_dir: PathBuf::from("/dirs/broken"),
            tint: None,
            identity: Some(UnserializableIdentity),
            added_at: 0,
            last_used: None,
        });

        let error = store.save(&data).expect_err("serialize must fail");
        assert!(
            matches!(error, AccountStoreError::Json(_)),
            "serde failures must stay Json, got {error:?}"
        );
    }

    #[test]
    fn try_update_keeps_json_errors_as_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store: DirectoryAccountStore<UnserializableIdentity> =
            DirectoryAccountStore::with_path(dir.path().join("test-accounts.json"));

        let error = store
            .try_update(|data| {
                data.accounts.push(DirectoryAccount {
                    id: Uuid::new_v4(),
                    label: "broken".to_string(),
                    config_dir: PathBuf::from("/dirs/broken"),
                    tint: None,
                    identity: Some(UnserializableIdentity),
                    added_at: 0,
                    last_used: None,
                });
                Ok(())
            })
            .expect_err("serialize must fail");

        let store_error = error
            .downcast_ref::<AccountStoreError>()
            .expect("store error must not be collapsed into io::Error");
        assert!(
            matches!(store_error, AccountStoreError::Json(_)),
            "serde failures must stay Json, got {store_error:?}"
        );
    }
}
