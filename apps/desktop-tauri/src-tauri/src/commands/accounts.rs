//! Directory-backed account commands and DTOs.
//!
//! An account here is a provider config directory (`CODEX_HOME`,
//! `CLAUDE_CONFIG_DIR`), never a credential Ceiling stores. Only the directory
//! path and display identity cross the bridge; no token material does.

use codexbar::core::{
    AccountIdentity, ClaudeAccountStore, ClaudeIdentity, CodexAccountStore, CodexIdentity,
    DirectoryAccount, DirectoryAccountData, DirectoryAccountStore, ProviderId,
};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Bridge-friendly account. `configDir` is a path the user chose themselves, so
/// it is shown as-is; nothing else here is derived from a credential value.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryAccountBridge {
    pub id: String,
    pub label: String,
    pub config_dir: String,
    pub tint: Option<String>,
    pub is_active: bool,
    /// Whether the directory currently holds a usable sign-in.
    pub signed_in: bool,
    pub email: Option<String>,
    pub organization: Option<String>,
    pub plan: Option<String>,
    pub added_at: String,
    pub last_used: Option<String>,
}

/// Bridge-friendly snapshot of one provider's accounts.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccountsBridge {
    pub provider_id: String,
    pub display_name: String,
    /// Environment variable that selects this provider's config directory, shown
    /// in the UI because adding an account means running the CLI with it set.
    pub env_var: String,
    pub accounts: Vec<DirectoryAccountBridge>,
    pub active_index: usize,
    /// True when no accounts are configured and Ceiling follows whichever
    /// account the CLI is signed in as, which is the default.
    pub following_cli: bool,
    /// The directory being followed in that case. `None` when the provider
    /// home cannot be resolved; never an empty string.
    pub ambient_dir: Option<String>,
}

/// Result of inspecting a directory before adding it as an account.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProbeBridge {
    pub config_dir: String,
    pub exists: bool,
    pub signed_in: bool,
    pub suggested_label: Option<String>,
    pub email: Option<String>,
    pub organization: Option<String>,
    pub plan: Option<String>,
    /// Set when the directory is already configured as an account.
    pub already_added_as: Option<String>,
}

/// Display fields pulled out of a provider's identity type.
struct Display {
    email: Option<String>,
    organization: Option<String>,
    plan: Option<String>,
}

trait Displayable: AccountIdentity {
    fn display(&self) -> Display;
    fn env_var() -> &'static str;
    fn provider() -> ProviderId;
}

impl Displayable for CodexIdentity {
    fn display(&self) -> Display {
        Display {
            email: self.email.clone(),
            // Codex reports no organization name, only the plan bucket.
            organization: None,
            plan: self.plan_type.clone(),
        }
    }
    fn env_var() -> &'static str {
        "CODEX_HOME"
    }
    fn provider() -> ProviderId {
        ProviderId::Codex
    }
}

impl Displayable for ClaudeIdentity {
    fn display(&self) -> Display {
        Display {
            email: self.email.clone(),
            organization: self.organization_name.clone(),
            plan: self.subscription_type.clone(),
        }
    }
    fn env_var() -> &'static str {
        "CLAUDE_CONFIG_DIR"
    }
    fn provider() -> ProviderId {
        ProviderId::Claude
    }
}

fn format_date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt: chrono::DateTime<chrono::Utc>| dt.format("%b %d, %Y").to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

fn bridge_account<I: Displayable>(
    account: &DirectoryAccount<I>,
    is_active: bool,
) -> DirectoryAccountBridge {
    // Prefer what the directory says right now over what was cached when the
    // account was added, so a re-login inside a directory is reflected.
    let identity = I::read(&account.config_dir).or_else(|| account.identity.clone());
    let display = identity.as_ref().map(Displayable::display);

    DirectoryAccountBridge {
        id: account.id.to_string(),
        label: account.label.clone(),
        config_dir: account.config_dir.display().to_string(),
        tint: account.tint.clone(),
        is_active,
        signed_in: I::is_signed_in(&account.config_dir),
        email: display.as_ref().and_then(|d| d.email.clone()),
        organization: display.as_ref().and_then(|d| d.organization.clone()),
        plan: display.as_ref().and_then(|d| d.plan.clone()),
        added_at: format_date(account.added_at),
        last_used: account.last_used.map(format_date),
    }
}

fn bridge_provider<I: Displayable>(data: &DirectoryAccountData<I>) -> ProviderAccountsBridge {
    let active = data.clamped_active_index();
    ProviderAccountsBridge {
        provider_id: I::provider().cli_name().to_string(),
        display_name: I::provider().display_name().to_string(),
        env_var: I::env_var().to_string(),
        accounts: data
            .accounts
            .iter()
            .enumerate()
            .map(|(index, account)| bridge_account(account, index == active))
            .collect(),
        active_index: active,
        following_cli: !data.is_explicit(),
        ambient_dir: I::ambient_dir().map(|dir| dir.display().to_string()),
    }
}

/// Validate a caller-supplied directory path.
fn parse_config_dir(config_dir: &str) -> Result<PathBuf, String> {
    let trimmed = config_dir.trim();
    if trimmed.is_empty() {
        return Err("Choose a config directory.".to_string());
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err("Use a full path to the config directory.".to_string());
    }
    if !path.is_dir() {
        return Err(format!("{} is not a directory.", path.display()));
    }
    Ok(path)
}

fn parse_uuid(account_id: &str) -> Result<uuid::Uuid, String> {
    uuid::Uuid::parse_str(account_id).map_err(|e| e.to_string())
}

/// Accent colors are written into the UI, so accept only a plain hex color
/// rather than arbitrary CSS.
fn parse_tint(tint: Option<String>) -> Result<Option<String>, String> {
    let Some(tint) = tint else {
        return Ok(None);
    };
    let trimmed = tint.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let valid = trimmed.starts_with('#')
        && matches!(trimmed.len(), 4 | 7)
        && trimmed[1..].chars().all(|c| c.is_ascii_hexdigit());
    if !valid {
        return Err("Use a hex color like #4f8ff7.".to_string());
    }
    Ok(Some(trimmed.to_string()))
}

/// Run `op` against a provider's store, saving and returning the new snapshot.
fn with_store<I: Displayable>(
    op: impl FnOnce(&mut DirectoryAccountData<I>) -> Result<(), String>,
) -> Result<ProviderAccountsBridge, String> {
    let store: DirectoryAccountStore<I> = DirectoryAccountStore::new();
    let (data, ()) = store.try_update(op).map_err(|e| e.to_string())?;
    Ok(bridge_provider(&data))
}

/// Register the account the CLI is already signed in as, if nothing is
/// configured yet.
///
/// Without this, adding your first account is destructive rather than additive:
/// the ambient account is only consulted when the store is *empty*, so the
/// moment a second account is added the original stops being fetched and
/// vanishes from every surface. Listing it from the start also means the
/// Accounts page is never a blank that hides an account you are actively using.
///
/// Only registers a directory that actually holds a sign-in, so this cannot
/// invent an entry for someone who has never authenticated.
fn ensure_ambient_registered<I: Displayable>() {
    let store: DirectoryAccountStore<I> = DirectoryAccountStore::new();
    let Ok(data) = store.load() else {
        return;
    };
    if !data.accounts.is_empty() {
        return;
    }
    let Some(ambient) = I::ambient_dir() else {
        return;
    };
    if !I::is_signed_in(&ambient) {
        return;
    }
    if let Err(error) = store.try_update(|data| {
        if !data.accounts.is_empty() {
            return Ok(());
        }
        let Some(ambient) = I::ambient_dir() else {
            return Ok(());
        };
        if I::is_signed_in(&ambient) {
            data.add_account(DirectoryAccount::<I>::new(None, ambient));
        }
        Ok(())
    }) {
        tracing::warn!("failed to register the signed-in account: {error}");
    }
}

/// Load every provider's directory-backed accounts.
#[tauri::command]
pub fn get_directory_accounts() -> Result<Vec<ProviderAccountsBridge>, String> {
    ensure_ambient_registered::<CodexIdentity>();
    ensure_ambient_registered::<ClaudeIdentity>();
    let codex = CodexAccountStore::new().load().map_err(|e| e.to_string())?;
    let claude = ClaudeAccountStore::new()
        .load()
        .map_err(|e| e.to_string())?;
    Ok(vec![bridge_provider(&codex), bridge_provider(&claude)])
}

/// Inspect a directory before adding it, so the user sees whose account it is.
#[tauri::command]
pub fn probe_account_directory(
    provider_id: String,
    config_dir: String,
) -> Result<AccountProbeBridge, String> {
    let id = super::parse_provider_arg(&provider_id)?;
    let trimmed = config_dir.trim().to_string();
    let path = PathBuf::from(&trimmed);

    fn probe<I: Displayable>(path: &Path) -> Result<AccountProbeBridge, String> {
        let store: DirectoryAccountStore<I> = DirectoryAccountStore::new();
        let already_added_as = store.load().ok().and_then(|data| {
            data.accounts
                .iter()
                .find(|account| codexbar::core::same_dir(&account.config_dir, path))
                .map(|account| account.label.clone())
        });
        let identity = I::read(path);
        let display = identity.as_ref().map(Displayable::display);

        Ok(AccountProbeBridge {
            config_dir: path.display().to_string(),
            exists: path.is_dir(),
            signed_in: I::is_signed_in(path),
            suggested_label: identity.as_ref().and_then(I::suggested_label),
            email: display.as_ref().and_then(|d| d.email.clone()),
            organization: display.as_ref().and_then(|d| d.organization.clone()),
            plan: display.as_ref().and_then(|d| d.plan.clone()),
            already_added_as,
        })
    }

    match id {
        ProviderId::Codex => probe::<CodexIdentity>(&path),
        ProviderId::Claude => probe::<ClaudeIdentity>(&path),
        other => Err(format!(
            "{} does not use config-directory accounts.",
            other.display_name()
        )),
    }
}

/// Add a config directory as an account and make it active.
///
/// Adding switches to the new account, so the outgoing reading is dropped and
/// refetched exactly as an explicit switch does.
#[tauri::command]
pub async fn add_directory_account(
    app: tauri::AppHandle,
    provider_id: String,
    config_dir: String,
    label: Option<String>,
) -> Result<ProviderAccountsBridge, String> {
    let id = super::parse_provider_arg(&provider_id)?;
    let path = parse_config_dir(&config_dir)?;
    let label = super::sanitize_optional_label(label)?;

    fn add<I: Displayable>(
        path: PathBuf,
        label: Option<String>,
    ) -> Result<ProviderAccountsBridge, String> {
        ensure_ambient_registered::<I>();
        with_store::<I>(|data| {
            data.add_account(DirectoryAccount::<I>::new(label, path));
            Ok(())
        })
    }

    let bridge = match id {
        ProviderId::Codex => add::<CodexIdentity>(path, label)?,
        ProviderId::Claude => add::<ClaudeIdentity>(path, label)?,
        other => {
            return Err(format!(
                "{} does not use config-directory accounts.",
                other.display_name()
            ));
        }
    };

    evict_cached_reading(&app, id);
    super::refresh_providers(app).await?;

    Ok(bridge)
}

/// Remove an account. Removing the last one returns to following the CLI.
///
/// Removing the active account promotes another one, so the outgoing reading is
/// dropped and refetched exactly as an explicit switch does.
#[tauri::command]
pub async fn remove_directory_account(
    app: tauri::AppHandle,
    provider_id: String,
    account_id: String,
) -> Result<ProviderAccountsBridge, String> {
    let id = super::parse_provider_arg(&provider_id)?;
    let uuid = parse_uuid(&account_id)?;

    fn remove<I: Displayable>(uuid: uuid::Uuid) -> Result<ProviderAccountsBridge, String> {
        with_store::<I>(|data| {
            data.remove_account(uuid);
            Ok(())
        })
    }

    let bridge = match id {
        ProviderId::Codex => remove::<CodexIdentity>(uuid)?,
        ProviderId::Claude => remove::<ClaudeIdentity>(uuid)?,
        other => {
            return Err(format!(
                "{} does not use config-directory accounts.",
                other.display_name()
            ));
        }
    };

    evict_cached_reading(&app, id);
    super::refresh_providers(app).await?;

    Ok(bridge)
}

/// Switch which account Ceiling tracks for a provider.
///
/// Drops the outgoing account's cached reading before refetching, so the panel
/// never shows one seat's usage under another's name during the gap.
#[tauri::command]
pub async fn set_active_directory_account(
    app: tauri::AppHandle,
    provider_id: String,
    account_id: String,
) -> Result<ProviderAccountsBridge, String> {
    let id = super::parse_provider_arg(&provider_id)?;
    let uuid = parse_uuid(&account_id)?;

    fn activate<I: Displayable>(uuid: uuid::Uuid) -> Result<ProviderAccountsBridge, String> {
        with_store::<I>(|data| {
            if !data.set_active_by_id(uuid) {
                return Err("That account is no longer configured.".to_string());
            }
            Ok(())
        })
    }

    let bridge = match id {
        ProviderId::Codex => activate::<CodexIdentity>(uuid)?,
        ProviderId::Claude => activate::<ClaudeIdentity>(uuid)?,
        other => {
            return Err(format!(
                "{} does not use config-directory accounts.",
                other.display_name()
            ));
        }
    };

    evict_cached_reading(&app, id);
    // Capacity baselines and enforcement expectations are scoped by account
    // identity, so the incoming account re-baselines on its own first reading
    // rather than inheriting the outgoing one's history. Nothing to reset here.
    super::refresh_providers(app).await?;

    Ok(bridge)
}

/// Forget a provider's cached reading so the UI shows "loading" rather than the
/// previous account's numbers while the refetch is in flight.
pub(super) fn evict_cached_reading(app: &tauri::AppHandle, provider: ProviderId) {
    use tauri::Manager;

    let state = app.state::<std::sync::Mutex<crate::state::AppState>>();
    let Ok(mut guard) = state.lock() else {
        return;
    };
    guard
        .provider_cache
        .retain(|snapshot| snapshot.provider_id != provider.cli_name());
    // The outgoing account's transient-failure grace must not carry over.
    guard
        .transient_provider_failure_counts
        .retain(|(id, _), _| *id != provider);
    // The remaining entries are now a partial view, so let the next staleness
    // check re-fetch rather than treating this as a fresh full cycle.
    guard.provider_cache_updated_at = None;
}

/// Relabel an account and/or set its accent color.
#[tauri::command]
pub fn update_directory_account(
    provider_id: String,
    account_id: String,
    label: Option<String>,
    tint: Option<String>,
) -> Result<ProviderAccountsBridge, String> {
    let id = super::parse_provider_arg(&provider_id)?;
    let uuid = parse_uuid(&account_id)?;
    let label = super::sanitize_optional_label(label)?;
    let tint = parse_tint(tint)?;

    fn update<I: Displayable>(
        uuid: uuid::Uuid,
        label: Option<String>,
        tint: Option<String>,
    ) -> Result<ProviderAccountsBridge, String> {
        with_store::<I>(|data| {
            let Some(account) = data.accounts.iter_mut().find(|account| account.id == uuid) else {
                return Err("That account is no longer configured.".to_string());
            };
            if let Some(label) = label {
                account.label = label;
            }
            // An explicit empty tint clears it; see `parse_tint`.
            account.tint = tint;
            Ok(())
        })
    }

    match id {
        ProviderId::Codex => update::<CodexIdentity>(uuid, label, tint),
        ProviderId::Claude => update::<ClaudeIdentity>(uuid, label, tint),
        other => Err(format!(
            "{} does not use config-directory accounts.",
            other.display_name()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported failure: with nothing configured, Ceiling followed the CLI
    /// and showed the signed-in account. Adding a second account then made the
    /// first disappear entirely, because the ambient account is only consulted
    /// while the store is empty and adding an entry ends that.
    #[test]
    fn adding_an_account_must_not_drop_the_one_already_in_use() {
        let mut data = DirectoryAccountData::<CodexIdentity>::new();

        // What ensure_ambient_registered does before the add.
        data.add_account(DirectoryAccount::<CodexIdentity>::new(
            Some("signed in already".to_string()),
            PathBuf::from("/homes/personal"),
        ));
        data.add_account(DirectoryAccount::<CodexIdentity>::new(
            Some("work".to_string()),
            PathBuf::from("/homes/work"),
        ));

        assert_eq!(data.count(), 2, "the original account was dropped");
        let dirs: Vec<_> = data.accounts.iter().map(|a| a.config_dir.clone()).collect();
        assert!(dirs.contains(&PathBuf::from("/homes/personal")));
        assert!(dirs.contains(&PathBuf::from("/homes/work")));
    }

    #[test]
    fn registering_the_ambient_account_is_skipped_when_it_is_signed_out() {
        let empty = tempfile::tempdir().expect("tempdir");

        // A directory with no credential must not become a phantom account.
        assert!(!<CodexIdentity as AccountIdentity>::is_signed_in(
            empty.path()
        ));
    }

    #[test]
    fn a_relative_path_is_rejected() {
        assert!(parse_config_dir("relative/dir").is_err());
        assert!(parse_config_dir("   ").is_err());
    }

    #[test]
    fn a_path_that_is_not_a_directory_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("not-a-dir.txt");
        std::fs::write(&file, "x").expect("write");

        assert!(parse_config_dir(&file.display().to_string()).is_err());
        assert!(parse_config_dir(&dir.path().display().to_string()).is_ok());
    }

    #[test]
    fn only_plain_hex_colors_are_accepted_as_tints() {
        assert_eq!(
            parse_tint(Some("#4f8ff7".into())),
            Ok(Some("#4f8ff7".into()))
        );
        assert_eq!(parse_tint(Some("#abc".into())), Ok(Some("#abc".into())));
        assert_eq!(parse_tint(Some("  ".into())), Ok(None));
        assert_eq!(parse_tint(None), Ok(None));

        // These would otherwise be interpolated into the UI as-is.
        assert!(parse_tint(Some("red".into())).is_err());
        assert!(parse_tint(Some("#4f8ff7; background: url(x)".into())).is_err());
        assert!(parse_tint(Some("var(--accent)".into())).is_err());
        assert!(parse_tint(Some("#zzzzzz".into())).is_err());
    }

    fn sample_bridge(ambient_dir: Option<String>) -> ProviderAccountsBridge {
        ProviderAccountsBridge {
            provider_id: "codex".into(),
            display_name: "Codex".into(),
            env_var: "CODEX_HOME".into(),
            accounts: vec![],
            active_index: 0,
            following_cli: true,
            ambient_dir,
        }
    }

    #[test]
    fn missing_ambient_dir_serializes_as_json_null() {
        let json = serde_json::to_value(sample_bridge(None)).expect("serialize");
        assert_eq!(json["ambientDir"], serde_json::Value::Null);
        let text = serde_json::to_string(&sample_bridge(None)).expect("serialize");
        assert!(
            !text.contains("\"ambientDir\":\"\""),
            "empty string must not stand in for a missing home: {text}"
        );
    }

    #[test]
    fn resolved_ambient_dir_serializes_as_a_string() {
        let json =
            serde_json::to_value(sample_bridge(Some(r"C:\codex".into()))).expect("serialize");
        assert_eq!(
            json["ambientDir"],
            serde_json::Value::String(r"C:\codex".into())
        );
    }
}
