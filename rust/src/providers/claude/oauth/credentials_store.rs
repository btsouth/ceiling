//! Credential loading (environment / file / OS keyring), persistence of
//! refreshed tokens back to disk, and the in-memory refreshed-credentials
//! cache.
//!
//! The cache is keyed by [`CredentialSource`] so a refreshed token read from
//! one source (e.g. the credentials file) can never shadow credentials read
//! from a different source (e.g. an environment-provided token) that happens
//! to compare as "fresher" under the naive `expires_at` ordering.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::ClaudeOAuthCredentials;
use crate::core::ProviderError;

const KEYRING_SERVICE: &str = "Claude Code-credentials";
const ENV_TOKEN_KEY: &str = "CODEXBAR_CLAUDE_OAUTH_TOKEN";
const ENV_SCOPES_KEY: &str = "CODEXBAR_CLAUDE_OAUTH_SCOPES";

/// Identifies where a set of OAuth credentials was loaded from, so the
/// refreshed-credentials cache never mixes tokens across sources.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum CredentialSource {
    Environment,
    File(PathBuf),
    Keyring(String), // the account string that matched
}

/// In-memory cache of the most recently refreshed credentials, keyed by
/// [`CredentialSource`]. Consulted when a disk persist fails, so we don't hit
/// the refresh endpoint (and rotate the refresh token) on every poll.
static REFRESHED_CREDENTIALS: OnceLock<Mutex<HashMap<CredentialSource, ClaudeOAuthCredentials>>> =
    OnceLock::new();

/// Raw JSON structure from Claude CLI credentials file
#[derive(Debug, Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<OAuthData>,
}

#[derive(Debug, Deserialize)]
struct OAuthData {
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<f64>, // milliseconds since epoch
    scopes: Option<Vec<String>>,
    #[serde(rename = "rateLimitTier")]
    rate_limit_tier: Option<String>,
    /// "pro" / "max" / "free". The only signal that separates a Pro seat from a
    /// Free one, since both report the same `rateLimitTier`.
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

fn refreshed_cache() -> &'static Mutex<HashMap<CredentialSource, ClaudeOAuthCredentials>> {
    REFRESHED_CREDENTIALS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Look up a cached refreshed credential for `source`, returning it only if
/// it is fresher than `file_creds` (i.e. what was just re-read from disk).
pub(super) fn cached_refreshed_if_fresher(
    source: &CredentialSource,
    file_creds: &ClaudeOAuthCredentials,
) -> Option<ClaudeOAuthCredentials> {
    let guard = refreshed_cache().lock().ok()?;
    let cached = guard.get(source)?;
    let fresher = match (cached.expires_at, file_creds.expires_at) {
        (Some(cached_at), Some(file_at)) => cached_at > file_at,
        (Some(_), None) => true,
        _ => false,
    };
    fresher.then(|| cached.clone())
}

/// Store `credentials` in the refreshed-credentials cache under `source`.
pub(super) fn store_refreshed(source: &CredentialSource, credentials: &ClaudeOAuthCredentials) {
    if let Ok(mut guard) = refreshed_cache().lock() {
        guard.insert(source.clone(), credentials.clone());
    }
}

/// Load OAuth credentials from environment, file, or Claude Code's OS credential store.
///
/// `config_dir` selects a specific Ceiling-managed account; `None` follows
/// whichever account the CLI is currently signed in as.
pub(super) fn load_credentials(
    config_dir: Option<&Path>,
) -> Result<(ClaudeOAuthCredentials, CredentialSource), ProviderError> {
    // The environment token and the OS credential store are both single global
    // slots with no way to tell which account they belong to. Consulting them
    // for an explicitly chosen account would silently serve a different seat's
    // usage under that account's label, so an explicit account resolves from its
    // own directory or not at all.
    let is_explicit = is_explicit_account(config_dir);

    if !is_explicit && let Some(creds) = load_from_environment() {
        return Ok((creds, CredentialSource::Environment));
    }

    let file_error = match load_from_file(config_dir) {
        Ok(creds) => return Ok((creds, CredentialSource::File(credentials_path(config_dir)?))),
        Err(err) => err,
    };

    if is_explicit {
        return Err(file_error);
    }

    // Current Claude Code builds store the same JSON payload in the OS credential store.
    // SBS-1023: skip when the user opted out of keychain reads.
    if let Some((creds, source)) = load_from_keyring()? {
        return Ok((creds, source));
    }

    Err(file_error)
}

/// Whether `config_dir` names an account other than the one the CLI is signed
/// in as. Passing the ambient directory explicitly is still the ambient account.
fn is_explicit_account(config_dir: Option<&Path>) -> bool {
    match (config_dir, crate::core::ambient_claude_config_dir()) {
        (Some(dir), Some(ambient)) => !crate::core::same_dir(dir, &ambient),
        // No ambient home: a caller-supplied directory cannot be the CLI's
        // default account, so treat it as explicit (SBS-1021).
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// Load credentials from environment variables
fn load_from_environment() -> Option<ClaudeOAuthCredentials> {
    let token = std::env::var(ENV_TOKEN_KEY).ok()?;
    let token = token.trim();
    if token.is_empty() {
        return None;
    }

    let scopes: Vec<String> = std::env::var(ENV_SCOPES_KEY)
        .ok()
        .map(|s| {
            s.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_else(|| vec!["user:profile".to_string()]);

    Some(ClaudeOAuthCredentials {
        access_token: token.to_string(),
        refresh_token: None,
        expires_at: None, // Environment tokens don't expire
        scopes,
        rate_limit_tier: None,
        subscription_type: None,
    })
}

/// Load credentials from the config directory's `.credentials.json`
fn load_from_file(config_dir: Option<&Path>) -> Result<ClaudeOAuthCredentials, ProviderError> {
    let path = credentials_path(config_dir)?;

    if !path.exists() {
        return Err(ProviderError::OAuth(format!(
            "Claude OAuth credentials not found in {}. Run `claude` to authenticate.",
            path.display()
        )));
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| ProviderError::OAuth(format!("Failed to read credentials file: {}", e)))?;

    parse_credentials_json(&content)
}

pub(super) fn credentials_file_available(config_dir: Option<&Path>) -> bool {
    load_from_file(config_dir).is_ok()
}

/// Load credentials from Claude Code's OS keychain / credential manager entry.
fn load_from_keyring() -> Result<Option<(ClaudeOAuthCredentials, CredentialSource)>, ProviderError>
{
    if !crate::keychain::allowed(crate::keychain::Scope::Claude) {
        tracing::debug!("Skipping Claude keyring read; keychain access disabled (SBS-1023)");
        return Ok(None);
    }

    for account in keyring_account_candidates() {
        let content = match crate::keychain::get_password(
            crate::keychain::Scope::Claude,
            KEYRING_SERVICE,
            &account,
        ) {
            Ok(content) => content,
            Err(crate::keychain::Error::Disabled) => {
                tracing::debug!(
                    "Skipping Claude keyring read; keychain access disabled (SBS-1023)"
                );
                return Ok(None);
            }
            Err(crate::keychain::Error::NotFound) => continue,
            Err(err) => {
                tracing::debug!(
                    "Failed to read Claude Code credential entry for account {}: {}",
                    account,
                    err
                );
                continue;
            }
        };

        if content.trim().is_empty() {
            continue;
        }

        return parse_credentials_json(&content)
            .map(|creds| Some((creds, CredentialSource::Keyring(account))));
    }

    #[cfg(target_os = "macos")]
    if let Some(result) = load_from_macos_security_cli()? {
        return Ok(Some(result));
    }

    Ok(None)
}

#[cfg(target_os = "macos")]
fn load_from_macos_security_cli()
-> Result<Option<(ClaudeOAuthCredentials, CredentialSource)>, ProviderError> {
    for account in keyring_account_candidates() {
        let output = match std::process::Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-s",
                KEYRING_SERVICE,
                "-a",
                &account,
                "-w",
            ])
            .output()
        {
            Ok(output) => output,
            Err(err) => {
                tracing::debug!(
                    "Failed to run macOS security CLI for Claude credentials: {}",
                    err
                );
                continue;
            }
        };

        if !output.status.success() {
            continue;
        }

        let content = String::from_utf8_lossy(&output.stdout);
        if content.trim().is_empty() {
            continue;
        }

        return parse_credentials_json(content.trim())
            .map(|creds| Some((creds, CredentialSource::Keyring(account))));
    }

    Ok(None)
}

fn parse_credentials_json(content: &str) -> Result<ClaudeOAuthCredentials, ProviderError> {
    if let Ok(file) = serde_json::from_str::<CredentialsFile>(content)
        && let Some(oauth) = file.claude_ai_oauth
    {
        return credentials_from_oauth_data(oauth);
    }

    let oauth: OAuthData = serde_json::from_str(content)
        .map_err(|e| ProviderError::OAuth(format!("Invalid credentials format: {}", e)))?;
    credentials_from_oauth_data(oauth)
}

fn credentials_from_oauth_data(oauth: OAuthData) -> Result<ClaudeOAuthCredentials, ProviderError> {
    let access_token = oauth.access_token.ok_or_else(|| {
        ProviderError::OAuth(
            "Claude OAuth access token missing. Run `claude` to authenticate.".to_string(),
        )
    })?;

    let access_token = access_token.trim().to_string();
    if access_token.is_empty() {
        return Err(ProviderError::OAuth(
            "Claude OAuth access token is empty. Run `claude` to authenticate.".to_string(),
        ));
    }

    // Convert milliseconds to DateTime
    let expires_at = oauth.expires_at.map(|millis| {
        let secs = (millis / 1000.0) as i64;
        DateTime::from_timestamp(secs, 0).unwrap_or_else(Utc::now)
    });

    Ok(ClaudeOAuthCredentials {
        access_token,
        refresh_token: oauth.refresh_token,
        expires_at,
        scopes: oauth.scopes.unwrap_or_default(),
        rate_limit_tier: oauth.rate_limit_tier,
        subscription_type: oauth.subscription_type,
    })
}

fn keyring_account_candidates() -> Vec<String> {
    let mut candidates = Vec::new();
    for key in ["USER", "USERNAME"] {
        if let Ok(value) = std::env::var(key) {
            push_keyring_candidate(&mut candidates, value);
        }
    }

    #[cfg(not(windows))]
    {
        if let Ok(output) = std::process::Command::new("whoami").output()
            && output.status.success()
        {
            let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
            push_keyring_candidate(&mut candidates, value.clone());
            if let Some((_, username)) = value.rsplit_once('\\') {
                push_keyring_candidate(&mut candidates, username.to_string());
            }
            if let Some((_, username)) = value.rsplit_once('/') {
                push_keyring_candidate(&mut candidates, username.to_string());
            }
        }
    }

    candidates
}

fn push_keyring_candidate(candidates: &mut Vec<String>, value: String) {
    let value = value.trim();
    if value.is_empty() || candidates.iter().any(|candidate| candidate == value) {
        return;
    }
    candidates.push(value.to_string());
}

/// Resolve the credentials file for a config directory, defaulting to the one
/// the CLI itself would use (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
///
/// No resolvable ambient directory is an explicit error, not `./.claude`
/// (SBS-1021 / SBS-950).
fn credentials_path(config_dir: Option<&Path>) -> Result<PathBuf, ProviderError> {
    credentials_path_from(config_dir, crate::core::ambient_claude_config_dir())
}

fn credentials_path_from(
    config_dir: Option<&Path>,
    ambient_dir: Option<PathBuf>,
) -> Result<PathBuf, ProviderError> {
    let dir = config_dir
        .map(Path::to_path_buf)
        .or(ambient_dir)
        .ok_or_else(|| {
            ProviderError::NotInstalled("Could not resolve Claude config directory.".to_string())
        })?;
    Ok(crate::core::claude_credentials_path(&dir))
}

/// Persist refreshed tokens back to the store they were loaded from.
///
/// Claude Code owns both `.credentials.json` and the `Claude Code-credentials`
/// keyring entry. Writing the rotated pair back is required: Anthropic retires
/// the exchanged refresh token, and leaving the old one in the keyring signs
/// Claude Code out.
///
/// `exchanged_refresh_token` is the token this process actually sent to the
/// server. Claude rotates the refresh token, so it doubles as a
/// compare-and-swap: if the store no longer holds it, another refresh already
/// rotated it away and ours is retired.
///
/// Returns `None` when our tokens were written, or the live credentials when
/// another process won the race, so the caller can adopt those rather than
/// cache a token the server has retired.
pub(super) fn persist_refreshed_credentials(
    credentials: &ClaudeOAuthCredentials,
    config_dir: Option<&Path>,
    exchanged_refresh_token: &str,
) -> Result<Option<ClaudeOAuthCredentials>, ProviderError> {
    persist_refreshed_for_source(
        credentials,
        &CredentialSource::File(credentials_path(config_dir)?),
        exchanged_refresh_token,
    )
}

pub(super) fn persist_refreshed_for_source(
    credentials: &ClaudeOAuthCredentials,
    source: &CredentialSource,
    exchanged_refresh_token: &str,
) -> Result<Option<ClaudeOAuthCredentials>, ProviderError> {
    match source {
        CredentialSource::Environment => Ok(None),
        CredentialSource::File(path) => {
            persist_refreshed_file(credentials, path, exchanged_refresh_token)
        }
        CredentialSource::Keyring(account) => {
            persist_refreshed_keyring(credentials, account, exchanged_refresh_token)
        }
    }
}

fn persist_refreshed_file(
    credentials: &ClaudeOAuthCredentials,
    path: &Path,
    exchanged_refresh_token: &str,
) -> Result<Option<ClaudeOAuthCredentials>, ProviderError> {
    if !path.exists() {
        return Ok(None);
    }

    // Lock the resolved target, not the link. A Windows path that links at a
    // WSL file and the WSL path that *is* that file would otherwise take two
    // different locks and still interleave on the one file they share.
    let lock_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    crate::secure_file::with_file_write_lock(&lock_path, || {
        let content = std::fs::read_to_string(path)?;
        match merge_refreshed_credentials_json(&content, credentials, exchanged_refresh_token)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        {
            Err(live) => Ok(Some(live)),
            Ok(serialized) => {
                crate::secure_file::atomic_write_preserving_permissions(
                    path,
                    serialized.as_bytes(),
                )?;
                Ok(None)
            }
        }
    })
    .map_err(|e| ProviderError::OAuth(format!("Failed to update Claude credentials: {e}")))
}

fn persist_refreshed_keyring(
    credentials: &ClaudeOAuthCredentials,
    account: &str,
    exchanged_refresh_token: &str,
) -> Result<Option<ClaudeOAuthCredentials>, ProviderError> {
    if !crate::keychain::allowed(crate::keychain::Scope::Claude) {
        return Err(ProviderError::OAuth(
            "Claude keychain writes are disabled (SBS-1023). The refreshed token was not stored."
                .to_string(),
        ));
    }

    let content =
        crate::keychain::get_password(crate::keychain::Scope::Claude, KEYRING_SERVICE, account)
            .map_err(|err| {
                ProviderError::OAuth(format!(
                    "Failed to read Claude Code credential entry for account {account}: {err}"
                ))
            })?;
    match merge_refreshed_credentials_json(&content, credentials, exchanged_refresh_token)? {
        Err(live) => Ok(Some(live)),
        Ok(serialized) => {
            crate::keychain::set_password(
                crate::keychain::Scope::Claude,
                KEYRING_SERVICE,
                account,
                &serialized,
            )
            .map_err(|err| {
                ProviderError::OAuth(format!(
                    "Failed to write Claude Code credential entry for account {account}: {err}"
                ))
            })?;
            Ok(None)
        }
    }
}

/// Merge rotated tokens into a Claude credentials JSON payload.
///
/// `Ok(Ok(json))` is the replacement payload. `Ok(Err(live))` means another
/// refresh already rotated the exchanged token; the caller must adopt `live`.
fn merge_refreshed_credentials_json(
    content: &str,
    credentials: &ClaudeOAuthCredentials,
    exchanged_refresh_token: &str,
) -> Result<Result<String, ClaudeOAuthCredentials>, ProviderError> {
    let mut root: serde_json::Value = serde_json::from_str(content)
        .map_err(|e| ProviderError::OAuth(format!("Invalid credentials format: {e}")))?;
    if !disk_still_holds_exchanged_refresh(&root, exchanged_refresh_token) {
        return Ok(Err(parse_credentials_json(content)?));
    }
    apply_refresh_to_credentials_json(&mut root, credentials)?;
    let serialized = serde_json::to_string_pretty(&root).map_err(|e| {
        ProviderError::OAuth(format!("Failed to serialize Claude credentials: {e}"))
    })?;
    Ok(Ok(serialized))
}

/// Whether the file still holds the refresh token this process exchanged.
///
/// A desktop and a CLI can refresh the same seat at once, both starting from
/// the same refresh token. Claude rotates that token, so only the first
/// exchange stays valid; the loser's copy is already retired and writing it
/// signs Claude Code out.
///
/// Token identity is the discriminator, not `expiresAt`: both processes set
/// the expiry to `now + server TTL`, so the winner usually has the *smaller*
/// timestamp and an ordering check gets the race backwards.
///
/// A file with no `refreshToken` (never had one, or hand-edited away) is not
/// evidence that someone else rotated it, so the write proceeds.
fn disk_still_holds_exchanged_refresh(root: &serde_json::Value, exchanged: &str) -> bool {
    refresh_token_in_credentials_json(root).is_none_or(|on_disk| on_disk == exchanged)
}

fn refresh_token_in_credentials_json(root: &serde_json::Value) -> Option<&str> {
    root.get("claudeAiOauth")
        .and_then(|oauth| oauth.get("refreshToken"))
        .or_else(|| root.get("refreshToken"))
        .and_then(serde_json::Value::as_str)
}

fn oauth_object_mut(
    root: &mut serde_json::Value,
) -> Result<&mut serde_json::Map<String, serde_json::Value>, ProviderError> {
    if root.get("claudeAiOauth").is_some() {
        return root
            .get_mut("claudeAiOauth")
            .and_then(|value| value.as_object_mut())
            .ok_or_else(|| {
                ProviderError::OAuth("credentials file missing claudeAiOauth object".to_string())
            });
    }
    root.as_object_mut()
        .ok_or_else(|| ProviderError::OAuth("credentials payload is not a JSON object".to_string()))
}

/// Pure JSON merge used by [`persist_refreshed_credentials`]. Updates only
/// the token fields inside `claudeAiOauth`, or the root object when the
/// payload is a bare OAuth blob (Claude Code's keyring format).
fn apply_refresh_to_credentials_json(
    root: &mut serde_json::Value,
    credentials: &ClaudeOAuthCredentials,
) -> Result<(), ProviderError> {
    let oauth = oauth_object_mut(root)?;

    oauth.insert(
        "accessToken".to_string(),
        serde_json::Value::String(credentials.access_token.clone()),
    );
    if let Some(refresh_token) = &credentials.refresh_token {
        oauth.insert(
            "refreshToken".to_string(),
            serde_json::Value::String(refresh_token.clone()),
        );
    }
    if let Some(expires_at) = credentials.expires_at {
        oauth.insert(
            "expiresAt".to_string(),
            serde_json::Value::Number(expires_at.timestamp_millis().into()),
        );
    }
    if !credentials.scopes.is_empty() {
        oauth.insert(
            "scopes".to_string(),
            serde_json::Value::Array(
                credentials
                    .scopes
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CredentialSource, ENV_TOKEN_KEY, KEYRING_SERVICE, apply_refresh_to_credentials_json,
        cached_refreshed_if_fresher, credentials_path, credentials_path_from,
        disk_still_holds_exchanged_refresh, load_credentials, merge_refreshed_credentials_json,
        parse_credentials_json, persist_refreshed_credentials, persist_refreshed_for_source,
        store_refreshed,
    };
    use crate::core::ProviderError;
    use crate::providers::claude::oauth::ClaudeOAuthCredentials;
    use std::path::Path;

    fn refreshed_credentials(access_token: &str) -> ClaudeOAuthCredentials {
        ClaudeOAuthCredentials {
            access_token: access_token.to_string(),
            refresh_token: Some("rotated-refresh".to_string()),
            expires_at: chrono::DateTime::from_timestamp(2_000, 0),
            scopes: vec!["user:inference".to_string()],
            rate_limit_tier: None,
            subscription_type: None,
        }
    }

    /// Writes a minimal but realistic `.credentials.json` into `dir`.
    fn seed_credentials_file(dir: &Path) -> std::path::PathBuf {
        let path = dir.join(".credentials.json");
        std::fs::write(
            &path,
            br#"{
              "claudeAiOauth": {
                "accessToken": "old-access",
                "refreshToken": "old-refresh",
                "subscriptionType": "max"
              },
              "mcpOAuth": { "some-server": { "accessToken": "keepme" } }
            }"#,
        )
        .unwrap();
        path
    }

    #[test]
    fn persist_updates_tokens_and_keeps_unrelated_blocks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = seed_credentials_file(dir.path());

        persist_refreshed_credentials(
            &refreshed_credentials("new-access"),
            Some(dir.path()),
            "old-refresh",
        )
        .expect("persist");

        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["claudeAiOauth"]["accessToken"], "new-access");
        assert_eq!(root["claudeAiOauth"]["refreshToken"], "rotated-refresh");
        // Fields Ceiling does not own survive the replace.
        assert_eq!(root["claudeAiOauth"]["subscriptionType"], "max");
        assert_eq!(root["mcpOAuth"]["some-server"]["accessToken"], "keepme");
    }

    /// SBS-883: the write lock orders two concurrent refreshes, it does not
    /// make the later one correct. Claude rotates the refresh token, so the
    /// loser holds one the server already invalidated.
    ///
    /// The winner finishes its HTTP call first and therefore usually has the
    /// *smaller* `expiresAt`, which is why this is decided on token identity
    /// rather than on any timestamp ordering.
    #[test]
    fn persist_does_not_overwrite_a_seat_another_process_already_rotated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".credentials.json");
        // The winner already replaced "shared-old-refresh" with its own, and
        // its expiry is *earlier* than ours because it finished first.
        std::fs::write(
            &path,
            br#"{
              "claudeAiOauth": {
                "accessToken": "winner-access",
                "refreshToken": "winner-refresh",
                "expiresAt": 1000
              }
            }"#,
        )
        .unwrap();

        let live = persist_refreshed_credentials(
            &refreshed_credentials("loser-access"),
            Some(dir.path()),
            "shared-old-refresh",
        )
        .expect("persist");

        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["claudeAiOauth"]["accessToken"], "winner-access");
        assert_eq!(root["claudeAiOauth"]["refreshToken"], "winner-refresh");
        assert_eq!(
            live.expect("the winner's credentials come back")
                .access_token,
            "winner-access",
            "the loser must adopt the live tokens, not cache its retired ones"
        );
    }

    #[test]
    fn persist_writes_when_the_exchanged_refresh_is_still_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = seed_credentials_file(dir.path());

        let live = persist_refreshed_credentials(
            &refreshed_credentials("new-access"),
            Some(dir.path()),
            "old-refresh",
        )
        .expect("persist");

        assert!(
            live.is_none(),
            "our write won, so there is nothing to adopt"
        );
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["claudeAiOauth"]["accessToken"], "new-access");
        assert_eq!(root["claudeAiOauth"]["refreshToken"], "rotated-refresh");
    }

    #[test]
    fn persist_leaves_an_unparseable_file_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".credentials.json");
        std::fs::write(&path, b"{ not json").unwrap();

        let error = persist_refreshed_credentials(
            &refreshed_credentials("new-access"),
            Some(dir.path()),
            "old-refresh",
        )
        .expect_err("parse failure must not write");

        assert!(matches!(error, crate::core::ProviderError::OAuth(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }

    /// The decision that settles a concurrent refresh, exercised without any
    /// file I/O so it is covered on every platform this ships to. The symlink
    /// and file-mode tests below cannot run everywhere, and this crate's CI
    /// runs on Windows.
    #[test]
    fn the_write_goes_through_while_our_exchanged_refresh_is_still_on_disk() {
        let root = serde_json::json!({ "claudeAiOauth": { "refreshToken": "old-refresh" } });

        assert!(disk_still_holds_exchanged_refresh(&root, "old-refresh"));
    }

    #[test]
    fn a_refresh_token_another_process_rotated_blocks_the_write() {
        let root = serde_json::json!({ "claudeAiOauth": { "refreshToken": "winner-refresh" } });

        assert!(!disk_still_holds_exchanged_refresh(&root, "old-refresh"));
    }

    #[test]
    fn merge_writes_rotated_tokens_into_a_bare_keyring_blob() {
        let original =
            r#"{"accessToken":"old-access","refreshToken":"old-refresh","subscriptionType":"max"}"#;
        let written = merge_refreshed_credentials_json(
            original,
            &refreshed_credentials("new-access"),
            "old-refresh",
        )
        .expect("merge")
        .expect("write");
        let root: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(root["accessToken"], "new-access");
        assert_eq!(root["refreshToken"], "rotated-refresh");
        assert_eq!(root["subscriptionType"], "max");
    }

    #[test]
    fn merge_adopts_a_keyring_blob_another_process_already_rotated() {
        let original = r#"{"accessToken":"winner-access","refreshToken":"winner-refresh"}"#;
        let live = merge_refreshed_credentials_json(
            original,
            &refreshed_credentials("loser-access"),
            "old-refresh",
        )
        .expect("merge")
        .expect_err("adopt");
        assert_eq!(live.access_token, "winner-access");
    }

    /// No timestamp takes part in the decision. Two refreshes of one seat set
    /// `expiresAt` to `now + server TTL`, so they land in the same second and
    /// the winner's is often the *smaller* one; ordering on it would call the
    /// race backwards.
    #[test]
    fn the_race_is_decided_on_token_identity_not_on_expires_at() {
        let rotated_but_later = serde_json::json!({
            "claudeAiOauth": { "refreshToken": "winner-refresh", "expiresAt": i64::MAX }
        });
        assert!(
            !disk_still_holds_exchanged_refresh(&rotated_but_later, "old-refresh"),
            "a rotated token is retired however far out its expiry sits"
        );

        let ours_but_earlier = serde_json::json!({
            "claudeAiOauth": { "refreshToken": "old-refresh", "expiresAt": 0 }
        });
        assert!(
            disk_still_holds_exchanged_refresh(&ours_but_earlier, "old-refresh"),
            "our own token on disk is ours to replace whatever its expiry says"
        );
    }

    /// A file with no `refreshToken` at all (never had one, or hand-edited
    /// away) is not evidence that another process rotated ours.
    #[test]
    fn a_file_without_a_refresh_token_is_not_a_lost_race() {
        let no_token = serde_json::json!({ "claudeAiOauth": { "accessToken": "old-access" } });
        assert!(disk_still_holds_exchanged_refresh(&no_token, "old-refresh"));

        let no_block = serde_json::json!({ "mcpOAuth": {} });
        assert!(disk_still_holds_exchanged_refresh(&no_block, "old-refresh"));
    }

    /// Mirrors the lock-file name `secure_file::with_file_write_lock` derives,
    /// so a test can say which path the lock was taken on.
    fn write_lock_path(file_path: &Path) -> std::path::PathBuf {
        let mut lock_name = std::ffi::OsString::from(".");
        lock_name.push(file_path.file_name().expect("file name"));
        lock_name.push(".ceiling-write.lock");
        file_path.parent().expect("parent").join(lock_name)
    }

    fn seed_symlink_target(dir: &Path) -> std::path::PathBuf {
        let target = dir.join("real-credentials.json");
        std::fs::write(
            &target,
            br#"{"claudeAiOauth":{"accessToken":"old-access","refreshToken":"old-refresh"}}"#,
        )
        .unwrap();
        target
    }

    /// SBS-883: Claude Code owns this file. A dotfile manager or a WSL shared
    /// target puts a symlink at the credential path; replacing the link with a
    /// fresh private file splits Ceiling's tokens from the real target.
    ///
    /// Shared by the unix and Windows cases, which differ only in how the link
    /// is made.
    fn assert_persist_follows_the_symlink(link_dir: &Path, target: &Path, link: &Path) {
        persist_refreshed_credentials(
            &refreshed_credentials("new-access"),
            Some(link_dir),
            "old-refresh",
        )
        .expect("persist");

        assert!(
            std::fs::symlink_metadata(link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the credential path must still be a symlink"
        );
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(target).unwrap()).unwrap();
        assert_eq!(
            root["claudeAiOauth"]["accessToken"], "new-access",
            "the refresh must land on the symlink target"
        );
        // Every path pointing at this file has to contend for one lock, so it
        // has to be keyed on the target rather than on the link.
        assert!(
            write_lock_path(target).exists(),
            "the write lock belongs beside the shared target"
        );
        assert!(
            !write_lock_path(link).exists(),
            "a lock beside the link would not serialize the other path at the same file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn persist_writes_through_a_symlinked_credential_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = tempfile::tempdir().expect("tempdir");

        let target = seed_symlink_target(store.path());
        let link = dir.path().join(".credentials.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert_persist_follows_the_symlink(dir.path(), &target, &link);
    }

    /// The same case on the platform this crate's CI actually runs, where the
    /// atomic replace is `ReplaceFileW` rather than `rename`. This is the
    /// shipped shape too: a Windows `.claude\.credentials.json` linked at the
    /// file WSL uses.
    ///
    /// Creating a symlink needs a privilege an unelevated account without
    /// developer mode does not hold, so that machine skips rather than fails.
    #[cfg(windows)]
    #[test]
    fn persist_writes_through_a_symlinked_credential_path() {
        /// `ERROR_PRIVILEGE_NOT_HELD`.
        const NO_SYMLINK_PRIVILEGE: i32 = 1314;

        let dir = tempfile::tempdir().expect("tempdir");
        let store = tempfile::tempdir().expect("tempdir");

        let target = seed_symlink_target(store.path());
        let link = dir.path().join(".credentials.json");
        match std::os::windows::fs::symlink_file(&target, &link) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(NO_SYMLINK_PRIVILEGE) => {
                eprintln!("skipped: this account may not create symlinks");
                return;
            }
            Err(error) => panic!("could not create the credential symlink: {error}"),
        }

        assert_persist_follows_the_symlink(dir.path(), &target, &link);
    }

    /// The file's own permissions are Claude Code's to choose; a refresh must
    /// not silently tighten or loosen them.
    #[cfg(unix)]
    #[test]
    fn persist_preserves_the_existing_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = seed_credentials_file(dir.path());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();

        persist_refreshed_credentials(
            &refreshed_credentials("new-access"),
            Some(dir.path()),
            "old-refresh",
        )
        .expect("persist");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "persist must not rewrite the permission bits");
    }

    /// Whether the file carries a protected DACL of its own instead of
    /// inheriting one from its directory.
    ///
    /// This is what separates the two replaces on Windows. The private one
    /// moves a temp file that was locked to the current user, so its protected
    /// DACL becomes the file's. The metadata-preserving one puts the
    /// destination's own descriptor back afterwards.
    #[cfg(windows)]
    fn dacl_is_protected(path: &Path) -> bool {
        use std::mem::size_of;
        use std::os::windows::ffi::OsStrExt;

        use windows::Win32::Security::{
            DACL_SECURITY_INFORMATION, GetFileSecurityW, GetSecurityDescriptorControl,
            PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
        };
        use windows::core::PCWSTR;

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut needed = 0u32;
        unsafe {
            // First call only sizes the buffer, so its failure is expected.
            let _ = GetFileSecurityW(
                PCWSTR(wide.as_ptr()),
                DACL_SECURITY_INFORMATION.0,
                None,
                0,
                &mut needed,
            );
        }
        assert!(needed > 0, "could not size the security descriptor");

        let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        let descriptor = PSECURITY_DESCRIPTOR(buffer.as_mut_ptr().cast());
        let mut control = 0u16;
        let mut revision = 0u32;
        unsafe {
            GetFileSecurityW(
                PCWSTR(wide.as_ptr()),
                DACL_SECURITY_INFORMATION.0,
                Some(descriptor),
                needed,
                &mut needed,
            )
            .ok()
            .expect("read the security descriptor");
            GetSecurityDescriptorControl(descriptor, &mut control, &mut revision)
                .expect("read the descriptor control bits");
        }
        control & SE_DACL_PROTECTED.0 != 0
    }

    /// The Windows half of `persist_preserves_the_existing_file_mode`, and the
    /// one that runs on this crate's CI. Claude Code's file inherits its DACL;
    /// a private replace would leave Ceiling's protected current-user DACL on
    /// it, which is the regression the symlink test cannot catch on a machine
    /// that may not create symlinks.
    #[cfg(windows)]
    #[test]
    fn persist_keeps_the_files_own_windows_security_descriptor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = seed_credentials_file(dir.path());
        assert!(
            !dacl_is_protected(&path),
            "a plain new file should still inherit its DACL"
        );

        persist_refreshed_credentials(
            &refreshed_credentials("new-access"),
            Some(dir.path()),
            "old-refresh",
        )
        .expect("persist");

        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("new-access"),
            "the refresh must still land in the file"
        );
        assert!(
            !dacl_is_protected(&path),
            "persist must restore the file's own DACL, not stamp Ceiling's private one"
        );
    }

    /// `CLAUDE_CONFIG_DIR` and the env token are process-global, so the tests
    /// that manipulate them run one at a time.
    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    /// Restores an environment variable to its prior value on drop, so a failing
    /// assertion cannot leak state into another test.
    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: serialized by `env_lock`, and restored on drop.
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        fn set_str(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: serialized by `env_lock`, and restored on drop.
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            // SAFETY: serialized by `env_lock`, and restored on drop.
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: serialized by `env_lock`.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    const CREDENTIALS_JSON: &str = r#"{"claudeAiOauth":{"accessToken":"from-disk",
        "refreshToken":"r","scopes":["user:profile"],"subscriptionType":"max"}}"#;

    fn write_credentials(dir: &Path) {
        std::fs::create_dir_all(dir).expect("create dir");
        std::fs::write(dir.join(".credentials.json"), CREDENTIALS_JSON).expect("write credentials");
    }

    #[test]
    fn the_default_path_honors_claude_config_dir() {
        let _guard = env_lock().lock().expect("env lock");
        let dir = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::set("CLAUDE_CONFIG_DIR", dir.path());

        let path = credentials_path(None).expect("path");

        assert_eq!(path, dir.path().join(".credentials.json"));
    }

    /// Pins SBS-1021: no ambient Claude home is `NotInstalled`, not `./.claude`.
    #[test]
    fn unresolved_ambient_dir_is_not_installed_and_does_not_probe_relative_claude() {
        let planted = tempfile::tempdir().expect("tempdir");
        write_credentials(&planted.path().join(".claude"));

        let error = credentials_path_from(None, None).expect_err("unresolved home");
        assert!(
            matches!(error, ProviderError::NotInstalled(_)),
            "expected NotInstalled, got {error:?}"
        );
        assert!(
            planted
                .path()
                .join(".claude")
                .join(".credentials.json")
                .exists(),
            "decoy exists so a relative .claude probe would have succeeded"
        );
    }

    #[test]
    fn an_explicit_account_reads_its_own_directory() {
        let _guard = env_lock().lock().expect("env lock");
        let ambient = tempfile::tempdir().expect("tempdir");
        let _env = EnvGuard::set("CLAUDE_CONFIG_DIR", ambient.path());
        let account = tempfile::tempdir().expect("tempdir");

        let path = credentials_path(Some(account.path())).expect("path");

        assert_eq!(path, account.path().join(".credentials.json"));
    }

    #[test]
    fn an_explicit_account_never_falls_back_to_the_global_env_token() {
        let _guard = env_lock().lock().expect("env lock");
        let ambient = tempfile::tempdir().expect("tempdir");
        let _config = EnvGuard::set("CLAUDE_CONFIG_DIR", ambient.path());
        let _token = EnvGuard::set_str(ENV_TOKEN_KEY, "env-token");
        // A configured account whose directory holds no credentials.
        let account = tempfile::tempdir().expect("tempdir");

        let result = load_credentials(Some(account.path()));

        // Serving the env token here would label another seat's usage as this
        // account's, which is exactly the leak switching must not have.
        assert!(
            result.is_err(),
            "an account with no credentials must fail rather than borrow a global token"
        );
    }

    #[test]
    fn the_ambient_account_still_uses_the_global_env_token() {
        let _guard = env_lock().lock().expect("env lock");
        let ambient = tempfile::tempdir().expect("tempdir");
        let _config = EnvGuard::set("CLAUDE_CONFIG_DIR", ambient.path());
        let _token = EnvGuard::set_str(ENV_TOKEN_KEY, "env-token");

        let (credentials, source) = load_credentials(None).expect("credentials");

        assert_eq!(credentials.access_token, "env-token");
        assert_eq!(source, CredentialSource::Environment);
    }

    #[test]
    fn naming_the_ambient_directory_explicitly_is_still_the_ambient_account() {
        let _guard = env_lock().lock().expect("env lock");
        let ambient = tempfile::tempdir().expect("tempdir");
        let _config = EnvGuard::set("CLAUDE_CONFIG_DIR", ambient.path());
        let _token = EnvGuard::set_str(ENV_TOKEN_KEY, "env-token");

        let (credentials, _) = load_credentials(Some(ambient.path())).expect("credentials");

        assert_eq!(credentials.access_token, "env-token");
    }

    #[test]
    fn an_explicit_account_loads_from_its_own_credentials_file() {
        let _guard = env_lock().lock().expect("env lock");
        let ambient = tempfile::tempdir().expect("tempdir");
        let _config = EnvGuard::set("CLAUDE_CONFIG_DIR", ambient.path());
        let _token = EnvGuard::unset(ENV_TOKEN_KEY);
        let account = tempfile::tempdir().expect("tempdir");
        write_credentials(account.path());

        let (credentials, source) = load_credentials(Some(account.path())).expect("credentials");

        assert_eq!(credentials.access_token, "from-disk");
        assert_eq!(
            source,
            CredentialSource::File(account.path().join(".credentials.json"))
        );
    }

    #[test]
    fn two_accounts_resolve_to_different_credential_files() {
        let _guard = env_lock().lock().expect("env lock");
        let _token = EnvGuard::unset(ENV_TOKEN_KEY);
        let personal = tempfile::tempdir().expect("tempdir");
        let work = tempfile::tempdir().expect("tempdir");
        write_credentials(personal.path());

        let personal_path = credentials_path(Some(personal.path())).expect("path");
        let work_path = credentials_path(Some(work.path())).expect("path");

        assert_ne!(personal_path, work_path);
        // The seat that is not signed in reports that, rather than showing the
        // other seat's usage.
        assert!(load_credentials(Some(personal.path())).is_ok());
        assert!(load_credentials(Some(work.path())).is_err());
    }

    #[test]
    fn parses_claude_code_credentials_payload() {
        let credentials = parse_credentials_json(
            r#"{
                "claudeAiOauth": {
                    "accessToken": "token",
                    "refreshToken": "refresh",
                    "expiresAt": 1770000000000,
                    "scopes": ["user:profile"],
                    "rateLimitTier": "default_claude_ai"
                }
            }"#,
        )
        .expect("Claude Code credential payload should parse");

        assert_eq!(credentials.access_token, "token");
        assert_eq!(credentials.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(credentials.scopes, vec!["user:profile"]);
        assert_eq!(
            credentials.rate_limit_tier.as_deref(),
            Some("default_claude_ai")
        );
        assert!(credentials.expires_at.is_some());
    }

    #[test]
    fn reads_subscription_type_from_claude_code_credentials() {
        // A Pro seat shares `default_claude_ai` with Free, so `subscriptionType`
        // is the only thing that says which plan it is.
        let credentials = parse_credentials_json(
            r#"{
                "claudeAiOauth": {
                    "accessToken": "token",
                    "scopes": ["user:profile"],
                    "rateLimitTier": "default_claude_ai",
                    "subscriptionType": "pro"
                }
            }"#,
        )
        .expect("Claude Code credential payload should parse");

        assert_eq!(credentials.subscription_type.as_deref(), Some("pro"));
    }

    #[test]
    fn parses_direct_oauth_credentials_payload() {
        let credentials = parse_credentials_json(
            r#"{
                "accessToken": "token",
                "scopes": ["user:profile"]
            }"#,
        )
        .expect("direct OAuth payload should parse");

        assert_eq!(credentials.access_token, "token");
        assert_eq!(credentials.scopes, vec!["user:profile"]);
    }

    #[test]
    fn rejects_credentials_payload_without_access_token() {
        let error = parse_credentials_json(
            r#"{
                "claudeAiOauth": {
                    "refreshToken": "refresh",
                    "scopes": ["user:profile"]
                }
            }"#,
        )
        .expect_err("access token is required");

        assert!(
            error
                .to_string()
                .contains("Claude OAuth access token missing")
        );
    }

    #[test]
    fn apply_refresh_updates_only_oauth_block_and_preserves_others() {
        let mut root: serde_json::Value = serde_json::from_str(
            r#"{
                "mcpOAuth": {"some-server": {"accessToken": "keepme"}},
                "claudeAiOauth": {
                    "accessToken": "old",
                    "refreshToken": "old-refresh",
                    "expiresAt": 1000,
                    "scopes": ["user:profile"],
                    "subscriptionType": "max"
                }
            }"#,
        )
        .unwrap();

        let creds = ClaudeOAuthCredentials {
            access_token: "fresh-access".to_string(),
            refresh_token: Some("fresh-refresh".to_string()),
            expires_at: chrono::DateTime::from_timestamp(2_000, 0),
            scopes: vec!["user:profile".to_string(), "user:inference".to_string()],
            rate_limit_tier: None,
            subscription_type: None,
        };

        apply_refresh_to_credentials_json(&mut root, &creds).unwrap();

        // Unrelated top-level blocks are preserved untouched.
        assert_eq!(root["mcpOAuth"]["some-server"]["accessToken"], "keepme");
        // Non-token fields inside claudeAiOauth are preserved.
        assert_eq!(root["claudeAiOauth"]["subscriptionType"], "max");
        // Token fields are updated.
        assert_eq!(root["claudeAiOauth"]["accessToken"], "fresh-access");
        assert_eq!(root["claudeAiOauth"]["refreshToken"], "fresh-refresh");
        assert_eq!(root["claudeAiOauth"]["expiresAt"], 2_000_000i64);
        assert_eq!(
            root["claudeAiOauth"]["scopes"],
            serde_json::json!(["user:profile", "user:inference"])
        );
    }

    /// Regression test for the cross-source cache contamination bug: an
    /// environment-provided token (which has no `expires_at`) must never be
    /// shadowed by a cached refreshed credential that came from the
    /// credentials *file* source, even though the naive `(Some(_), None) =>
    /// true` freshness rule would treat any file-cached value as "fresher"
    /// than an env token with no expiry.
    #[test]
    fn env_source_not_shadowed_by_file_cache() {
        // Distinct, unique source key so this test can't collide with other
        // tests touching the same process-global cache when run in parallel.
        let file_source = CredentialSource::File(std::path::PathBuf::from(
            "env_source_not_shadowed_by_file_cache-unique-marker.json",
        ));

        let file_cached_creds = ClaudeOAuthCredentials {
            access_token: "file-refreshed-token".to_string(),
            refresh_token: Some("file-refresh".to_string()),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            scopes: vec!["user:profile".to_string()],
            rate_limit_tier: None,
            subscription_type: None,
        };
        store_refreshed(&file_source, &file_cached_creds);

        let env_creds = ClaudeOAuthCredentials {
            access_token: "env-token".to_string(),
            refresh_token: None,
            expires_at: None,
            scopes: vec!["user:profile".to_string()],
            rate_limit_tier: None,
            subscription_type: None,
        };

        // Looking up under the Environment source must not see the File
        // source's cached (and "fresher"-by-the-naive-rule) entry.
        let result = cached_refreshed_if_fresher(&CredentialSource::Environment, &env_creds);
        assert!(
            result.is_none(),
            "environment credentials must not be shadowed by a file-sourced cache entry"
        );

        // Sanity check: the file source's own cache entry is still there and
        // still considered fresher than a file-read with no expiry.
        let file_disk_creds = ClaudeOAuthCredentials {
            access_token: "file-disk-token".to_string(),
            refresh_token: Some("file-disk-refresh".to_string()),
            expires_at: None,
            scopes: vec!["user:profile".to_string()],
            rate_limit_tier: None,
            subscription_type: None,
        };
        let same_source_result = cached_refreshed_if_fresher(&file_source, &file_disk_creds);
        assert_eq!(
            same_source_result.map(|c| c.access_token),
            Some("file-refreshed-token".to_string())
        );
    }

    /// SBS-1023: a keyring-resident Claude token must not be read when the
    /// user opted out. Without the gate this returns `from-keyring`.
    #[test]
    fn keychain_flags_skip_claude_keyring_reads() {
        let _guard = env_lock().lock().expect("env lock");
        let ambient = tempfile::tempdir().expect("tempdir");
        let _config = EnvGuard::set("CLAUDE_CONFIG_DIR", ambient.path());
        let _token = EnvGuard::unset(ENV_TOKEN_KEY);
        let _user = EnvGuard::set_str("USER", "sbs-1023-user");
        let _username = EnvGuard::unset("USERNAME");
        let payload =
            r#"{"accessToken":"from-keyring","refreshToken":"r","scopes":["user:profile"]}"#;
        let _keychain =
            crate::keychain::with_mock_store(false, &[(KEYRING_SERVICE, "sbs-1023-user", payload)]);

        let error = load_credentials(None).expect_err("disabled keychain must not use the keyring");
        assert!(
            error.to_string().contains("credentials not found"),
            "expected a file-miss error, got {error}"
        );
    }

    /// SBS-1023: the same mock secret is used when access is allowed, so the
    /// skip test above is not a missing-entry false pass.
    #[test]
    fn keychain_flags_still_read_claude_keyring_when_allowed() {
        let _guard = env_lock().lock().expect("env lock");
        let ambient = tempfile::tempdir().expect("tempdir");
        let _config = EnvGuard::set("CLAUDE_CONFIG_DIR", ambient.path());
        let _token = EnvGuard::unset(ENV_TOKEN_KEY);
        let _user = EnvGuard::set_str("USER", "sbs-1023-user");
        let _username = EnvGuard::unset("USERNAME");
        let payload =
            r#"{"accessToken":"from-keyring","refreshToken":"r","scopes":["user:profile"]}"#;
        let _keychain =
            crate::keychain::with_mock_store(true, &[(KEYRING_SERVICE, "sbs-1023-user", payload)]);

        let (credentials, source) = load_credentials(None).expect("keyring credentials");
        assert_eq!(credentials.access_token, "from-keyring");
        assert_eq!(source, CredentialSource::Keyring("sbs-1023-user".into()));
    }

    /// SBS-1023: a refreshed Claude token must not be written back to the
    /// keyring when the user opted out. Without the gate this opens the OS
    /// entry instead of returning the opt-out error.
    #[test]
    fn keychain_flags_skip_claude_token_writes() {
        let _keychain = crate::keychain::with_claude_allowed(false);
        let error = persist_refreshed_for_source(
            &refreshed_credentials("new-access"),
            &CredentialSource::Keyring("sbs-1023-user".into()),
            "old-refresh",
        )
        .expect_err("disabled keychain must not write");
        assert!(
            error.to_string().contains("SBS-1023"),
            "expected the opt-out error, got {error}"
        );
    }
}
