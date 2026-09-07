//! A record of which account was signed into a config directory, and when.
//!
//! Neither Codex rollouts nor Claude transcripts carry an account identifier, so
//! local activity cannot be attributed from log content alone. Splitting
//! accounts into separate config directories solves that, but most people keep
//! one directory and re-run `codex login` / `claude` to switch.
//!
//! This ledger covers that case. Ceiling already reads each directory's
//! credential on every refresh, so it notes when the identity there changes and
//! builds a timeline. Any log record can then be attributed by its timestamp.
//!
//! What it cannot do is recover the past: the ledger only knows what it watched.
//! Records written before Ceiling first observed a directory are reported as
//! [`Attribution::Unknown`] rather than guessed at.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{fs, io};

use super::{ClaudeIdentity, CodexIdentity, ConfiguredAccounts, ProviderId, same_dir};

/// One continuous stretch during which a directory held one account.
///
/// Switching away and back later produces two sightings, not one widened range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSighting {
    /// Stable per-account key: the provider's account uuid when it has one,
    /// otherwise the email.
    pub account_key: String,
    /// Human label at the time of the sighting, for display.
    pub label: String,
    /// First observation of this account in this directory (Unix seconds).
    pub first_seen: i64,
    /// Most recent observation (Unix seconds).
    pub last_seen: i64,
}

/// The sightings recorded for one config directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryLedger {
    pub config_dir: PathBuf,
    /// Chronological, non-overlapping.
    #[serde(default)]
    pub sightings: Vec<AccountSighting>,
}

/// How confidently a timestamp maps to an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attribution {
    /// The timestamp falls inside an observed stretch.
    Exact { account_key: String, label: String },
    /// The timestamp falls in a gap between two observations that saw different
    /// accounts. The switch happened somewhere in that gap, so this names the
    /// account observed before it without claiming certainty.
    Ambiguous {
        account_key: String,
        label: String,
        next_account_key: String,
    },
    /// The timestamp predates the first observation of this directory. Nothing
    /// recorded it, and the log itself does not say.
    Unknown,
}

impl Attribution {
    /// The account key, for grouping. `None` when unattributable.
    pub fn account_key(&self) -> Option<&str> {
        match self {
            Self::Exact { account_key, .. } | Self::Ambiguous { account_key, .. } => {
                Some(account_key)
            }
            Self::Unknown => None,
        }
    }

    /// Whether this attribution is certain enough to report without a caveat.
    pub fn is_exact(&self) -> bool {
        matches!(self, Self::Exact { .. })
    }
}

impl DirectoryLedger {
    /// Note that `account_key` was signed in at `at`.
    ///
    /// Extends the current stretch when the account is unchanged, and opens a
    /// new one when it differs. Returns whether a new stretch opened, which is
    /// the only change worth writing to disk: an extended `last_seen` is
    /// reconstructed on the next observation after a restart, but a missed
    /// switch is not.
    pub fn observe(&mut self, account_key: &str, label: &str, at: i64) -> bool {
        if let Some(last) = self.sightings.last_mut()
            && last.account_key == account_key
        {
            // Clock changes and out-of-order observations must not rewind a
            // range; a stretch only ever grows forward.
            last.last_seen = last.last_seen.max(at);
            last.label = label.to_string();
            return false;
        }

        self.sightings.push(AccountSighting {
            account_key: account_key.to_string(),
            label: label.to_string(),
            first_seen: at,
            last_seen: at,
        });
        true
    }

    /// Which account this directory held at `at`.
    pub fn attribute(&self, at: i64) -> Attribution {
        let Some(first) = self.sightings.first() else {
            return Attribution::Unknown;
        };
        if at < first.first_seen {
            return Attribution::Unknown;
        }

        for (index, sighting) in self.sightings.iter().enumerate() {
            if at <= sighting.last_seen {
                return Attribution::Exact {
                    account_key: sighting.account_key.clone(),
                    label: sighting.label.clone(),
                };
            }
            // Past this stretch: either the gap before the next one, or the end.
            match self.sightings.get(index + 1) {
                Some(next) if at < next.first_seen => {
                    return Attribution::Ambiguous {
                        account_key: sighting.account_key.clone(),
                        label: sighting.label.clone(),
                        next_account_key: next.account_key.clone(),
                    };
                }
                Some(_) => continue,
                None => {
                    // After the last observation. The directory has not been seen
                    // to change since, so this is the account it still holds.
                    return Attribution::Exact {
                        account_key: sighting.account_key.clone(),
                        label: sighting.label.clone(),
                    };
                }
            }
        }

        Attribution::Unknown
    }

    /// Distinct accounts ever seen in this directory, most recent first.
    pub fn accounts(&self) -> Vec<(&str, &str)> {
        let mut seen = Vec::new();
        for sighting in self.sightings.iter().rev() {
            if !seen
                .iter()
                .any(|(key, _): &(&str, &str)| *key == sighting.account_key)
            {
                seen.push((sighting.account_key.as_str(), sighting.label.as_str()));
            }
        }
        seen
    }
}

/// Per-provider directory ledgers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountLedger {
    #[serde(default = "default_version")]
    pub version: u32,
    /// Keyed by provider CLI name so the file stays readable.
    #[serde(default)]
    pub providers: BTreeMap<String, Vec<DirectoryLedger>>,
}

fn default_version() -> u32 {
    1
}

impl AccountLedger {
    pub fn new() -> Self {
        Self::default()
    }

    fn directory_mut(&mut self, provider: ProviderId, config_dir: &Path) -> &mut DirectoryLedger {
        let entries = self
            .providers
            .entry(provider.cli_name().to_string())
            .or_default();
        if let Some(index) = entries
            .iter()
            .position(|entry| same_dir(&entry.config_dir, config_dir))
        {
            return &mut entries[index];
        }
        entries.push(DirectoryLedger {
            config_dir: config_dir.to_path_buf(),
            sightings: Vec::new(),
        });
        entries.last_mut().expect("just pushed")
    }

    /// The ledger for a directory, if it has ever been observed.
    pub fn directory(&self, provider: ProviderId, config_dir: &Path) -> Option<&DirectoryLedger> {
        self.providers
            .get(provider.cli_name())?
            .iter()
            .find(|entry| same_dir(&entry.config_dir, config_dir))
    }

    /// Which account `provider` had in `config_dir` at `at`.
    pub fn attribute(&self, provider: ProviderId, config_dir: &Path, at: i64) -> Attribution {
        self.directory(provider, config_dir)
            .map(|entry| entry.attribute(at))
            .unwrap_or(Attribution::Unknown)
    }

    /// Record the account currently signed into a directory.
    pub fn observe(
        &mut self,
        provider: ProviderId,
        config_dir: &Path,
        account_key: &str,
        label: &str,
        at: i64,
    ) -> bool {
        self.directory_mut(provider, config_dir)
            .observe(account_key, label, at)
    }

    /// Read every directory Ceiling knows about and record what it finds.
    ///
    /// A directory that is signed out is skipped rather than recorded as a
    /// change, so a logout does not close a stretch that a later login to the
    /// same account should have continued.
    /// Returns whether any switch was detected, i.e. whether this is worth
    /// persisting.
    pub fn observe_all(&mut self, accounts: &ConfiguredAccounts, at: i64) -> bool {
        let mut switched = false;
        for dir in accounts.codex.all_dirs() {
            if let Some(identity) = <CodexIdentity as super::AccountIdentity>::read(&dir)
                && let Some(key) = codex_account_key(&identity)
            {
                let label = <CodexIdentity as super::AccountIdentity>::suggested_label(&identity)
                    .unwrap_or_else(|| key.clone());
                switched |= self.observe(ProviderId::Codex, &dir, &key, &label, at);
            }
        }
        for dir in accounts.claude.all_dirs() {
            if let Some(identity) = <ClaudeIdentity as super::AccountIdentity>::read(&dir)
                && let Some(key) = claude_account_key(&identity)
            {
                let label = <ClaudeIdentity as super::AccountIdentity>::suggested_label(&identity)
                    .unwrap_or_else(|| key.clone());
                switched |= self.observe(ProviderId::Claude, &dir, &key, &label, at);
            }
        }
        switched
    }

    /// Observe every known directory and persist when a switch was detected.
    ///
    /// An existing file that will not decode is left untouched. `load_default`
    /// is fail-open for readers; using it here would treat DPAPI / parse / IO
    /// failure as an empty ledger and the following save would wipe every
    /// prior sighting (SBS-1074).
    pub fn record_and_persist(accounts: &ConfiguredAccounts, at: i64) {
        Self::persist_if_changed(&Self::default_path(), |ledger| {
            ledger.observe_all(accounts, at)
        });
    }

    /// Load `path`, apply `mutate`, and write only when it reports a change.
    /// A missing file is an empty ledger; an existing undecodable one is not
    /// replaced.
    pub(crate) fn persist_if_changed(path: &Path, mutate: impl FnOnce(&mut Self) -> bool) {
        let mut ledger = match Self::load_from(path) {
            Ok(ledger) => ledger,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "account-ledger.json could not be read; refusing to replace recorded sightings"
                );
                return;
            }
        };
        if !mutate(&mut ledger) {
            return;
        }
        if let Err(error) = ledger.save_to(path) {
            tracing::warn!("failed to persist account ledger: {error}");
        }
    }

    /// Default storage path, alongside the account stores.
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .map(|dir| dir.join("CodexBar"))
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".codexbar")
            })
            .join("account-ledger.json")
    }

    pub fn load_from(path: &Path) -> Result<Self, LedgerError> {
        if !path.exists() {
            return Ok(Self::new());
        }
        Ok(serde_json::from_str(&crate::secure_file::read_string(
            path,
        )?)?)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), LedgerError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        crate::secure_file::write_string(path, &serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Load from the default path, treating a corrupt file as empty rather than
    /// failing a refresh over attribution metadata.
    ///
    /// Read-only. Persist must go through [`Self::persist_if_changed`] so an
    /// undecodable file is not rewritten from this empty snapshot.
    pub fn load_default() -> Self {
        Self::load_from(&Self::default_path()).unwrap_or_else(|error| {
            tracing::warn!("failed to load account ledger: {error}");
            Self::new()
        })
    }
}

/// Stable key for a Codex account: the ChatGPT account id, else the email.
pub fn codex_account_key(identity: &CodexIdentity) -> Option<String> {
    identity
        .account_id
        .clone()
        .or_else(|| identity.email.clone())
}

/// Stable key for a Claude account: the account uuid, else the email.
pub fn claude_account_key(identity: &ClaudeIdentity) -> Option<String> {
    identity
        .account_uuid
        .clone()
        .or_else(|| identity.email.clone())
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger_with(observations: &[(&str, i64)]) -> DirectoryLedger {
        let mut ledger = DirectoryLedger::default();
        for (account, at) in observations {
            ledger.observe(account, account, *at);
        }
        ledger
    }

    #[test]
    fn a_timestamp_before_any_observation_is_unattributable() {
        let ledger = ledger_with(&[("personal", 1_000)]);

        // The honest answer for history written before Ceiling watched.
        assert_eq!(ledger.attribute(500), Attribution::Unknown);
        assert_eq!(
            DirectoryLedger::default().attribute(500),
            Attribution::Unknown
        );
    }

    #[test]
    fn repeated_observations_of_one_account_extend_a_single_stretch() {
        let ledger = ledger_with(&[("personal", 100), ("personal", 200), ("personal", 300)]);

        assert_eq!(ledger.sightings.len(), 1);
        assert_eq!(ledger.sightings[0].first_seen, 100);
        assert_eq!(ledger.sightings[0].last_seen, 300);
        assert!(ledger.attribute(250).is_exact());
        assert_eq!(ledger.attribute(250).account_key(), Some("personal"));
    }

    #[test]
    fn a_switch_opens_a_new_stretch_and_attributes_each_side() {
        let ledger = ledger_with(&[("personal", 100), ("personal", 200), ("work", 300)]);

        assert_eq!(ledger.sightings.len(), 2);
        assert_eq!(ledger.attribute(150).account_key(), Some("personal"));
        assert_eq!(ledger.attribute(300).account_key(), Some("work"));
        assert!(ledger.attribute(150).is_exact());
        assert!(ledger.attribute(300).is_exact());
    }

    #[test]
    fn the_gap_around_a_switch_is_reported_as_ambiguous_not_guessed() {
        let ledger = ledger_with(&[("personal", 100), ("personal", 200), ("work", 300)]);

        // The switch happened somewhere in (200, 300]. Claiming either side
        // would be a guess, so say so while still naming the likelier one.
        let attribution = ledger.attribute(250);

        assert_eq!(
            attribution,
            Attribution::Ambiguous {
                account_key: "personal".to_string(),
                label: "personal".to_string(),
                next_account_key: "work".to_string(),
            }
        );
        assert!(!attribution.is_exact());
        assert_eq!(attribution.account_key(), Some("personal"));
    }

    #[test]
    fn after_the_last_observation_the_directory_still_holds_that_account() {
        let ledger = ledger_with(&[("work", 100)]);

        // A record written a minute ago, before the next refresh notices.
        assert!(ledger.attribute(10_000).is_exact());
        assert_eq!(ledger.attribute(10_000).account_key(), Some("work"));
    }

    #[test]
    fn switching_back_records_a_third_stretch_rather_than_widening_the_first() {
        let ledger = ledger_with(&[("personal", 100), ("work", 200), ("personal", 300)]);

        assert_eq!(ledger.sightings.len(), 3);
        assert_eq!(ledger.attribute(200).account_key(), Some("work"));
        // Work's stretch must not swallow the later personal one.
        assert_eq!(ledger.attribute(300).account_key(), Some("personal"));
        assert_eq!(ledger.accounts().len(), 2);
    }

    #[test]
    fn an_out_of_order_observation_does_not_rewind_a_stretch() {
        let mut ledger = ledger_with(&[("personal", 500)]);

        // A clock change or a late write must not shrink the range.
        ledger.observe("personal", "personal", 100);

        assert_eq!(ledger.sightings[0].first_seen, 500);
        assert_eq!(ledger.sightings[0].last_seen, 500);
    }

    #[test]
    fn directories_are_tracked_independently_per_provider() {
        let mut ledger = AccountLedger::new();
        let personal = PathBuf::from("/dirs/personal");
        let work = PathBuf::from("/dirs/work");

        ledger.observe(ProviderId::Codex, &personal, "acct-a", "a", 100);
        ledger.observe(ProviderId::Codex, &work, "acct-b", "b", 100);
        ledger.observe(ProviderId::Claude, &personal, "acct-c", "c", 100);

        assert_eq!(
            ledger
                .attribute(ProviderId::Codex, &personal, 100)
                .account_key(),
            Some("acct-a")
        );
        assert_eq!(
            ledger
                .attribute(ProviderId::Codex, &work, 100)
                .account_key(),
            Some("acct-b")
        );
        // Same directory, different provider: must not collide.
        assert_eq!(
            ledger
                .attribute(ProviderId::Claude, &personal, 100)
                .account_key(),
            Some("acct-c")
        );
        assert_eq!(
            ledger.attribute(ProviderId::Gemini, &personal, 100),
            Attribution::Unknown
        );
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("account-ledger.json");
        assert_eq!(
            AccountLedger::load_from(&path)
                .expect("empty")
                .providers
                .len(),
            0
        );

        let mut ledger = AccountLedger::new();
        ledger.observe(ProviderId::Codex, Path::new("/dirs/a"), "acct-a", "a", 100);
        ledger.observe(ProviderId::Codex, Path::new("/dirs/a"), "acct-b", "b", 200);
        ledger.save_to(&path).expect("save");

        let loaded = AccountLedger::load_from(&path).expect("load");
        let entry = loaded
            .directory(ProviderId::Codex, Path::new("/dirs/a"))
            .expect("directory");
        assert_eq!(entry.sightings.len(), 2);
        assert_eq!(
            loaded
                .attribute(ProviderId::Codex, Path::new("/dirs/a"), 200)
                .account_key(),
            Some("acct-b")
        );
    }

    /// SBS-1074: `record_and_persist` used `load_default`, which treats an
    /// undecodable file as empty. The next switch observation then replaced
    /// the live ledger with only the new sighting.
    #[test]
    fn persist_does_not_replace_an_undecodable_ledger() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("account-ledger.json");
        let mut ledger = AccountLedger::new();
        ledger.observe(ProviderId::Codex, Path::new("/dirs/a"), "acct-a", "a", 100);
        ledger.save_to(&path).expect("seed");

        let corrupt = "{not-valid-json";
        std::fs::write(&path, corrupt).expect("corrupt ledger");

        AccountLedger::persist_if_changed(&path, |ledger| {
            ledger.observe(ProviderId::Codex, Path::new("/dirs/a"), "acct-b", "b", 200)
        });

        assert_eq!(
            std::fs::read_to_string(&path).expect("live"),
            corrupt,
            "an undecodable ledger must stay so prior sightings are not overwritten"
        );
        assert!(
            AccountLedger::load_from(&path).is_err(),
            "writers must not see a corrupt ledger as an empty store"
        );
    }

    #[test]
    fn persist_does_not_replace_an_unreadable_ledger() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("account-ledger.json");
        let original = serde_json::json!({
            "format": "codexbar.secure-file",
            "version": 99,
            "protection": "windows-dpapi-user",
            "payload": "AAAA",
        })
        .to_string();
        std::fs::write(&path, &original).expect("write unsupported ProtectedFile");
        crate::secure_file::read_string(&path).expect_err("fixture must fail read_string");

        AccountLedger::persist_if_changed(&path, |ledger| {
            ledger.observe(ProviderId::Codex, Path::new("/dirs/a"), "acct-b", "b", 200)
        });

        assert_eq!(
            std::fs::read_to_string(&path).expect("live"),
            original,
            "a ProtectedFile this build cannot read must not be replaced"
        );
    }

    #[test]
    fn missing_ledger_can_still_be_persisted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("account-ledger.json");

        AccountLedger::persist_if_changed(&path, |ledger| {
            ledger.observe(ProviderId::Codex, Path::new("/dirs/a"), "acct-a", "a", 100)
        });

        let loaded = AccountLedger::load_from(&path).expect("first persist");
        assert_eq!(
            loaded
                .attribute(ProviderId::Codex, Path::new("/dirs/a"), 100)
                .account_key(),
            Some("acct-a")
        );
    }

    #[test]
    fn account_keys_prefer_the_stable_id_over_the_email() {
        let with_id = CodexIdentity {
            email: Some("person@example.com".into()),
            account_id: Some("acct-uuid".into()),
            plan_type: None,
        };
        let without_id = CodexIdentity {
            email: Some("person@example.com".into()),
            account_id: None,
            plan_type: None,
        };

        // An email can change; the account behind it does not.
        assert_eq!(codex_account_key(&with_id).as_deref(), Some("acct-uuid"));
        assert_eq!(
            codex_account_key(&without_id).as_deref(),
            Some("person@example.com")
        );
        assert_eq!(
            codex_account_key(&CodexIdentity {
                email: None,
                account_id: None,
                plan_type: Some("pro".into()),
            }),
            None,
            "a plan bucket is not an account"
        );
    }
}
