//! Claude Desktop's own sign-in, read from its `config.json`.
//!
//! Desktop keeps OAuth tokens there, encrypted with the same key as its cookies.
//! Unlike its cookie database, the file is not locked while the app runs, so this
//! works with Desktop open or closed and needs no Claude Code.
//!
//! The token is Desktop's. Ceiling only reads it, holds it in memory for one
//! fetch and never refreshes it, because rotating it would sign Desktop out.

use base64::Engine;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use std::path::Path;

use super::oauth::ClaudeOAuthCredentials;
use super::web_api::claude_desktop_data_dirs;
use crate::browser::cookies::CookieExtractor;

/// Newest format first. Desktop writes both.
const CONFIG_KEYS: [&str; 2] = ["oauth:tokenCacheV2", "oauth:tokenCache"];
const ACTIVE_ACCOUNT_KEY: &str = "lastKnownAccountUuid";
/// The usage endpoint answers 403 to Desktop's `user:inference`-only token and
/// requires `user:profile`. Its token with both is the one proven to work.
const REQUIRED_SCOPES: [&str; 2] = ["user:inference", "user:profile"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheEntry {
    token: Option<String>,
    expires_at: Option<i64>,
    rate_limit_tier: Option<String>,
    subscription_type: Option<String>,
}

/// Desktop's token and the account it belongs to.
pub(super) struct DesktopToken {
    pub(super) credentials: ClaudeOAuthCredentials,
    pub(super) account_uuid: String,
}

/// Desktop's current usable token, if it has one on this machine.
pub(super) fn load_desktop_token() -> Option<DesktopToken> {
    if !cfg!(windows) {
        return None;
    }
    let now = Utc::now();
    claude_desktop_data_dirs()
        .into_iter()
        .find_map(|dir| load_from_profile(&dir, now))
}

/// Whether Desktop has saved a sign-in here, usable or not.
pub(super) fn has_saved_sign_in() -> bool {
    cfg!(windows)
        && claude_desktop_data_dirs()
            .iter()
            .filter_map(|dir| read_config(dir))
            .any(|config| CONFIG_KEYS.iter().any(|key| config.get(*key).is_some()))
}

fn read_config(dir: &Path) -> Option<serde_json::Value> {
    let config = std::fs::read_to_string(dir.join("config.json")).ok()?;
    serde_json::from_str(&config).ok()
}

fn load_from_profile(dir: &Path, now: DateTime<Utc>) -> Option<DesktopToken> {
    let config = read_config(dir)?;
    let account = config.get(ACTIVE_ACCOUNT_KEY)?.as_str()?;
    let local_state = dir.join("Local State");

    let credentials = CONFIG_KEYS.iter().find_map(|key| {
        let encoded = config.get(*key)?.as_str()?;
        let encrypted = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let plain = CookieExtractor::decrypt_chromium_secret(&local_state, &encrypted)
            .map_err(|error| tracing::debug!("Claude Desktop token unavailable: {error}"))
            .ok()?;
        let cache: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&plain).ok()?;
        select_credentials(&cache, account, now)
    })?;
    Some(DesktopToken {
        credentials,
        account_uuid: account.to_string(),
    })
}

/// Pick the signed-in account's longest-lived token with the scopes usage needs.
fn select_credentials(
    cache: &serde_json::Map<String, serde_json::Value>,
    active_account: &str,
    now: DateTime<Utc>,
) -> Option<ClaudeOAuthCredentials> {
    cache
        .iter()
        .filter(|(key, _)| has_required_scopes(key) && belongs_to_account(key, active_account))
        .filter_map(|(key, value)| {
            let entry: CacheEntry = serde_json::from_value(value.clone()).ok()?;
            let token = entry.token.as_deref()?.trim();
            if token.is_empty() {
                return None;
            }
            let credentials = ClaudeOAuthCredentials {
                access_token: token.to_string(),
                refresh_token: None,
                expires_at: Some(Utc.timestamp_millis_opt(entry.expires_at?).single()?),
                scopes: scopes_of(key).map(str::to_string).collect(),
                rate_limit_tier: entry.rate_limit_tier,
                subscription_type: entry.subscription_type,
            };
            (!credentials.is_expired_at(now)).then_some(credentials)
        })
        .max_by_key(|credentials| credentials.expires_at)
}

/// Keys look like `acct:<account>|<org>:<client>:https://<host>:<scopes>`.
fn scopes_of(key: &str) -> impl Iterator<Item = &str> {
    key.split_once("https://")
        .and_then(|(_, rest)| rest.split_once(':'))
        .map_or("", |(_, scopes)| scopes)
        .split_whitespace()
}

fn has_required_scopes(key: &str) -> bool {
    REQUIRED_SCOPES
        .iter()
        .all(|required| scopes_of(key).any(|scope| scope == *required))
}

/// A token left behind by an account Desktop has since switched away from, or
/// one that names no account, must not be read as the current one.
fn belongs_to_account(key: &str, active_account: &str) -> bool {
    key.strip_prefix("acct:")
        .and_then(|rest| rest.split('|').next())
        .is_some_and(|account| account == active_account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ACCOUNT: &str = "11111111-1111-1111-1111-111111111111";
    const OTHER: &str = "22222222-2222-2222-2222-222222222222";

    fn now() -> DateTime<Utc> {
        Utc.timestamp_millis_opt(1_800_000_000_000).unwrap()
    }

    fn key(account: &str, scopes: &str) -> String {
        format!("acct:{account}|org:client:https://api.anthropic.com:{scopes}")
    }

    fn entry(token: &str, expires_in_ms: i64) -> serde_json::Value {
        json!({
            "token": token,
            "refreshToken": "must-not-be-used",
            "expiresAt": now().timestamp_millis() + expires_in_ms,
            "subscriptionType": "max",
            "rateLimitTier": "default_claude_max_5x",
        })
    }

    fn cache(
        items: Vec<(String, serde_json::Value)>,
    ) -> serde_json::Map<String, serde_json::Value> {
        items.into_iter().collect()
    }

    #[test]
    fn picks_the_entry_usage_accepts() {
        // Shapes from a real Desktop install: a null entry, a profile-only token,
        // the working one, and an inference-only token the API answers 403 to.
        let cache = cache(vec![
            (
                key(
                    ACCOUNT,
                    "user:inference user:file_upload user:profile user:sessions:claude_code",
                ),
                serde_json::Value::Null,
            ),
            (
                key(ACCOUNT, "user:profile"),
                entry("profile-only", 9_000_000),
            ),
            (
                key(ACCOUNT, "user:inference user:file_upload user:profile"),
                entry("usable", 9_000_000),
            ),
            (
                key(ACCOUNT, "user:inference"),
                entry("inference-only", 9_000_000),
            ),
        ]);

        let credentials = select_credentials(&cache, ACCOUNT, now()).unwrap();

        assert_eq!(credentials.access_token, "usable");
        assert!(credentials.has_scope("user:profile"));
        assert_eq!(credentials.subscription_type.as_deref(), Some("max"));
        assert_eq!(
            credentials.rate_limit_tier.as_deref(),
            Some("default_claude_max_5x")
        );
    }

    #[test]
    fn never_carries_the_refresh_token() {
        let cache = cache(vec![(
            key(ACCOUNT, "user:inference user:profile"),
            entry("usable", 9_000_000),
        )]);

        let credentials = select_credentials(&cache, ACCOUNT, now()).unwrap();

        assert!(credentials.refresh_token.is_none());
    }

    #[test]
    fn prefers_the_longest_lived_token() {
        let cache = cache(vec![
            (
                key(ACCOUNT, "user:inference user:profile"),
                entry("short", 9_000_000),
            ),
            (
                key(ACCOUNT, "user:inference user:file_upload user:profile"),
                entry("long", 90_000_000),
            ),
        ]);

        let credentials = select_credentials(&cache, ACCOUNT, now()).unwrap();

        assert_eq!(credentials.access_token, "long");
    }

    #[test]
    fn skips_tokens_the_usage_fetch_would_treat_as_expired() {
        // `fetch_usage` refuses a token within five minutes of expiry, so one in
        // that window must not be selected and then fail as "sign in again".
        const FIVE_MINUTES_MS: i64 = 5 * 60 * 1000;
        for expires_in_ms in [-1_000, 0, 60_000, FIVE_MINUTES_MS] {
            let cache = cache(vec![(
                key(ACCOUNT, "user:inference user:profile"),
                entry("stale", expires_in_ms),
            )]);
            assert!(select_credentials(&cache, ACCOUNT, now()).is_none());
        }

        let cache = cache(vec![(
            key(ACCOUNT, "user:inference user:profile"),
            entry("fresh", FIVE_MINUTES_MS + 1_000),
        )]);
        assert!(select_credentials(&cache, ACCOUNT, now()).is_some());
    }

    #[test]
    fn ignores_an_account_desktop_has_switched_away_from() {
        let cache = cache(vec![(
            key(OTHER, "user:inference user:profile"),
            entry("old-account", 9_000_000),
        )]);

        assert!(select_credentials(&cache, ACCOUNT, now()).is_none());
    }

    #[test]
    fn ignores_a_token_that_names_no_account() {
        let cache = cache(vec![(
            "org:client:https://api.anthropic.com:user:inference user:profile".to_string(),
            entry("unattributed", 9_000_000),
        )]);

        assert!(select_credentials(&cache, ACCOUNT, now()).is_none());
    }

    #[test]
    fn scope_matching_is_exact() {
        assert!(!has_required_scopes(&key(
            ACCOUNT,
            "user:inference_extra user:profile_extra"
        )));
        assert!(!has_required_scopes("acct:no-host-or-scopes"));
    }

    #[test]
    fn tolerates_malformed_entries() {
        let cache = cache(vec![
            (key(ACCOUNT, "user:inference user:profile"), json!("text")),
            (
                key(ACCOUNT, "user:profile user:inference"),
                json!({"token": ""}),
            ),
        ]);

        assert!(select_credentials(&cache, ACCOUNT, now()).is_none());
    }
}
