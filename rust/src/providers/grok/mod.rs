//! Grok provider implementation.
//!
//! Reads SuperGrok / Grok Build usage from grok.com via:
//! - `~/.grok/auth.json` produced by `grok login` (primary, Claude/Codex-style), or
//! - browser cookies for grok.com when available.
//!
//! Billing RPC: `GrokBuildBilling/GetGrokCreditsConfig` (weekly shared usage pool).
//! Banked resets: `prod_mc_billing.ConsumerUiSvc/GetRemainingResets`.

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use reqwest::Client;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    RateWindow, SourceMode, UsageSnapshot,
};

const BILLING_ENDPOINT: &str = "https://grok.com/grok_api_v2.GrokBuildBilling/GetGrokCreditsConfig";
const RESETS_ENDPOINT: &str = "https://grok.com/prod_mc_billing.ConsumerUiSvc/GetRemainingResets";
const SUBSCRIPTIONS_ENDPOINT: &str = "https://grok.com/rest/subscriptions";
const WEEKLY_MINUTES: u32 = 7 * 24 * 60;

/// `ConsumerGetRemainingResetsResp.tokens` / `ConsumerResetToken` field numbers
/// from grok.com's `consumer_ui.proto`.
const RESET_TOKEN_FIELD: u64 = 10;
const RESET_TOKEN_ID_FIELD: u64 = 10;
const RESET_TOKEN_END_FIELD: u64 = 30;

/// `GetGrokCreditsConfig` / `GrokCreditsConfig` field numbers from grok.com's
/// billing proto (live SuperGrok Heavy responses + existing fixtures).
const BILLING_CONFIG_FIELD: u64 = 1;
const CREDIT_USAGE_PERCENT_FIELD: u64 = 1;
const PERIOD_START_FIELD: u64 = 4;
const PERIOD_END_FIELD: u64 = 5;

/// `google.protobuf.Timestamp.seconds`.
const TIMESTAMP_SECONDS_FIELD: u64 = 1;

/// `Cent` / web-client Money `val` (USD cents). Proto3 first field; grok.com
/// JSON is `{ "val": <cents> }`. The config-level `prepaid_balance` field
/// number is not published in grok.com comments or this repo's fixtures.
const MONEY_VAL_FIELD: u64 = 1;

/// Whether a usable `~/.grok/auth.json` (or `$GROK_HOME/auth.json`) exists.
/// True when an access token or refresh token is present (expired access is OK
/// if we can refresh, same idea as Claude OAuth).
pub fn local_credentials_available() -> bool {
    GrokCredentials::load_from_disk()
        .map(|creds| !creds.access_token.is_empty() || creds.refresh_token.is_some())
        .unwrap_or(false)
}

/// Whether the `grok` CLI appears on PATH or in known Windows install locations.
pub fn cli_installed() -> bool {
    which::which("grok").is_ok()
        || GrokProvider::detect_cli_version().is_some()
        || dirs::data_local_dir().is_some_and(|base| {
            base.join("Programs")
                .join("grok")
                .join("grok.exe")
                .is_file()
                || base.join("grok").join("grok.exe").is_file()
        })
        || std::env::var_os("USERPROFILE").is_some_and(|home| {
            PathBuf::from(home)
                .join(".grok")
                .join("bin")
                .join("grok.exe")
                .is_file()
        })
}

pub struct GrokProvider {
    metadata: ProviderMetadata,
    client: Client,
}

impl GrokProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::Grok,
                display_name: "Grok",
                // Primary meter is the shared SuperGrok / Grok Build weekly
                // pool. Bridge uses weekly_label when window_minutes is weekly
                // cadence (~7d), so both labels must read "Weekly" — not
                // "Extra credits" (that was only meant for optional prepaid).
                session_label: "Weekly",
                weekly_label: "Weekly",
                supports_opus: false,
                // Prepaid balance may surface later as a secondary meter; the
                // main product surface is the weekly usage pool.
                supports_credits: true,
                default_enabled: true,
                is_primary: true,
                dashboard_url: Some("https://grok.com/?_s=usage"),
                status_page_url: Some("https://status.x.ai"),
            },
            client: crate::core::credentialed_http_client_builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    fn auth_file_path() -> Option<PathBuf> {
        if let Ok(home) = std::env::var("GROK_HOME")
            && !home.trim().is_empty()
        {
            return Some(PathBuf::from(home).join("auth.json"));
        }
        dirs::home_dir().map(|home| home.join(".grok").join("auth.json"))
    }

    /// Load credentials and refresh the access token when expired (or about to).
    async fn load_fresh_credentials(&self) -> Result<GrokCredentials, ProviderError> {
        let mut credentials = GrokCredentials::load_from_disk()?;
        if credentials.needs_refresh() {
            credentials = self.refresh_and_persist(credentials).await?;
        }
        if credentials.access_token.is_empty() {
            return Err(ProviderError::AuthRequired);
        }
        Ok(credentials)
    }

    async fn refresh_and_persist(
        &self,
        current: GrokCredentials,
    ) -> Result<GrokCredentials, ProviderError> {
        let Some(refresh_token) = current.refresh_token.as_deref().filter(|s| !s.is_empty()) else {
            return Err(ProviderError::AuthRequired);
        };
        let client_id = current
            .oidc_client_id
            .clone()
            .filter(|s| !s.is_empty())
            .ok_or(ProviderError::AuthRequired)?;
        let token_url = grok_token_url(current.oidc_issuer.as_deref())?;

        let response = self
            .client
            .post(token_url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "grant_type=refresh_token&refresh_token={}&client_id={}",
                urlencoding_form(refresh_token),
                urlencoding_form(&client_id),
            ))
            .send()
            .await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            tracing::warn!(
                "Grok token refresh failed ({status}): {}",
                text.chars().take(160).collect::<String>()
            );
            return Err(ProviderError::AuthRequired);
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(format!("Grok refresh response: {e}")))?;
        let access_token = body
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(ProviderError::AuthRequired)?
            .to_string();
        let issued_refresh = body
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned);
        let ttl = body
            .get("expires_in")
            .and_then(Value::as_i64)
            .filter(|v| *v > 0)
            .unwrap_or(21_600);
        let expires_at = Some(Utc::now() + chrono::Duration::seconds(ttl));
        let exchanged_refresh = refresh_token.to_string();
        let mut next = current;
        next.access_token = access_token;
        next.rotated_refresh_token = issued_refresh.clone();
        next.refresh_token = issued_refresh.or(next.refresh_token);
        next.expires_at = expires_at;
        match next.persist_to_disk(&exchanged_refresh) {
            Ok(GrokPersistOutcome::Written) => Ok(next),
            Ok(GrokPersistOutcome::AuthFileMissing) => {
                // The user ran `grok logout` while the refresh was in flight.
                // Do not recreate auth.json or keep using the new session.
                tracing::debug!(
                    "Grok auth.json disappeared during refresh; treating as signed out"
                );
                Err(ProviderError::AuthRequired)
            }
            Ok(GrokPersistOutcome::Rotated(live)) => {
                tracing::debug!("Another process refreshed this Grok seat first; using its tokens");
                if live.access_token.is_empty() {
                    Err(ProviderError::AuthRequired)
                } else {
                    Ok(*live)
                }
            }
            Err(err) => {
                tracing::warn!("Grok token refreshed but could not persist auth.json: {err}");
                Ok(next)
            }
        }
    }

    async fn fetch_with_auth(
        &self,
        credentials: &GrokCredentials,
        source_label: &str,
    ) -> Result<ProviderFetchResult, ProviderError> {
        let auth_header = format!("Bearer {}", credentials.access_token);
        let billing = match self.fetch_billing(Some(auth_header.clone()), None).await {
            Ok(billing) => billing,
            Err(ProviderError::AuthRequired) if credentials.refresh_token.is_some() => {
                // Access token rejected; force one refresh and retry once.
                let refreshed = self.refresh_and_persist(credentials.clone()).await?;
                let retry_header = format!("Bearer {}", refreshed.access_token);
                let billing = self.fetch_billing(Some(retry_header.clone()), None).await?;
                let plan = self
                    .fetch_plan_name(Some(retry_header.clone()), None)
                    .await
                    .or_else(|| refreshed.login_method());
                return Ok(self
                    .with_remaining_resets(
                        result_from_billing(
                            billing,
                            source_label,
                            refreshed.email.clone(),
                            refreshed.team_id.clone(),
                            plan,
                        ),
                        Some(retry_header),
                        None,
                    )
                    .await);
            }
            Err(e) => return Err(e),
        };
        let plan = self
            .fetch_plan_name(Some(auth_header.clone()), None)
            .await
            .or_else(|| credentials.login_method());
        Ok(self
            .with_remaining_resets(
                result_from_billing(
                    billing,
                    source_label,
                    credentials.email.clone(),
                    credentials.team_id.clone(),
                    plan,
                ),
                Some(auth_header),
                None,
            )
            .await)
    }

    async fn fetch_with_cookie(
        &self,
        cookie_header: &str,
    ) -> Result<ProviderFetchResult, ProviderError> {
        let billing = self
            .fetch_billing(None, Some(cookie_header.to_string()))
            .await?;
        let plan = self
            .fetch_plan_name(None, Some(cookie_header.to_string()))
            .await;
        Ok(self
            .with_remaining_resets(
                result_from_billing(billing, "grok-browser", None, None, plan),
                None,
                Some(cookie_header.to_string()),
            )
            .await)
    }

    async fn with_remaining_resets(
        &self,
        mut result: ProviderFetchResult,
        authorization: Option<String>,
        cookie_header: Option<String>,
    ) -> ProviderFetchResult {
        match self
            .fetch_remaining_resets(authorization, cookie_header)
            .await
        {
            Ok(count) => result.usage.reset_credits_available = Some(count),
            Err(err) => {
                tracing::debug!("Grok remaining resets unavailable: {err}");
            }
        }
        result
    }

    async fn fetch_billing(
        &self,
        authorization: Option<String>,
        cookie_header: Option<String>,
    ) -> Result<GrokBillingSnapshot, ProviderError> {
        let (headers, bytes) = self
            .post_grpc_web(BILLING_ENDPOINT, authorization, cookie_header, "billing")
            .await?;
        validate_grpc_headers(&headers)?;
        parse_grpc_web_response(&bytes)
    }

    async fn fetch_remaining_resets(
        &self,
        authorization: Option<String>,
        cookie_header: Option<String>,
    ) -> Result<u32, ProviderError> {
        let (headers, bytes) = self
            .post_grpc_web(RESETS_ENDPOINT, authorization, cookie_header, "resets")
            .await?;
        validate_grpc_headers(&headers)?;
        parse_remaining_resets(&bytes)
    }

    async fn post_grpc_web(
        &self,
        url: &str,
        authorization: Option<String>,
        cookie_header: Option<String>,
        label: &str,
    ) -> Result<(reqwest::header::HeaderMap, Vec<u8>), ProviderError> {
        let mut request = self
            .client
            .post(url)
            .body(vec![0, 0, 0, 0, 0])
            .header("Origin", "https://grok.com")
            .header("Referer", "https://grok.com/?_s=usage")
            .header("Accept", "*/*")
            .header("Content-Type", "application/grpc-web+proto")
            .header("x-grpc-web", "1")
            .header("x-user-agent", "connect-es/2.1.1")
            .header("User-Agent", "Ceiling");
        if let Some(auth) = authorization {
            request = request.header("Authorization", auth);
        }
        if let Some(cookie) = cookie_header {
            request = request.header("Cookie", cookie);
        }

        let response = request.send().await?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return Err(ProviderError::AuthRequired);
            }
            return Err(ProviderError::Other(format!(
                "Grok web {label} returned status {status}"
            )));
        }
        Ok((headers, bytes.to_vec()))
    }

    /// Best-effort plan label from grok.com (e.g. SuperGrok Heavy).
    async fn fetch_plan_name(
        &self,
        authorization: Option<String>,
        cookie_header: Option<String>,
    ) -> Option<String> {
        let mut request = self
            .client
            .get(SUBSCRIPTIONS_ENDPOINT)
            .header("Origin", "https://grok.com")
            .header("Referer", "https://grok.com/?_s=usage")
            .header("Accept", "application/json")
            .header("User-Agent", "Ceiling");
        if let Some(auth) = authorization {
            request = request.header("Authorization", auth);
        }
        if let Some(cookie) = cookie_header {
            request = request.header("Cookie", cookie);
        }
        let response = request.send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let value: Value = response.json().await.ok()?;
        plan_name_from_subscriptions(&value)
    }

    /// Local CLI auth (used for Auto fallback, Cli, and OAuth source modes).
    async fn fetch_local_cli_auth(&self) -> Result<ProviderFetchResult, ProviderError> {
        let credentials = self.load_fresh_credentials().await?;
        self.fetch_with_auth(&credentials, "cli").await
    }

    /// Prefer a manually supplied cookie, then `grok login` credentials.
    async fn fetch_auto(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        if let Some(ref cookie_header) = ctx.manual_cookie_header {
            match self.fetch_with_cookie(cookie_header).await {
                Ok(result) => return Ok(result),
                Err(ProviderError::AuthRequired) => {}
                Err(e) => return Err(e),
            }
        }
        match self.fetch_local_cli_auth().await {
            Ok(result) => return Ok(result),
            Err(ProviderError::AuthRequired) | Err(ProviderError::NotInstalled(_)) => {}
            Err(e) => return Err(e),
        }
        Err(ProviderError::AuthRequired)
    }

    fn detect_cli_version() -> Option<String> {
        let mut candidates = vec![PathBuf::from("grok")];
        if let Some(home) = std::env::var_os("USERPROFILE") {
            candidates.push(
                PathBuf::from(home)
                    .join(".grok")
                    .join("bin")
                    .join("grok.exe"),
            );
        }
        if let Some(base) = dirs::data_local_dir() {
            candidates.push(base.join("Programs").join("grok").join("grok.exe"));
            candidates.push(base.join("grok").join("grok.exe"));
        }
        for bin in candidates {
            let mut command = std::process::Command::new(&bin);
            command.arg("--version");
            let Some(output) = crate::providers::cli_probe::output_with_timeout(
                &mut command,
                crate::providers::cli_probe::PROBE_TIMEOUT,
            ) else {
                continue;
            };
            let text = String::from_utf8_lossy(&output.stdout);
            let trimmed = text
                .lines()
                .next()
                .map(str::trim)
                .unwrap_or("")
                .strip_prefix("grok ")
                .unwrap_or(text.trim());
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        None
    }
}

/// OIDC issuers Ceiling will send a Grok refresh token to.
const ALLOWED_OIDC_ISSUER_HOSTS: &[&str] = &["auth.x.ai", "accounts.x.ai"];
const DEFAULT_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";

/// Token endpoint for the `oidc_issuer` recorded in `auth.json`.
///
/// The refresh token is a long-lived credential, so it only goes to an HTTPS
/// x.ai issuer with no userinfo, port, query or fragment. A tampered or
/// unexpected issuer fails instead of leaking the token elsewhere.
fn grok_token_url(issuer: Option<&str>) -> Result<String, ProviderError> {
    let Some(issuer) = issuer.map(str::trim).filter(|issuer| !issuer.is_empty()) else {
        return Ok(DEFAULT_TOKEN_URL.to_string());
    };
    let rejected = || {
        ProviderError::OAuth(
            "Grok auth.json names an unexpected sign-in issuer. Run `grok login` again."
                .to_string(),
        )
    };
    let url = reqwest::Url::parse(issuer).map_err(|_| rejected())?;
    let host = url.host_str().map(str::to_ascii_lowercase);
    let allowed = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(url.path(), "" | "/")
        && host
            .as_deref()
            .is_some_and(|host| ALLOWED_OIDC_ISSUER_HOSTS.contains(&host));
    if !allowed {
        return Err(rejected());
    }
    Ok(format!("https://{}/oauth2/token", host.unwrap_or_default()))
}

/// Minimal form-encoding for OAuth refresh (tokens are base64url-safe).
fn urlencoding_form(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl Default for GrokProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for GrokProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Grok
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        match ctx.source_mode {
            // Default shell path for Grok with no pasted cookie is often Cli;
            // treat it like Gemini/Codex: use local `grok login` credentials.
            SourceMode::Auto | SourceMode::Web => self.fetch_auto(ctx).await,
            SourceMode::Cli | SourceMode::OAuth => self.fetch_local_cli_auth().await,
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![
            SourceMode::Auto,
            SourceMode::Web,
            SourceMode::Cli,
            SourceMode::OAuth,
        ]
    }

    fn supports_web(&self) -> bool {
        true
    }

    fn supports_cli(&self) -> bool {
        true
    }

    fn supports_oauth(&self) -> bool {
        true
    }

    fn detect_version(&self) -> Option<String> {
        Self::detect_cli_version()
    }
}

#[derive(Debug, Clone)]
struct GrokCredentials {
    scope: String,
    access_token: String,
    refresh_token: Option<String>,
    /// Set only when the token endpoint returned a replacement refresh token.
    rotated_refresh_token: Option<String>,
    auth_mode: Option<String>,
    email: Option<String>,
    team_id: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    oidc_issuer: Option<String>,
    oidc_client_id: Option<String>,
}

impl GrokCredentials {
    fn load_from_disk() -> Result<Self, ProviderError> {
        let path = GrokProvider::auth_file_path()
            .ok_or_else(|| ProviderError::NotInstalled("Grok auth path not found".to_string()))?;
        let text = std::fs::read_to_string(&path).map_err(|_| {
            ProviderError::NotInstalled("Grok auth.json not found. Run `grok login`.".to_string())
        })?;
        Self::parse(&text)
    }

    fn parse(text: &str) -> Result<Self, ProviderError> {
        let root: Value = serde_json::from_str(text)
            .map_err(|e| ProviderError::Parse(format!("Failed to decode Grok auth.json: {e}")))?;
        let map = root
            .as_object()
            .ok_or_else(|| ProviderError::Parse("Invalid Grok auth.json".to_string()))?;
        let (scope, entry) = select_auth_scope(map).ok_or(ProviderError::AuthRequired)?;
        let access_token = entry
            .get("key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();
        let refresh_token = text_field(entry, "refresh_token");
        if access_token.is_empty() && refresh_token.is_none() {
            return Err(ProviderError::AuthRequired);
        }
        let expires_at = entry
            .get("expires_at")
            .and_then(Value::as_str)
            .and_then(parse_expires_at);
        Ok(Self {
            scope,
            access_token,
            refresh_token,
            rotated_refresh_token: None,
            auth_mode: text_field(entry, "auth_mode"),
            email: text_field(entry, "email"),
            team_id: text_field(entry, "team_id"),
            expires_at,
            oidc_issuer: text_field(entry, "oidc_issuer"),
            oidc_client_id: text_field(entry, "oidc_client_id"),
        })
    }

    fn needs_refresh(&self) -> bool {
        if self.refresh_token.as_ref().is_none_or(|s| s.is_empty()) {
            return false;
        }
        if self.access_token.is_empty() {
            return true;
        }
        match self.expires_at {
            Some(exp) => exp <= Utc::now() + chrono::Duration::minutes(2),
            // Unknown expiry: still try refresh when billing returns 401.
            None => false,
        }
    }

    fn persist_to_disk(&self, exchanged_refresh_token: &str) -> Result<GrokPersistOutcome, String> {
        let path = GrokProvider::auth_file_path().ok_or_else(|| "no auth path".to_string())?;
        self.persist_to_path(&path, exchanged_refresh_token)
    }

    /// Write refreshed tokens back to `auth.json`.
    ///
    /// `exchanged_refresh_token` is the token this process sent to x.ai. It is
    /// a compare-and-swap guard: when the file now holds a different refresh
    /// token, another process (usually the Grok CLI) rotated the seat first,
    /// so its live tokens are returned instead of being clobbered. A missing
    /// file means the user signed out and is never recreated.
    fn persist_to_path(
        &self,
        path: &Path,
        exchanged_refresh_token: &str,
    ) -> Result<GrokPersistOutcome, String> {
        // Check before taking the lock: the lock helper creates the parent
        // directory, which would partly resurrect a removed ~/.grok.
        match std::fs::symlink_metadata(path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(GrokPersistOutcome::AuthFileMissing);
            }
            Err(error) => return Err(format!("update auth.json: {error}")),
        }
        crate::secure_file::with_file_write_lock(path, || {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(GrokPersistOutcome::AuthFileMissing);
                }
                Err(error) => return Err(error),
            };
            let mut root: Value = serde_json::from_str(&text)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            if root.as_object().is_some_and(|map| map.is_empty()) {
                return Ok(GrokPersistOutcome::AuthFileMissing);
            }
            if let Some(disk_refresh) = disk_refresh_token(&root, &self.scope)
                && disk_refresh != exchanged_refresh_token.trim()
            {
                let live = GrokCredentials::parse(&text).map_err(|error| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
                })?;
                return Ok(GrokPersistOutcome::Rotated(Box::new(live)));
            }
            apply_refresh_to_auth_json(&mut root, self)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            let encoded = serde_json::to_vec_pretty(&root).map_err(std::io::Error::other)?;
            crate::secure_file::atomic_write_preserving_permissions(path, &encoded)?;
            Ok(GrokPersistOutcome::Written)
        })
        .map_err(|error| format!("update auth.json: {error}"))
    }

    fn login_method(&self) -> Option<String> {
        match self.auth_mode.as_deref().map(str::to_lowercase).as_deref() {
            Some("oidc") => Some("SuperGrok".to_string()),
            Some("session") => Some("session".to_string()),
            Some(other) => Some(other.to_string()),
            None if self.expires_at.is_some() => Some("Grok".to_string()),
            None => None,
        }
    }
}

/// What happened when refreshed Grok tokens were written back.
#[derive(Debug)]
enum GrokPersistOutcome {
    Written,
    /// `auth.json` is gone (signed out); nothing was written.
    AuthFileMissing,
    /// Another process rotated the refresh token first; these are its tokens.
    Rotated(Box<GrokCredentials>),
}

/// The usable refresh token stored for `scope`, if any.
fn disk_refresh_token<'a>(root: &'a Value, scope: &str) -> Option<&'a str> {
    root.get(scope)?
        .get("refresh_token")?
        .as_str()
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// Usable `auth.json` seats. Top-level non-objects (`active`, `future_root`, …)
/// are not credentials. Object key order is not account identity.
///
/// Priority: explicit active marker, then newest parseable `expires_at`, then
/// lexicographically smaller scope. When nothing is marked active, prefer
/// `https://auth.x.ai::` seats over leftover `/sign-in` sessions.
fn select_auth_scope(map: &serde_json::Map<String, Value>) -> Option<(String, &Value)> {
    let usable: Vec<UsableAuthScope<'_>> = map
        .iter()
        .filter_map(|(scope, entry)| usable_auth_scope(scope, entry))
        .collect();
    if usable.is_empty() {
        return None;
    }

    let named_active: Vec<&str> = ["active", "active_account", "active_scope", "current"]
        .into_iter()
        .filter_map(|key| {
            map.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .collect();

    let active: Vec<&UsableAuthScope<'_>> = usable
        .iter()
        .filter(|candidate| candidate.marked_active || named_active.contains(&candidate.scope))
        .collect();

    let xai: Vec<&UsableAuthScope<'_>> = usable
        .iter()
        .filter(|candidate| candidate.scope.starts_with("https://auth.x.ai::"))
        .collect();

    let pool = if !active.is_empty() {
        active
    } else if !xai.is_empty() {
        xai
    } else {
        usable.iter().collect()
    };

    pool.into_iter()
        .max_by(|a, b| better_auth_scope(a, b))
        .map(|best| (best.scope.to_string(), best.entry))
}

struct UsableAuthScope<'a> {
    scope: &'a str,
    entry: &'a Value,
    expires_at: Option<DateTime<Utc>>,
    marked_active: bool,
}

fn usable_auth_scope<'a>(scope: &'a str, entry: &'a Value) -> Option<UsableAuthScope<'a>> {
    if !entry.is_object() {
        return None;
    }
    let has_key = entry
        .get("key")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    let has_refresh = entry
        .get("refresh_token")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if !(has_key || has_refresh) {
        return None;
    }
    Some(UsableAuthScope {
        scope,
        entry,
        expires_at: entry
            .get("expires_at")
            .and_then(Value::as_str)
            .and_then(parse_expires_at),
        marked_active: entry.get("active").and_then(Value::as_bool) == Some(true),
    })
}

fn better_auth_scope(a: &UsableAuthScope<'_>, b: &UsableAuthScope<'_>) -> std::cmp::Ordering {
    a.expires_at
        .cmp(&b.expires_at)
        .then_with(|| b.scope.cmp(a.scope))
}

fn apply_refresh_to_auth_json(
    root: &mut Value,
    credentials: &GrokCredentials,
) -> Result<(), String> {
    let map = root
        .as_object_mut()
        .ok_or_else(|| "Grok auth.json root is not an object".to_string())?;
    if !map.contains_key(&credentials.scope) {
        if !map.is_empty() {
            return Err("auth scope missing".to_string());
        }
        map.insert(
            credentials.scope.clone(),
            Value::Object(serde_json::Map::new()),
        );
    }
    let entry = map
        .get_mut(&credentials.scope)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "auth scope is not an object".to_string())?;
    entry.insert(
        "key".to_string(),
        Value::String(credentials.access_token.clone()),
    );
    let disk_refresh = entry
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty());
    let persist_refresh = credentials.rotated_refresh_token.as_ref().or_else(|| {
        if disk_refresh.is_some() {
            None
        } else {
            credentials.refresh_token.as_ref()
        }
    });
    if let Some(refresh) = persist_refresh {
        entry.insert("refresh_token".to_string(), Value::String(refresh.clone()));
    }
    if let Some(exp) = credentials.expires_at {
        entry.insert(
            "expires_at".to_string(),
            Value::String(exp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        );
    }
    for (field, value) in [
        ("auth_mode", credentials.auth_mode.as_ref()),
        ("email", credentials.email.as_ref()),
        ("team_id", credentials.team_id.as_ref()),
        ("oidc_issuer", credentials.oidc_issuer.as_ref()),
        ("oidc_client_id", credentials.oidc_client_id.as_ref()),
    ] {
        if let Some(value) = value
            && !entry.contains_key(field)
        {
            entry.insert(field.to_string(), Value::String(value.clone()));
        }
    }
    Ok(())
}

fn parse_expires_at(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S"))
                .ok()
                .map(|naive| naive.and_utc())
        })
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

/// Active subscription tier ranking (higher wins). Matches grok.com labels.
fn plan_name_from_subscriptions(root: &Value) -> Option<String> {
    let list = root.get("subscriptions")?.as_array()?;
    let mut best: Option<(i32, String)> = None;
    for sub in list {
        let status = sub.get("status").and_then(Value::as_str).unwrap_or("");
        if status != "SUBSCRIPTION_STATUS_ACTIVE" {
            continue;
        }
        let tier = sub.get("tier").and_then(Value::as_str).unwrap_or("");
        let (rank, label) = match tier {
            "SUBSCRIPTION_TIER_SUPER_GROK_PRO" => (60, "SuperGrok Heavy"),
            "SUBSCRIPTION_TIER_GROK_PRO" => (50, "SuperGrok"),
            "SUBSCRIPTION_TIER_SUPER_GROK_LITE" => (40, "SuperGrok Lite"),
            "SUBSCRIPTION_TIER_X_PREMIUM_PLUS" => (30, "X Premium+"),
            "SUBSCRIPTION_TIER_X_PREMIUM" => (20, "X Premium"),
            "SUBSCRIPTION_TIER_X_BASIC" => (10, "X Basic"),
            _ => continue,
        };
        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
            best = Some((rank, label.to_string()));
        }
    }
    best.map(|(_, label)| label)
}

#[derive(Debug, Clone)]
struct GrokBillingSnapshot {
    used_percent: f64,
    resets_at: Option<DateTime<Utc>>,
    window_minutes: Option<u32>,
    /// Extra usage credits remaining, when the RPC reports a prepaid balance.
    prepaid_balance_cents: Option<u64>,
}

fn result_from_billing(
    billing: GrokBillingSnapshot,
    source_label: &str,
    email: Option<String>,
    team_id: Option<String>,
    login_method: Option<String>,
) -> ProviderFetchResult {
    let mut usage = UsageSnapshot::new(RateWindow::with_details(
        billing.used_percent,
        billing.window_minutes.or(Some(WEEKLY_MINUTES)),
        billing.resets_at,
        None,
    ));
    // Optional prepaid balance (not yet decoded from the RPC). When present,
    // surface it as a secondary meter. Absolute balances have no used%; show
    // 0% used when balance > 0 so the bar reads as "have credits". The secondary
    // UI label still comes from metadata.weekly_label ("Weekly") today — the
    // dollar amount is carried in reset_description until prepaid is productized.
    if let Some(cents) = billing.prepaid_balance_cents.filter(|c| *c > 0) {
        let dollars = cents as f64 / 100.0;
        usage = usage.with_secondary(RateWindow::with_details(
            0.0,
            None,
            None,
            Some(format!("${dollars:.2} extra credits")),
        ));
    }
    usage.account_email = email;
    usage.account_organization = team_id;
    usage.login_method = login_method;
    ProviderFetchResult::new(usage, source_label)
}

fn validate_grpc_headers(headers: &reqwest::header::HeaderMap) -> Result<(), ProviderError> {
    if let Some(status) = headers
        .get("grpc-status")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u16>().ok())
        && status != 0
    {
        if status == 16 {
            return Err(ProviderError::AuthRequired);
        }
        return Err(ProviderError::Other(format!(
            "Grok RPC failed with status {status}"
        )));
    }
    Ok(())
}

fn parse_grpc_web_response(data: &[u8]) -> Result<GrokBillingSnapshot, ProviderError> {
    let frames = grpc_web_data_frames(data);
    if frames.is_empty() {
        return Err(ProviderError::Parse(
            "Grok web billing returned no payload".to_string(),
        ));
    }
    let mut used_percent = None;
    let mut period_start = None;
    let mut period_end = None;
    for frame in frames {
        let parsed = parse_credits_config_response(&frame)?;
        if parsed.used_percent.is_some() {
            used_percent = parsed.used_percent;
        }
        if parsed.period_start.is_some() {
            period_start = parsed.period_start;
        }
        if parsed.period_end.is_some() {
            period_end = parsed.period_end;
        }
    }

    // grok.com UI maps config.creditUsagePercent. Zero-usage responses often
    // omit the float entirely (protobuf default 0), so treat a valid config
    // message without a percent as 0% rather than a hard parse failure.
    let used_percent = used_percent.unwrap_or(0.0);
    let resets_at = period_end;
    let window_minutes = weekly_window_minutes(period_start, period_end);

    // GetGrokCreditsConfig.prepaid_balance is a nested Money/Cent `{ val }`.
    // Zero balances are omitted. The config field number is not published in
    // grok.com comments or this repo's fixtures — do not invent one or scan
    // sibling varints. Decode Money.val only when that documented payload is
    // supplied to proto_money_cents.
    let prepaid_balance_cents = None;

    Ok(GrokBillingSnapshot {
        used_percent,
        resets_at,
        window_minutes,
        prepaid_balance_cents,
    })
}

struct CreditsConfigFields {
    used_percent: Option<f64>,
    period_start: Option<DateTime<Utc>>,
    period_end: Option<DateTime<Utc>>,
}

fn parse_credits_config_response(data: &[u8]) -> Result<CreditsConfigFields, ProviderError> {
    let mut fields = CreditsConfigFields {
        used_percent: None,
        period_start: None,
        period_end: None,
    };
    if data.is_empty() {
        return Ok(fields);
    }
    for field in proto_fields(data)? {
        if field.number == BILLING_CONFIG_FIELD && field.wire == 2 {
            merge_credits_config(&mut fields, field.bytes)?;
        }
    }
    Ok(fields)
}

fn merge_credits_config(
    fields: &mut CreditsConfigFields,
    data: &[u8],
) -> Result<(), ProviderError> {
    for field in proto_fields(data)? {
        match (field.number, field.wire) {
            (CREDIT_USAGE_PERCENT_FIELD, 5) => {
                if let Some(value) = proto_float32(field.bytes) {
                    fields.used_percent = Some(value as f64);
                }
            }
            (PERIOD_START_FIELD, 2) => {
                fields.period_start = proto_timestamp(field.bytes)?;
            }
            (PERIOD_END_FIELD, 2) => {
                fields.period_end = proto_timestamp(field.bytes)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn weekly_window_minutes(
    period_start: Option<DateTime<Utc>>,
    period_end: Option<DateTime<Utc>>,
) -> Option<u32> {
    match (period_start, period_end) {
        (Some(start), Some(end)) => {
            let days = end.signed_duration_since(start).num_days().unsigned_abs();
            (6..=8).contains(&days).then_some(WEEKLY_MINUTES)
        }
        // Current product often ships period-end alone; still label weekly.
        (None, Some(_)) => Some(WEEKLY_MINUTES),
        _ => None,
    }
}

fn proto_float32(data: &[u8]) -> Option<f32> {
    let bytes: [u8; 4] = data.try_into().ok()?;
    let value = f32::from_le_bytes(bytes);
    value.is_finite().then_some(value)
}

/// Decode a grok.com Money/Cent message (`val` = field 1, USD cents).
/// Omitted or zero `val` is proto3 default 0 → `None`.
#[allow(dead_code)] // wired once grok.com publishes prepaid_balance's field number
fn proto_money_cents(data: &[u8]) -> Result<Option<u64>, ProviderError> {
    for field in proto_fields(data)? {
        if field.number == MONEY_VAL_FIELD && field.wire == 0 {
            return Ok((field.varint > 0).then_some(field.varint));
        }
    }
    Ok(None)
}

/// Count still-valid banked resets from `GetRemainingResets`.
///
/// A known zero is an empty grpc-web data frame (optionally with grpc-status 0).
/// Tokens missing an id or whose `validity_end` is in the past are ignored,
/// matching grok.com's own filter. Trailer errors, unframed bodies, truncated
/// proto, and messages with no `tokens` field stay unknown rather than 0.
fn parse_remaining_resets(data: &[u8]) -> Result<u32, ProviderError> {
    let body = grpc_web_split(data)?;
    validate_grpc_status_trailers(&body.trailers)?;
    if body.frames.is_empty() {
        return Err(ProviderError::Parse(
            "Grok remaining resets returned no payload".to_string(),
        ));
    }
    let now = Utc::now();
    let mut count = 0u32;
    for frame in &body.frames {
        if frame.is_empty() {
            continue;
        }
        let (n, saw_tokens) = count_reset_tokens(frame, now)?;
        if !saw_tokens {
            return Err(ProviderError::Parse(
                "Grok remaining resets payload had no reset tokens".to_string(),
            ));
        }
        count = count.saturating_add(n);
    }
    Ok(count)
}

fn validate_grpc_status_trailers(trailers: &[Vec<u8>]) -> Result<(), ProviderError> {
    for trailer in trailers {
        if let Some(status) = grpc_status_from_trailer_block(trailer)
            && status != 0
        {
            if status == 16 {
                return Err(ProviderError::AuthRequired);
            }
            return Err(ProviderError::Other(format!(
                "Grok RPC failed with status {status}"
            )));
        }
    }
    Ok(())
}

fn grpc_status_from_trailer_block(block: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(block).ok()?;
    for line in text.split(['\r', '\n']) {
        let line = line.trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.eq_ignore_ascii_case("grpc-status") {
            return value.trim().parse().ok();
        }
    }
    None
}

fn count_reset_tokens(data: &[u8], now: DateTime<Utc>) -> Result<(u32, bool), ProviderError> {
    let mut count = 0u32;
    let mut saw_tokens = false;
    for field in proto_fields(data)? {
        if field.number == RESET_TOKEN_FIELD && field.wire == 2 {
            saw_tokens = true;
            if reset_token_is_available(field.bytes, now)? {
                count = count.saturating_add(1);
            }
        }
    }
    Ok((count, saw_tokens))
}

fn reset_token_is_available(data: &[u8], now: DateTime<Utc>) -> Result<bool, ProviderError> {
    let mut token_id = None;
    let mut validity_end = None;
    for field in proto_fields(data)? {
        match (field.number, field.wire) {
            (RESET_TOKEN_ID_FIELD, 2) => {
                token_id = std::str::from_utf8(field.bytes)
                    .ok()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned);
            }
            (RESET_TOKEN_END_FIELD, 2) => {
                validity_end = proto_timestamp(field.bytes)?;
            }
            _ => {}
        }
    }
    Ok(token_id.is_some() && validity_end.is_some_and(|end| end > now))
}

fn proto_timestamp(data: &[u8]) -> Result<Option<DateTime<Utc>>, ProviderError> {
    for field in proto_fields(data)? {
        if field.number == TIMESTAMP_SECONDS_FIELD && field.wire == 0 {
            return Ok(Utc.timestamp_opt(field.varint as i64, 0).single());
        }
    }
    Ok(None)
}

struct ProtoField<'a> {
    number: u64,
    wire: u64,
    varint: u64,
    bytes: &'a [u8],
}

fn proto_fields(data: &[u8]) -> Result<Vec<ProtoField<'_>>, ProviderError> {
    let mut fields = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let Some((key, next)) = read_varint(data, i) else {
            return Err(resets_parse_error());
        };
        i = next;
        let number = key >> 3;
        let wire = key & 0x07;
        match wire {
            0 => {
                let Some((value, next)) = read_varint(data, i) else {
                    return Err(resets_parse_error());
                };
                i = next;
                fields.push(ProtoField {
                    number,
                    wire,
                    varint: value,
                    bytes: &[],
                });
            }
            1 => {
                let end = i.saturating_add(8);
                if end > data.len() {
                    return Err(resets_parse_error());
                }
                fields.push(ProtoField {
                    number,
                    wire,
                    varint: 0,
                    bytes: &data[i..end],
                });
                i = end;
            }
            2 => {
                let Some((len, next)) = read_varint(data, i) else {
                    return Err(resets_parse_error());
                };
                i = next;
                let end = i.saturating_add(len as usize);
                if end > data.len() {
                    return Err(resets_parse_error());
                }
                fields.push(ProtoField {
                    number,
                    wire,
                    varint: 0,
                    bytes: &data[i..end],
                });
                i = end;
            }
            5 => {
                let end = i.saturating_add(4);
                if end > data.len() {
                    return Err(resets_parse_error());
                }
                fields.push(ProtoField {
                    number,
                    wire,
                    varint: 0,
                    bytes: &data[i..end],
                });
                i = end;
            }
            _ => return Err(resets_parse_error()),
        }
    }
    Ok(fields)
}

fn resets_parse_error() -> ProviderError {
    ProviderError::Parse("Grok remaining resets payload is malformed".to_string())
}

fn grpc_web_data_frames(data: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut index = 0;
    while index + 5 <= data.len() {
        let flags = data[index];
        let len = ((data[index + 1] as usize) << 24)
            | ((data[index + 2] as usize) << 16)
            | ((data[index + 3] as usize) << 8)
            | (data[index + 4] as usize);
        let start = index + 5;
        let end = start.saturating_add(len);
        if end > data.len() {
            break;
        }
        if flags & 0x80 == 0 {
            frames.push(data[start..end].to_vec());
        }
        index = end;
    }
    frames
}

struct GrpcWebBody {
    frames: Vec<Vec<u8>>,
    trailers: Vec<Vec<u8>>,
}

fn grpc_web_split(data: &[u8]) -> Result<GrpcWebBody, ProviderError> {
    let mut frames = Vec::new();
    let mut trailers = Vec::new();
    let mut index = 0;
    while index + 5 <= data.len() {
        let flags = data[index];
        let len = ((data[index + 1] as usize) << 24)
            | ((data[index + 2] as usize) << 16)
            | ((data[index + 3] as usize) << 8)
            | (data[index + 4] as usize);
        let start = index + 5;
        let end = start.saturating_add(len);
        if end > data.len() {
            return Err(ProviderError::Parse(
                "Grok remaining resets frame is truncated".to_string(),
            ));
        }
        if flags & 0x80 != 0 {
            trailers.push(data[start..end].to_vec());
        } else {
            frames.push(data[start..end].to_vec());
        }
        index = end;
    }
    if index != data.len() {
        return Err(ProviderError::Parse(
            "Grok remaining resets frame is truncated".to_string(),
        ));
    }
    Ok(GrpcWebBody { frames, trailers })
}

fn read_varint(data: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let mut value = 0u64;
    let mut shift = 0;
    while i < data.len() && shift < 64 {
        let b = data[i];
        i += 1;
        value |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Some((value, i));
        }
        shift += 7;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refreshed_credentials(access_token: &str) -> GrokCredentials {
        GrokCredentials {
            scope: "https://auth.x.ai::test".to_string(),
            access_token: access_token.to_string(),
            refresh_token: Some(format!("refresh-{access_token}")),
            rotated_refresh_token: None,
            auth_mode: Some("oidc".to_string()),
            email: Some("user@example.com".to_string()),
            team_id: Some("team".to_string()),
            expires_at: DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z")
                .ok()
                .map(|date| date.with_timezone(&Utc)),
            oidc_issuer: Some("https://auth.x.ai".to_string()),
            oidc_client_id: Some("client".to_string()),
        }
    }

    #[test]
    fn refresh_persistence_preserves_unknown_scopes_and_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            r#"{
                "https://auth.x.ai::test": {
                    "key": "old",
                    "refresh_token": "old-refresh",
                    "future_field": {"keep": true}
                },
                "https://accounts.x.ai/sign-in": {"key": "other-scope"},
                "future_root": "keep"
            }"#,
        )
        .expect("seed auth file");

        refreshed_credentials("new-access")
            .persist_to_path(&path, "old-refresh")
            .expect("persist refresh");

        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read result"))
                .expect("valid result");
        let entry = &stored["https://auth.x.ai::test"];
        assert_eq!(entry["key"], "new-access");
        assert_eq!(entry["refresh_token"], "old-refresh");
        assert_eq!(entry["future_field"]["keep"], true);
        assert_eq!(
            stored["https://accounts.x.ai/sign-in"]["key"],
            "other-scope"
        );
        assert_eq!(stored["future_root"], "keep");
    }

    #[test]
    fn concurrent_refresh_persistence_keeps_valid_complete_auth_json() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            r#"{
                "https://auth.x.ai::test": {"key":"old","unknown":"keep"},
                "other": {"key":"other"}
            }"#,
        )
        .expect("seed auth file");
        let barrier = Arc::new(Barrier::new(3));
        let writers: Vec<_> = ["refresh-a", "refresh-b"]
            .into_iter()
            .map(|access_token| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    refreshed_credentials(access_token)
                        .persist_to_path(&path, "refresh-exchanged")
                        .expect("persist concurrent refresh");
                })
            })
            .collect();

        barrier.wait();
        for writer in writers {
            writer.join().expect("writer thread");
        }

        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read result"))
                .expect("valid result");
        assert!(matches!(
            stored["https://auth.x.ai::test"]["key"].as_str(),
            Some("refresh-a" | "refresh-b")
        ));
        assert_eq!(stored["https://auth.x.ai::test"]["unknown"], "keep");
        assert_eq!(stored["other"]["key"], "other");
    }

    #[test]
    fn failed_refresh_persistence_leaves_original_auth_file_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        let original = b"{not valid json";
        std::fs::write(&path, original).expect("seed invalid auth file");

        assert!(
            refreshed_credentials("new-access")
                .persist_to_path(&path, "refresh-new-access")
                .is_err()
        );
        assert_eq!(std::fs::read(&path).expect("read original"), original);
    }

    #[test]
    fn persist_adopts_live_tokens_when_another_process_rotated_the_refresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        let original = r#"{
                "https://auth.x.ai::test": {
                    "key": "cli-access",
                    "refresh_token": "newer-disk-refresh"
                }
            }"#;
        std::fs::write(&path, original).expect("seed auth file");

        let outcome = refreshed_credentials("new-access")
            .persist_to_path(&path, "stale-exchanged-refresh")
            .expect("persist refresh");

        match outcome {
            GrokPersistOutcome::Rotated(live) => {
                assert_eq!(live.access_token, "cli-access");
                assert_eq!(live.refresh_token.as_deref(), Some("newer-disk-refresh"));
            }
            other => panic!("expected the live rotated seat, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(&path).expect("read original"),
            original,
            "a rotated seat must not be clobbered"
        );
    }

    #[test]
    fn persist_writes_when_the_exchanged_refresh_is_still_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            r#"{"https://auth.x.ai::test":{"key":"old","refresh_token":"exchanged"}}"#,
        )
        .expect("seed auth file");

        let mut credentials = refreshed_credentials("new-access");
        credentials.rotated_refresh_token = Some("rotated".to_string());
        let outcome = credentials
            .persist_to_path(&path, "exchanged")
            .expect("persist refresh");

        assert!(matches!(outcome, GrokPersistOutcome::Written));
        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read result"))
                .expect("valid result");
        assert_eq!(stored["https://auth.x.ai::test"]["key"], "new-access");
        assert_eq!(
            stored["https://auth.x.ai::test"]["refresh_token"],
            "rotated"
        );
    }

    #[test]
    fn persist_repairs_null_or_empty_refresh_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");

        for seed in [
            r#"{"https://auth.x.ai::test":{"key":"old","refresh_token":null}}"#,
            r#"{"https://auth.x.ai::test":{"key":"old","refresh_token":""}}"#,
            r#"{"https://auth.x.ai::test":{"key":"old","refresh_token":"   "}}"#,
        ] {
            std::fs::write(&path, seed).expect("seed unusable refresh token");
            refreshed_credentials("new-access")
                .persist_to_path(&path, "refresh-new-access")
                .expect("repair refresh token");
            let stored: Value =
                serde_json::from_str(&std::fs::read_to_string(&path).expect("read result"))
                    .expect("valid result");
            assert_eq!(stored["https://auth.x.ai::test"]["key"], "new-access");
            assert_eq!(
                stored["https://auth.x.ai::test"]["refresh_token"],
                "refresh-new-access"
            );
        }
    }

    #[test]
    fn persist_does_not_create_a_second_scope_when_loaded_scope_is_gone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");
        let original = r#"{
                "https://accounts.x.ai/sign-in": {"key": "session"}
            }"#;
        std::fs::write(&path, original).expect("seed other-scope auth file");

        assert!(
            refreshed_credentials("new-access")
                .persist_to_path(&path, "refresh-new-access")
                .is_err()
        );

        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read result"))
                .expect("valid result");
        assert_eq!(stored["https://accounts.x.ai/sign-in"]["key"], "session");
        assert!(stored.get("https://auth.x.ai::test").is_none());
        assert_eq!(stored.as_object().map(|map| map.len()), Some(1));
    }

    #[test]
    fn persist_does_not_recreate_auth_file_after_logout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("auth.json");

        let outcome = refreshed_credentials("new-access")
            .persist_to_path(&path, "refresh-new-access")
            .expect("a missing auth file is not an error");

        assert!(matches!(outcome, GrokPersistOutcome::AuthFileMissing));
        assert!(!path.exists(), "logout must not be undone by a refresh");
    }

    #[test]
    fn persist_does_not_recreate_a_removed_grok_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let grok_home = dir.path().join(".grok");
        let path = grok_home.join("auth.json");

        let outcome = refreshed_credentials("new-access")
            .persist_to_path(&path, "refresh-new-access")
            .expect("a missing auth directory is not an error");

        assert!(matches!(outcome, GrokPersistOutcome::AuthFileMissing));
        assert!(!grok_home.exists());
    }

    #[test]
    fn token_url_defaults_to_xai_when_issuer_is_absent() {
        assert_eq!(
            grok_token_url(None).unwrap(),
            "https://auth.x.ai/oauth2/token"
        );
        assert_eq!(
            grok_token_url(Some("  ")).unwrap(),
            "https://auth.x.ai/oauth2/token"
        );
    }

    #[test]
    fn token_url_accepts_allowlisted_https_issuers() {
        assert_eq!(
            grok_token_url(Some("https://auth.x.ai")).unwrap(),
            "https://auth.x.ai/oauth2/token"
        );
        assert_eq!(
            grok_token_url(Some("https://auth.x.ai/")).unwrap(),
            "https://auth.x.ai/oauth2/token"
        );
        assert_eq!(
            grok_token_url(Some("https://ACCOUNTS.x.ai")).unwrap(),
            "https://accounts.x.ai/oauth2/token"
        );
    }

    #[test]
    fn token_url_rejects_issuers_that_could_leak_the_refresh_token() {
        for issuer in [
            "http://auth.x.ai",
            "https://evil.test",
            "https://auth.x.ai.evil.test",
            "https://evilauth.x.ai",
            "https://auth.x.ai@evil.test",
            "https://user:example@auth.x.ai",
            "https://auth.x.ai:8443",
            "https://auth.x.ai/redirect",
            "https://auth.x.ai/?next=evil",
            "https://auth.x.ai/#frag",
            "auth.x.ai",
            "not a url",
        ] {
            assert!(matches!(
                grok_token_url(Some(issuer)),
                Err(ProviderError::OAuth(_))
            ));
        }
    }

    #[test]
    fn parses_auth_file_prefer_oidc() {
        let auth = r#"{
          "https://accounts.x.ai/sign-in": {"key": "legacy"},
          "https://auth.x.ai::abc": {
            "key": "oidc",
            "auth_mode": "oidc",
            "email": "u@example.com",
            "refresh_token": "refresh",
            "oidc_client_id": "client",
            "oidc_issuer": "https://auth.x.ai"
          }
        }"#;
        let parsed = GrokCredentials::parse(auth).unwrap();
        assert_eq!(parsed.access_token, "oidc");
        assert_eq!(parsed.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(parsed.login_method().as_deref(), Some("SuperGrok"));
        assert!(!parsed.needs_refresh());
    }

    #[test]
    fn selects_xai_scope_with_newest_expires_at_regardless_of_object_order() {
        let older = r#""https://auth.x.ai::older": {
            "key": "older-seat",
            "refresh_token": "older-refresh",
            "expires_at": "2028-01-01T00:00:00.000Z"
        }"#;
        let newer = r#""https://auth.x.ai::newer": {
            "key": "newer-seat",
            "refresh_token": "newer-refresh",
            "expires_at": "2030-06-15T12:00:00.000Z"
        }"#;
        for auth in [
            format!("{{{older},{newer}}}"),
            format!("{{{newer},{older}}}"),
        ] {
            let parsed = GrokCredentials::parse(&auth).unwrap();
            assert_eq!(parsed.scope, "https://auth.x.ai::newer");
            assert_eq!(parsed.access_token, "newer-seat");
            assert_eq!(parsed.refresh_token.as_deref(), Some("newer-refresh"));
        }
    }

    #[test]
    fn selects_lexicographically_smaller_scope_when_expires_at_ties() {
        let aaa =
            r#""https://auth.x.ai::aaa": {"key": "aaa-seat", "refresh_token": "aaa-refresh"}"#;
        let zzz =
            r#""https://auth.x.ai::zzz": {"key": "zzz-seat", "refresh_token": "zzz-refresh"}"#;
        let same_exp_aaa = r#""https://auth.x.ai::aaa": {
            "key": "aaa-seat",
            "expires_at": "2030-01-01T00:00:00.000Z"
        }"#;
        let same_exp_zzz = r#""https://auth.x.ai::zzz": {
            "key": "zzz-seat",
            "expires_at": "2030-01-01T00:00:00.000Z"
        }"#;
        let unparseable_aaa = r#""https://auth.x.ai::aaa": {
            "key": "aaa-seat",
            "expires_at": "not-a-timestamp"
        }"#;
        let unparseable_zzz = r#""https://auth.x.ai::zzz": {
            "key": "zzz-seat",
            "expires_at": "also-bad"
        }"#;
        for auth in [
            format!("{{{aaa},{zzz}}}"),
            format!("{{{zzz},{aaa}}}"),
            format!("{{{same_exp_aaa},{same_exp_zzz}}}"),
            format!("{{{same_exp_zzz},{same_exp_aaa}}}"),
            format!("{{{unparseable_aaa},{unparseable_zzz}}}"),
            format!("{{{unparseable_zzz},{unparseable_aaa}}}"),
        ] {
            let parsed = GrokCredentials::parse(&auth).unwrap();
            assert_eq!(parsed.scope, "https://auth.x.ai::aaa");
            assert_eq!(parsed.access_token, "aaa-seat");
        }
    }

    #[test]
    fn explicit_active_marker_wins_over_newer_expires_at() {
        let older_scope = "https://auth.x.ai::older";
        let newer_scope = "https://auth.x.ai::newer";
        let seats = format!(
            r#""{older_scope}": {{
                "key": "older-seat",
                "expires_at": "2028-01-01T00:00:00.000Z"
            }},
            "{newer_scope}": {{
                "key": "newer-seat",
                "expires_at": "2030-06-15T12:00:00.000Z"
            }}"#
        );
        let fixtures = [
            format!(r#"{{"active": "{older_scope}", {seats}}}"#),
            format!(r#"{{"active_account": "{older_scope}", {seats}}}"#),
            format!(r#"{{"active_scope": "{older_scope}", {seats}}}"#),
            format!(r#"{{"current": "{older_scope}", {seats}}}"#),
            format!(
                r#"{{
                    "{older_scope}": {{
                        "key": "older-seat",
                        "expires_at": "2028-01-01T00:00:00.000Z",
                        "active": true
                    }},
                    "{newer_scope}": {{
                        "key": "newer-seat",
                        "expires_at": "2030-06-15T12:00:00.000Z"
                    }}
                }}"#
            ),
        ];
        for auth in fixtures {
            let parsed = GrokCredentials::parse(&auth).unwrap();
            assert_eq!(parsed.scope, older_scope);
            assert_eq!(parsed.access_token, "older-seat");
        }
    }

    #[test]
    fn parses_sign_in_session_when_no_oidc_scope() {
        let auth = r#"{
          "future_root": "keep",
          "active": "not-a-usable-scope",
          "https://accounts.x.ai/sign-in": {
            "key": "session-token",
            "auth_mode": "session"
          }
        }"#;
        let parsed = GrokCredentials::parse(auth).unwrap();
        assert_eq!(parsed.scope, "https://accounts.x.ai/sign-in");
        assert_eq!(parsed.access_token, "session-token");
        assert_eq!(parsed.login_method().as_deref(), Some("session"));
    }

    #[test]
    fn expired_token_with_refresh_is_not_hard_fail() {
        let auth = r#"{
          "https://auth.x.ai::abc": {
            "key": "old",
            "refresh_token": "refresh",
            "oidc_client_id": "client",
            "expires_at": "2020-01-01T00:00:00.000Z"
          }
        }"#;
        let parsed = GrokCredentials::parse(auth).unwrap();
        assert!(parsed.needs_refresh());
    }

    #[test]
    fn splits_grpc_web_data_frames() {
        let data = [0, 0, 0, 0, 2, 1, 2, 0x80, 0, 0, 0, 1, b'x'];
        assert_eq!(grpc_web_data_frames(&data), vec![vec![1, 2]]);
    }

    /// Real SuperGrok Heavy zero-usage payload shape (no creditUsagePercent
    /// float; weekly window timestamps only). Must not hard-fail.
    #[test]
    fn parses_zero_usage_weekly_pool_without_percent_float() {
        let start_secs: u64 = 2_000_000_000;
        let end_secs: u64 = 2_000_604_800;
        let snap = parse_grpc_web_response(&encode_credits_frame(
            None,
            Some(start_secs),
            Some(end_secs),
            &[],
        ))
        .unwrap();
        assert_eq!(snap.used_percent, 0.0);
        assert_eq!(
            snap.resets_at,
            Some(Utc.timestamp_opt(end_secs as i64, 0).single().unwrap())
        );
        assert_eq!(snap.window_minutes, Some(WEEKLY_MINUTES));
        assert_eq!(snap.prepaid_balance_cents, None);
    }

    #[test]
    fn parses_percent_float_when_present() {
        let snap =
            parse_grpc_web_response(&encode_credits_frame(Some(42.5), None, None, &[])).unwrap();
        assert!((snap.used_percent - 42.5).abs() < 0.01);
        assert_eq!(snap.prepaid_balance_cents, None);
    }

    #[test]
    fn omitted_credit_usage_percent_is_zero() {
        let snap =
            parse_grpc_web_response(&encode_credits_frame(None, None, Some(2_000_604_800), &[]))
                .unwrap();
        assert_eq!(snap.used_percent, 0.0);
        assert_eq!(
            snap.resets_at,
            Some(Utc.timestamp_opt(2_000_604_800, 0).single().unwrap())
        );
        assert_eq!(snap.window_minutes, Some(WEEKLY_MINUTES));
    }

    #[test]
    fn explicit_zero_percent_float_is_zero() {
        let snap = parse_grpc_web_response(&encode_credits_frame(
            Some(0.0),
            None,
            Some(2_000_604_800),
            &[],
        ))
        .unwrap();
        assert_eq!(snap.used_percent, 0.0);
    }

    #[test]
    fn explicit_hundred_percent_float_is_full() {
        let snap = parse_grpc_web_response(&encode_credits_frame(
            Some(100.0),
            None,
            Some(2_000_604_800),
            &[],
        ))
        .unwrap();
        assert!((snap.used_percent - 100.0).abs() < 0.01);
    }

    #[test]
    fn reset_comes_from_period_end_not_period_start() {
        let start_secs: u64 = 2_000_000_000;
        let end_secs: u64 = 2_000_604_800;
        let snap = parse_grpc_web_response(&encode_credits_frame(
            Some(12.0),
            Some(start_secs),
            Some(end_secs),
            &[],
        ))
        .unwrap();
        assert_eq!(
            snap.resets_at,
            Some(Utc.timestamp_opt(end_secs as i64, 0).single().unwrap())
        );
        assert_ne!(
            snap.resets_at,
            Some(Utc.timestamp_opt(start_secs as i64, 0).single().unwrap())
        );
        assert_eq!(snap.window_minutes, Some(WEEKLY_MINUTES));
    }

    #[test]
    fn period_start_alone_is_not_the_reset_time() {
        let start_secs: u64 = 2_000_000_000;
        let snap = parse_grpc_web_response(&encode_credits_frame(
            Some(5.0),
            Some(start_secs),
            None,
            &[],
        ))
        .unwrap();
        assert_eq!(snap.resets_at, None);
        assert_eq!(snap.window_minutes, None);
    }

    #[test]
    fn extra_later_timestamp_in_other_field_is_not_reset() {
        let start_secs: u64 = 2_000_000_000;
        let end_secs: u64 = 2_000_604_800;
        let decoy_secs: u64 = 2_099_000_000;
        let snap = parse_grpc_web_response(&encode_credits_frame(
            Some(8.0),
            Some(start_secs),
            Some(end_secs),
            &[ProtoExtra::Timestamp {
                field: 9,
                seconds: decoy_secs,
            }],
        ))
        .unwrap();
        assert_eq!(
            snap.resets_at,
            Some(Utc.timestamp_opt(end_secs as i64, 0).single().unwrap())
        );
        assert_ne!(
            snap.resets_at,
            Some(Utc.timestamp_opt(decoy_secs as i64, 0).single().unwrap())
        );
    }

    #[test]
    fn prepaid_absent_is_none() {
        let snap = parse_grpc_web_response(&encode_credits_frame(
            Some(42.5),
            Some(2_000_000_000),
            Some(2_000_604_800),
            &[],
        ))
        .unwrap();
        assert_eq!(snap.prepaid_balance_cents, None);
    }

    #[test]
    fn undocumented_nested_money_is_not_treated_as_prepaid() {
        // Field 3 is not a published prepaid_balance number. A nested Money
        // there must not become extra credits (no invented field scan).
        let snap = parse_grpc_web_response(&encode_credits_frame(
            Some(10.0),
            None,
            Some(2_000_604_800),
            &[ProtoExtra::Money {
                field: 3,
                cents: 1_250,
            }],
        ))
        .unwrap();
        assert_eq!(snap.prepaid_balance_cents, None);
    }

    #[test]
    fn documented_money_val_decodes_cents() {
        assert_eq!(
            proto_money_cents(&encode_money(1_250)).unwrap(),
            Some(1_250)
        );
        assert_eq!(proto_money_cents(&encode_money(99)).unwrap(), Some(99));
    }

    #[test]
    fn documented_money_omitted_or_zero_val_is_none() {
        assert_eq!(proto_money_cents(&[]).unwrap(), None);
        assert_eq!(proto_money_cents(&encode_money(0)).unwrap(), None);
    }

    #[test]
    fn maps_active_supergrok_pro_to_heavy_label() {
        let json = serde_json::json!({
            "subscriptions": [
                {
                    "tier": "SUBSCRIPTION_TIER_GROK_PRO",
                    "status": "SUBSCRIPTION_STATUS_INACTIVE"
                },
                {
                    "tier": "SUBSCRIPTION_TIER_SUPER_GROK_PRO",
                    "status": "SUBSCRIPTION_STATUS_ACTIVE"
                },
                {
                    "tier": "SUBSCRIPTION_TIER_X_PREMIUM",
                    "status": "SUBSCRIPTION_STATUS_ACTIVE"
                }
            ]
        });
        assert_eq!(
            plan_name_from_subscriptions(&json).as_deref(),
            Some("SuperGrok Heavy")
        );
    }

    #[test]
    fn metadata_labels_weekly_pool_as_weekly() {
        let provider = GrokProvider::new();
        assert_eq!(provider.metadata().session_label, "Weekly");
        assert_eq!(provider.metadata().weekly_label, "Weekly");
    }

    #[test]
    fn billing_snapshot_marks_primary_as_weekly_window() {
        let result = result_from_billing(
            GrokBillingSnapshot {
                used_percent: 12.0,
                resets_at: Some(Utc::now() + chrono::Duration::days(3)),
                window_minutes: Some(WEEKLY_MINUTES),
                prepaid_balance_cents: None,
            },
            "oidc",
            None,
            None,
            Some("SuperGrok".into()),
        );
        assert_eq!(result.usage.primary.window_minutes, Some(WEEKLY_MINUTES));
        assert!(result.usage.secondary.is_none());
        assert!(result.usage.reset_credits_available.is_none());
        assert!((result.usage.primary.used_percent - 12.0).abs() < f64::EPSILON);
    }

    #[test]
    fn billing_snapshot_surfaces_prepaid_as_secondary_meter() {
        let result = result_from_billing(
            GrokBillingSnapshot {
                used_percent: 40.0,
                resets_at: None,
                window_minutes: Some(WEEKLY_MINUTES),
                prepaid_balance_cents: Some(1_250),
            },
            "oidc",
            None,
            None,
            None,
        );
        let secondary = result.usage.secondary.expect("prepaid secondary meter");
        assert!((secondary.used_percent - 0.0).abs() < f64::EPSILON);
        assert_eq!(
            secondary.reset_description.as_deref(),
            Some("$12.50 extra credits")
        );
    }

    #[test]
    fn remaining_resets_empty_payload_is_a_known_zero() {
        assert_eq!(parse_remaining_resets(&grpc_frame(&[])).unwrap(), 0);
        let ok_empty = [grpc_frame(&[]), grpc_trailer(0)].concat();
        assert_eq!(parse_remaining_resets(&ok_empty).unwrap(), 0);
    }

    #[test]
    fn remaining_resets_rpc_error_trailer_is_not_a_known_zero() {
        // grpc-web errors are HTTP 200 plus a trailer status. Empty data
        // frames would look like a successful zero if the trailer is ignored.
        assert!(parse_remaining_resets(&grpc_trailer(12)).is_err());
        assert!(parse_remaining_resets(&grpc_trailer(13)).is_err());
        assert!(matches!(
            parse_remaining_resets(&grpc_trailer(16)),
            Err(ProviderError::AuthRequired)
        ));
    }

    #[test]
    fn remaining_resets_unframed_or_unmatched_bodies_are_not_zero() {
        assert!(parse_remaining_resets(&[]).is_err());
        assert!(parse_remaining_resets(b"<html>not grpc</html>").is_err());
        let mut unmatched = Vec::new();
        write_key(&mut unmatched, 1, 0);
        write_varint(&mut unmatched, 5);
        assert!(parse_remaining_resets(&grpc_frame(&unmatched)).is_err());
    }

    #[test]
    fn remaining_resets_truncated_varint_is_not_zero() {
        let mut truncated = Vec::new();
        write_key(&mut truncated, RESET_TOKEN_FIELD, 2);
        write_varint(&mut truncated, 20);
        truncated.extend_from_slice(&[1, 2, 3]);
        assert!(parse_remaining_resets(&grpc_frame(&truncated)).is_err());
        assert!(parse_remaining_resets(&grpc_frame(&[0x80])).is_err());
    }

    #[test]
    fn remaining_resets_counts_unexpired_tokens_only() {
        let future = Utc::now() + chrono::Duration::days(30);
        let past = Utc::now() - chrono::Duration::days(1);
        let payload = encode_remaining_resets(&[
            ("restok_live", future),
            ("", future),
            ("restok_expired", past),
            ("restok_other", future),
        ]);
        assert_eq!(parse_remaining_resets(&payload).unwrap(), 2);
    }

    #[test]
    fn remaining_resets_decodes_live_shaped_token() {
        // Field numbers 10/20/30 match consumer_ui.proto as shipped by grok.com.
        let end = Utc::now() + chrono::Duration::days(30);
        let payload = encode_remaining_resets(&[("restok_example", end)]);
        assert_eq!(parse_remaining_resets(&payload).unwrap(), 1);
    }

    enum ProtoExtra {
        Timestamp { field: u64, seconds: u64 },
        Money { field: u64, cents: u64 },
    }

    fn encode_credits_frame(
        percent: Option<f32>,
        period_start: Option<u64>,
        period_end: Option<u64>,
        extras: &[ProtoExtra],
    ) -> Vec<u8> {
        let mut config = Vec::new();
        if let Some(percent) = percent {
            write_key(&mut config, CREDIT_USAGE_PERCENT_FIELD, 5);
            config.extend_from_slice(&percent.to_le_bytes());
        }
        if let Some(seconds) = period_start {
            write_len_field(&mut config, PERIOD_START_FIELD, &encode_timestamp(seconds));
        }
        if let Some(seconds) = period_end {
            write_len_field(&mut config, PERIOD_END_FIELD, &encode_timestamp(seconds));
        }
        for extra in extras {
            match extra {
                ProtoExtra::Timestamp { field, seconds } => {
                    write_len_field(&mut config, *field, &encode_timestamp(*seconds));
                }
                ProtoExtra::Money { field, cents } => {
                    write_len_field(&mut config, *field, &encode_money(*cents));
                }
            }
        }
        let mut payload = Vec::new();
        write_len_field(&mut payload, BILLING_CONFIG_FIELD, &config);
        grpc_frame(&payload)
    }

    fn encode_timestamp(seconds: u64) -> Vec<u8> {
        let mut ts = Vec::new();
        write_key(&mut ts, TIMESTAMP_SECONDS_FIELD, 0);
        write_varint(&mut ts, seconds);
        ts
    }

    fn encode_money(cents: u64) -> Vec<u8> {
        let mut money = Vec::new();
        write_key(&mut money, MONEY_VAL_FIELD, 0);
        write_varint(&mut money, cents);
        money
    }

    fn write_len_field(buf: &mut Vec<u8>, field: u64, payload: &[u8]) {
        write_key(buf, field, 2);
        write_varint(buf, payload.len() as u64);
        buf.extend_from_slice(payload);
    }

    fn encode_remaining_resets(tokens: &[(&str, DateTime<Utc>)]) -> Vec<u8> {
        let mut payload = Vec::new();
        for (id, end) in tokens {
            let mut token = Vec::new();
            write_key(&mut token, RESET_TOKEN_ID_FIELD, 2);
            write_varint(&mut token, id.len() as u64);
            token.extend_from_slice(id.as_bytes());
            let mut ts = Vec::new();
            write_key(&mut ts, 1, 0);
            write_varint(&mut ts, end.timestamp() as u64);
            write_key(&mut token, RESET_TOKEN_END_FIELD, 2);
            write_varint(&mut token, ts.len() as u64);
            token.extend_from_slice(&ts);
            write_key(&mut payload, RESET_TOKEN_FIELD, 2);
            write_varint(&mut payload, token.len() as u64);
            payload.extend_from_slice(&token);
        }
        grpc_frame(&payload)
    }

    fn grpc_frame(payload: &[u8]) -> Vec<u8> {
        grpc_web_frame(0, payload)
    }

    fn grpc_trailer(status: u16) -> Vec<u8> {
        grpc_web_frame(0x80, format!("grpc-status: {status}\r\n").as_bytes())
    }

    fn grpc_web_frame(flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![flags];
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    fn write_key(buf: &mut Vec<u8>, field: u64, wire: u64) {
        write_varint(buf, (field << 3) | wire);
    }

    fn write_varint(buf: &mut Vec<u8>, mut value: u64) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            buf.push(byte);
            if value == 0 {
                break;
            }
        }
    }
}
