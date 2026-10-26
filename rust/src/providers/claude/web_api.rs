//! Claude Web API fetcher - uses browser cookies to fetch usage from claude.ai

use chrono::{DateTime, Utc};
use reqwest::{Client, header};
use serde::Deserialize;
use std::path::PathBuf;

use super::UtilizationScale;
use super::usage_api::{ClaudeExtraUsage, ClaudeUsageResponse, ClaudeUsageWindow};
use crate::browser::cookies::get_cookie_header_from_browser;
use crate::browser::detection::{BrowserProfile, BrowserType, DetectedBrowser};
use crate::core::{PromoSignal, ProviderError, ProviderFetchResult, RateWindow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeDesktopSessionStatus {
    Ready,
    Locked,
    SignedOut,
    Unavailable,
}

/// Locate Claude Desktop's Chromium profile without reading any credential
/// values. Windows Store builds keep Electron data under the package's
/// redirected Roaming directory; older standalone builds use `%APPDATA%`.
/// Linux Electron builds keep `Network/Cookies` and `Local State` under
/// `~/.config/Claude`.
fn claude_desktop_data_dirs() -> Vec<PathBuf> {
    let mut candidates = std::env::var_os("CLAUDE_DESKTOP_DATA_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect::<Vec<_>>();
    for candidate in
        claude_desktop_data_dirs_from(dirs::data_local_dir(), dirs::data_dir(), dirs::config_dir())
    {
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    candidates
}

fn claude_desktop_data_dirs_from(
    data_local_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    config_dir: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(local) = data_local_dir {
        candidates.push(
            local
                .join("Packages")
                .join("Claude_pzs8sxrjxfjjc")
                .join("LocalCache")
                .join("Roaming")
                .join("Claude"),
        );
    }
    if let Some(data) = data_dir {
        candidates.push(data.join("Claude"));
    }
    if let Some(config) = config_dir {
        candidates.push(config.join("Claude"));
    }
    candidates.dedup();
    candidates
}

fn claude_desktop_session() -> Result<String, ClaudeDesktopSessionStatus> {
    let mut found_profile = false;
    for data_dir in claude_desktop_data_dirs() {
        if !data_dir.join("Network").join("Cookies").is_file()
            || !data_dir.join("Local State").is_file()
        {
            continue;
        }
        found_profile = true;

        let desktop = DetectedBrowser {
            browser_type: BrowserType::Chromium,
            user_data_dir: data_dir.clone(),
            profiles: vec![BrowserProfile {
                name: "Claude Desktop".to_string(),
                path: data_dir,
                is_default: true,
            }],
        };

        match get_cookie_header_from_browser("claude.ai", &desktop) {
            Ok(header) if cookie_value(&header, "sessionKey").is_some() => {
                tracing::debug!("Using Claude Desktop session for usage fetch");
                return Ok(header);
            }
            Ok(_) => tracing::debug!("Claude Desktop is present but has no active session"),
            Err(error) => {
                tracing::debug!("Claude Desktop session unavailable: {error}");
                let message = error.to_string().to_ascii_lowercase();
                if message.contains("os error 32")
                    || message.contains("being used by another process")
                    || message.contains("sharing violation")
                {
                    return Err(ClaudeDesktopSessionStatus::Locked);
                }
            }
        }
    }
    Err(if found_profile {
        ClaudeDesktopSessionStatus::SignedOut
    } else {
        ClaudeDesktopSessionStatus::Unavailable
    })
}

pub fn claude_desktop_session_status() -> ClaudeDesktopSessionStatus {
    match claude_desktop_session() {
        Ok(_) => ClaudeDesktopSessionStatus::Ready,
        Err(status) => status,
    }
}

fn claude_desktop_cookie_header() -> Option<String> {
    claude_desktop_session().ok()
}

/// Read the response body as text, then deserialize as JSON. On failure, include
/// non-sensitive shape metadata so auth redirects, error envelopes, and schema
/// changes are distinguishable without exposing account data in UI/log output.
async fn parse_json_with_body<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    label: &str,
) -> Result<T, ProviderError> {
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response
        .text()
        .await
        .map_err(|e| ProviderError::Parse(format!("Failed to read {label} response body: {e}")))?;

    serde_json::from_str::<T>(&body).map_err(|e| {
        ProviderError::Parse(format!(
            "Failed to parse {label}: {e} ({})",
            describe_json_body_shape(&body, content_type.as_deref())
        ))
    })
}

fn describe_json_body_shape(body: &str, content_type: Option<&str>) -> String {
    let content_type = content_type.unwrap_or("unknown");
    let body_len = body.len();

    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(serde_json::Value::Object(map)) => {
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort_unstable();
            let suffix = if keys.len() > 12 { ", ..." } else { "" };
            let keys = keys.into_iter().take(12).collect::<Vec<_>>().join(", ");
            format!("content_type={content_type}, body_len={body_len}, json_keys=[{keys}{suffix}]")
        }
        Ok(value) => format!(
            "content_type={content_type}, body_len={body_len}, json_type={}",
            json_value_kind(&value)
        ),
        Err(_) => format!("content_type={content_type}, body_len={body_len}, body_kind=non-json"),
    }
}

fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Claude Web API fetcher
pub struct ClaudeWebApiFetcher {
    client: Result<Client, String>,
}

/// Organization info from Claude API
#[derive(Debug, Deserialize)]
struct Organization {
    uuid: String,
    #[allow(dead_code)]
    name: Option<String>,
}

/// Account info response
#[derive(Debug, Deserialize)]
struct AccountResponse {
    email_address: Option<String>,

    #[serde(rename = "rate_limit_tier")]
    rate_limit_tier: Option<String>,

    #[serde(default)]
    memberships: Vec<AccountMembership>,
}

#[derive(Debug, Deserialize)]
struct AccountMembership {
    uuid: Option<String>,
    organization: Option<AccountOrganization>,
}

#[derive(Debug, Deserialize)]
struct AccountOrganization {
    uuid: Option<String>,
}

impl AccountResponse {
    fn first_membership_org_id(&self) -> Option<String> {
        self.memberships.iter().find_map(|membership| {
            membership
                .organization
                .as_ref()
                .and_then(|organization| organization.uuid.as_deref())
                .or(membership.uuid.as_deref())
                .map(str::trim)
                .filter(|uuid| !uuid.is_empty())
                .map(ToString::to_string)
        })
    }
}

impl ClaudeWebApiFetcher {
    const BASE_URL: &'static str = "https://claude.ai/api";

    /// Create a new fetcher
    pub fn new() -> Self {
        Self {
            client: crate::core::credentialed_http_client_builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|error| format!("Failed to create HTTP client: {error}")),
        }
    }

    fn client(&self) -> Result<&Client, ProviderError> {
        self.client
            .as_ref()
            .map_err(|error| ProviderError::Other(error.clone()))
    }

    /// Fetch usage using browser cookies or env-var session key
    pub async fn fetch_with_cookies(&self) -> Result<ProviderFetchResult, ProviderError> {
        if let Some(session_key) = Self::resolve_session_key_from_env() {
            tracing::debug!("Using Claude session key from environment variable");
            let cookie_header = format!("sessionKey={session_key}");
            return self.fetch_with_cookie_header(&cookie_header).await;
        }

        // Reuse the signed-in Claude Desktop session before probing browsers.
        // This keeps Automatic genuinely zero-setup for desktop users and does
        // not persist or log the extracted cookie value in Ceiling.
        if let Some(cookie_header) = claude_desktop_cookie_header() {
            let mut result = self.fetch_with_cookie_header(&cookie_header).await?;
            result.source_label = "desktop".to_string();
            return Ok(result);
        }

        Err(ProviderError::NoCookies)
    }

    /// Fetch usage with a provided cookie header
    pub async fn fetch_with_cookie_header(
        &self,
        cookie_header: &str,
    ) -> Result<ProviderFetchResult, ProviderError> {
        tracing::debug!("Fetching Claude usage via web API");

        let headers = Self::build_headers(cookie_header);

        // Step 1: Get organization ID
        let org_id = self.get_organization_id(cookie_header, &headers).await?;
        tracing::debug!("Got organization ID: {}", org_id);

        // Step 2: Fetch usage data
        let usage = self.get_usage(&org_id, &headers).await?;

        // Step 3: Fetch extra usage (credits) - optional
        let extra_usage = self
            .get_extra_usage(&org_id, &headers)
            .await
            .ok()
            .or_else(|| usage.extra_usage.clone());

        // Step 4: Fetch account info - optional
        let account = self.get_account_info(&headers).await.ok();

        // Build every common Claude usage lane through one normalized path so
        // OAuth and web cannot silently drop fields the other source renders.
        let mut snapshot = super::usage_api::with_spend_limit_lane(
            usage.build_snapshot(|window, minutes, scale| {
                self.to_rate_window(window, minutes, scale)
            }),
            extra_usage.as_ref(),
        );

        if let Some(promo) = usage.seven_day_promotional.as_ref() {
            let ends_at = promo
                .resets_at
                .as_deref()
                .and_then(ClaudeWebApiFetcher::parse_iso8601);
            snapshot = snapshot.with_promo_signal(PromoSignal::boost(
                "claude-weekly-promo",
                "Weekly promo",
                "Temporary promotional weekly capacity reported by Claude",
                Some("claude-weekly-promo".to_string()),
                ends_at,
            ));
        }

        if let Some(ref acc) = account {
            if let Some(ref email) = acc.email_address {
                snapshot = snapshot.with_email(email.clone());
            }
            if let Some(ref tier) = acc.rate_limit_tier {
                snapshot = snapshot.with_login_method(super::claude_plan_label(tier));
            }
        }

        let mut result = ProviderFetchResult::new(snapshot, "web");

        // `extra_usage` already fell back to the embedded payload when the
        // dedicated endpoint failed, so a `None` here means the endpoint was
        // read and reported overage as disabled. Do not claim a cost for it.
        if let Some(cost) = extra_usage
            .as_ref()
            .and_then(ClaudeExtraUsage::cost_snapshot)
        {
            result = result.with_cost(cost);
        }

        Ok(result)
    }

    fn build_headers(cookie_header: &str) -> reqwest::header::HeaderMap {
        use reqwest::header::HeaderValue;

        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(cookie) = HeaderValue::from_str(cookie_header) {
            headers.insert(header::COOKIE, cookie);
        }
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("https://claude.ai"),
        );
        headers.insert(
            header::REFERER,
            HeaderValue::from_static("https://claude.ai/settings/usage"),
        );
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36",
            ),
        );
        headers.insert(
            reqwest::header::HeaderName::from_static("anthropic-client-platform"),
            HeaderValue::from_static("web_claude_ai"),
        );

        headers
    }

    fn resolve_session_key_from_env() -> Option<String> {
        for env_name in ["CLAUDE_AI_SESSION_KEY", "CLAUDE_WEB_SESSION_KEY"] {
            let Ok(value) = std::env::var(env_name) else {
                continue;
            };

            let trimmed = value.trim();
            if trimmed.is_empty() {
                continue;
            }

            let normalized = trimmed
                .strip_prefix("sessionKey=")
                .unwrap_or(trimmed)
                .trim();

            if !normalized.is_empty() {
                return Some(normalized.to_string());
            }
        }

        None
    }

    /// Get the organization ID
    async fn get_organization_id(
        &self,
        cookie_header: &str,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<String, ProviderError> {
        if let Some(org_id) = cookie_value(cookie_header, "lastActiveOrg") {
            return Ok(org_id);
        }

        if let Ok(account) = self.get_account_info(headers).await
            && let Some(org_id) = account.first_membership_org_id()
        {
            return Ok(org_id);
        }

        let url = format!("{}/organizations", Self::BASE_URL);

        let response = self
            .client()?
            .get(&url)
            .headers(headers.clone())
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Failed to get organizations: {}",
                response.status()
            )));
        }

        let orgs: Vec<Organization> = parse_json_with_body(response, "organizations").await?;

        orgs.into_iter()
            .next()
            .map(|o| o.uuid)
            .ok_or_else(|| ProviderError::Parse("No organizations found".to_string()))
    }

    /// Get usage data
    async fn get_usage(
        &self,
        org_id: &str,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<ClaudeUsageResponse, ProviderError> {
        let url = format!("{}/organizations/{}/usage", Self::BASE_URL, org_id);

        let response = self
            .client()?
            .get(&url)
            .headers(headers.clone())
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Failed to get usage: {}",
                response.status()
            )));
        }

        parse_json_with_body(response, "usage").await
    }

    /// Get extra usage (credits)
    async fn get_extra_usage(
        &self,
        org_id: &str,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<ClaudeExtraUsage, ProviderError> {
        let url = format!(
            "{}/organizations/{}/overage_spend_limit",
            Self::BASE_URL,
            org_id
        );

        let response = self
            .client()?
            .get(&url)
            .headers(headers.clone())
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Failed to get extra usage: {}",
                response.status()
            )));
        }

        parse_json_with_body(response, "extra usage").await
    }

    /// Get account info
    async fn get_account_info(
        &self,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<AccountResponse, ProviderError> {
        let url = format!("{}/account", Self::BASE_URL);

        let response = self
            .client()?
            .get(&url)
            .headers(headers.clone())
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Failed to get account: {}",
                response.status()
            )));
        }

        parse_json_with_body(response, "account").await
    }

    /// Convert a usage window to a RateWindow.
    ///
    /// Missing utilization is unknown, not 0% (SBS-1040).
    fn to_rate_window(
        &self,
        window: &ClaudeUsageWindow,
        window_minutes: Option<u32>,
        scale: UtilizationScale,
    ) -> Option<RateWindow> {
        let used_percent = scale.to_percent(window.utilization?);

        let resets_at = window
            .resets_at
            .as_ref()
            .and_then(|s| Self::parse_iso8601(s));

        let reset_description = resets_at.map(Self::format_reset_time);

        Some(RateWindow::with_details(
            used_percent,
            window_minutes,
            resets_at,
            reset_description,
        ))
    }

    /// Parse ISO8601 date string
    fn parse_iso8601(s: &str) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
            .or_else(|| {
                chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                    .ok()
                    .map(|ndt| ndt.and_utc())
            })
    }

    /// Format reset time for display
    fn format_reset_time(dt: DateTime<Utc>) -> String {
        dt.format("%b %-d at %-I:%M%p").to_string()
    }

    /// Convert rate limit tier to plan name
    fn tier_to_plan_name(tier: &str) -> String {
        super::claude_plan_label(tier)
    }
}

impl Default for ClaudeWebApiFetcher {
    fn default() -> Self {
        Self::new()
    }
}

fn cookie_value(cookie_header: &str, name: &str) -> Option<String> {
    cookie_header.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        if key.trim() != name {
            return None;
        }
        let value = value.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        AccountResponse, ClaudeUsageResponse, ClaudeUsageWindow, ClaudeWebApiFetcher,
        UtilizationScale, claude_desktop_data_dirs_from, cookie_value,
    };
    use reqwest::header;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn converts_fractional_utilization_to_percent() {
        let window = ClaudeUsageWindow {
            utilization: Some(0.23),
            resets_at: None,
        };

        let rate = ClaudeWebApiFetcher::new()
            .to_rate_window(&window, Some(300), UtilizationScale::Fraction)
            .expect("rate window");

        assert!((rate.used_percent - 23.0).abs() < f64::EPSILON);
    }

    #[test]
    fn preserves_existing_percentage_utilization() {
        let window = ClaudeUsageWindow {
            utilization: Some(23.0),
            resets_at: None,
        };

        let rate = ClaudeWebApiFetcher::new()
            .to_rate_window(&window, Some(300), UtilizationScale::Percent)
            .expect("rate window");

        assert!((rate.used_percent - 23.0).abs() < f64::EPSILON);
    }

    #[test]
    fn labels_max_5x_and_20x_plans() {
        assert_eq!(
            ClaudeWebApiFetcher::tier_to_plan_name("default_claude_max_5x"),
            "Claude Max 5x"
        );
        assert_eq!(
            ClaudeWebApiFetcher::tier_to_plan_name("v2_default_claude_max_20x"),
            "Claude Max 20x"
        );
    }

    #[test]
    fn discovers_packaged_and_legacy_claude_desktop_profiles() {
        let local = PathBuf::from(r"C:\Users\person\AppData\Local");
        let roaming = PathBuf::from(r"C:\Users\person\AppData\Roaming");
        let paths = claude_desktop_data_dirs_from(
            Some(local.clone()),
            Some(roaming.clone()),
            Some(roaming.clone()),
        );

        assert_eq!(
            paths,
            vec![
                local
                    .join("Packages")
                    .join("Claude_pzs8sxrjxfjjc")
                    .join("LocalCache")
                    .join("Roaming")
                    .join("Claude"),
                roaming.join("Claude"),
            ]
        );
    }

    #[test]
    fn discovers_linux_config_dir_claude_desktop_profile() {
        let share = PathBuf::from("/home/person/.local/share");
        let config = PathBuf::from("/home/person/.config");
        let paths = claude_desktop_data_dirs_from(
            Some(share.clone()),
            Some(share.clone()),
            Some(config.clone()),
        );

        assert_eq!(
            paths,
            vec![
                share
                    .join("Packages")
                    .join("Claude_pzs8sxrjxfjjc")
                    .join("LocalCache")
                    .join("Roaming")
                    .join("Claude"),
                share.join("Claude"),
                config.join("Claude"),
            ]
        );
    }

    #[test]
    fn dedups_identical_local_data_and_config_claude_desktop_profiles() {
        let same = PathBuf::from(r"C:\Users\person\AppData\Roaming");
        let paths = claude_desktop_data_dirs_from(
            Some(same.clone()),
            Some(same.clone()),
            Some(same.clone()),
        );

        assert_eq!(
            paths,
            vec![
                same.join("Packages")
                    .join("Claude_pzs8sxrjxfjjc")
                    .join("LocalCache")
                    .join("Roaming")
                    .join("Claude"),
                same.join("Claude"),
            ]
        );
    }

    #[test]
    fn resolves_raw_session_key_from_primary_env_var() {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::remove_var("CLAUDE_AI_SESSION_KEY");
            std::env::remove_var("CLAUDE_WEB_SESSION_KEY");
            std::env::set_var("CLAUDE_AI_SESSION_KEY", "sk-ant-primary");
            std::env::set_var("CLAUDE_WEB_SESSION_KEY", "sk-ant-secondary");
        }

        let session_key = ClaudeWebApiFetcher::resolve_session_key_from_env();

        assert_eq!(session_key.as_deref(), Some("sk-ant-primary"));

        unsafe {
            std::env::remove_var("CLAUDE_AI_SESSION_KEY");
            std::env::remove_var("CLAUDE_WEB_SESSION_KEY");
        }
    }

    #[test]
    fn resolves_session_key_assignment_from_env_var() {
        let _guard = env_lock().lock().expect("env lock");
        unsafe {
            std::env::remove_var("CLAUDE_AI_SESSION_KEY");
            std::env::remove_var("CLAUDE_WEB_SESSION_KEY");
            std::env::set_var("CLAUDE_WEB_SESSION_KEY", "sessionKey=sk-ant-cookie-format");
        }

        let session_key = ClaudeWebApiFetcher::resolve_session_key_from_env();

        assert_eq!(session_key.as_deref(), Some("sk-ant-cookie-format"));

        unsafe {
            std::env::remove_var("CLAUDE_AI_SESSION_KEY");
            std::env::remove_var("CLAUDE_WEB_SESSION_KEY");
        }
    }

    #[test]
    fn build_headers_include_required_browser_context() {
        let headers = ClaudeWebApiFetcher::build_headers("sessionKey=sk-ant-cookie-format");

        assert_eq!(
            headers
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok()),
            Some("sessionKey=sk-ant-cookie-format")
        );
        assert_eq!(
            headers
                .get(header::ACCEPT)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            headers
                .get(header::ORIGIN)
                .and_then(|value| value.to_str().ok()),
            Some("https://claude.ai")
        );
        assert_eq!(
            headers
                .get(header::REFERER)
                .and_then(|value| value.to_str().ok()),
            Some("https://claude.ai/settings/usage")
        );
        assert_eq!(
            headers
                .get("anthropic-client-platform")
                .and_then(|value| value.to_str().ok()),
            Some("web_claude_ai")
        );
        assert!(headers.contains_key(header::USER_AGENT));
    }

    #[test]
    fn extracts_last_active_org_from_cookie_header() {
        let org = cookie_value(
            "foo=bar; sessionKey=sk-ant-session; lastActiveOrg=org-123; other=value",
            "lastActiveOrg",
        );

        assert_eq!(org.as_deref(), Some("org-123"));
    }

    #[test]
    fn account_membership_prefers_nested_organization_uuid() {
        let account: AccountResponse = serde_json::from_str(
            r#"{
                "email_address": "user@example.com",
                "memberships": [
                    {
                        "uuid": "membership-id",
                        "organization": { "uuid": "org-id" }
                    }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(account.first_membership_org_id().as_deref(), Some("org-id"));
    }

    #[test]
    fn parses_extra_design_and_routines_aliases() {
        let usage: ClaudeUsageResponse = serde_json::from_str(
            r#"{
                "five_hour": { "utilization": 0.1 },
                "seven_day_design": { "utilization": 31 },
                "seven_day_omelette": { "utilization": 26 },
                "seven_day_cowork": { "utilization": 11 }
            }"#,
        )
        .unwrap();

        let fetcher = ClaudeWebApiFetcher::new();
        let design = usage
            .seven_day_design
            .as_ref()
            .and_then(|w| fetcher.to_rate_window(w, Some(10080), usage.utilization_scale()))
            .expect("design window");
        let promo = usage
            .seven_day_promotional
            .as_ref()
            .and_then(|w| fetcher.to_rate_window(w, Some(10080), usage.utilization_scale()))
            .expect("promotional omelette window");
        let routines = usage
            .seven_day_routines
            .as_ref()
            .and_then(|w| fetcher.to_rate_window(w, Some(10080), usage.utilization_scale()))
            .expect("routines window");

        assert!((design.used_percent - 31.0).abs() < f64::EPSILON);
        assert!((promo.used_percent - 26.0).abs() < f64::EPSILON);
        assert!((routines.used_percent - 11.0).abs() < f64::EPSILON);
    }

    #[test]
    fn maps_scoped_weekly_limits_even_when_inactive() {
        let usage: ClaudeUsageResponse = serde_json::from_str(
            r#"{
                "limits": [{
                    "kind": "weekly_scoped",
                    "group": "weekly",
                    "percent": 7,
                    "resets_at": "2026-07-16T10:00:00Z",
                    "scope": {"model": {"id": null, "display_name": "Fable"}},
                    "is_active": false
                }]
            }"#,
        )
        .unwrap();

        let windows = super::super::scoped_weekly::scoped_weekly_windows(&usage.limits);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].id, "claude-weekly-scoped-fable");
        assert_eq!(windows[0].title, "Fable only");
    }

    #[test]
    fn parses_duplicate_design_and_routines_aliases_with_preferred_key() {
        let usage: ClaudeUsageResponse = serde_json::from_str(
            r#"{
                "seven_day_design": { "utilization": 31 },
                "seven_day_omelette": { "utilization": 26 },
                "seven_day_routines": { "utilization": 19 },
                "seven_day_cowork": { "utilization": 11 }
            }"#,
        )
        .unwrap();

        let fetcher = ClaudeWebApiFetcher::new();
        let design = usage
            .seven_day_design
            .as_ref()
            .and_then(|w| fetcher.to_rate_window(w, Some(10080), usage.utilization_scale()))
            .expect("design window");
        let routines = usage
            .seven_day_routines
            .as_ref()
            .and_then(|w| fetcher.to_rate_window(w, Some(10080), usage.utilization_scale()))
            .expect("routines window");

        assert!((design.used_percent - 31.0).abs() < f64::EPSILON);
        assert!((routines.used_percent - 19.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parses_oauth_apps_window_and_embedded_extra_usage() {
        let usage: ClaudeUsageResponse = serde_json::from_str(
            r#"{
                "five_hour": { "utilization": 0.1 },
                "seven_day_oauth_apps": { "utilization": 42 },
                "extra_usage": {
                    "is_enabled": true,
                    "monthly_credit_limit": 2000,
                    "used_credits": 550,
                    "currency": "USD"
                }
            }"#,
        )
        .unwrap();

        let fetcher = ClaudeWebApiFetcher::new();
        let oauth_apps = usage
            .seven_day_oauth_apps
            .as_ref()
            .and_then(|w| fetcher.to_rate_window(w, Some(10080), usage.utilization_scale()))
            .expect("oauth apps window");
        let extra = usage.extra_usage.expect("extra usage");

        assert!((oauth_apps.used_percent - 42.0).abs() < f64::EPSILON);
        assert_eq!(extra.is_enabled, Some(true));
        assert_eq!(extra.monthly_limit, Some(2000.0));
        assert_eq!(extra.used_credits, Some(550.0));
    }

    fn web_snapshot(usage: &ClaudeUsageResponse) -> crate::core::UsageSnapshot {
        let fetcher = ClaudeWebApiFetcher::new();
        usage
            .build_snapshot(|window, minutes, scale| fetcher.to_rate_window(window, minutes, scale))
    }

    /// SBS-1040: web used `unwrap_or(0.0)`, so a null utilization became
    /// Session (5h) 0%. Absent utilization is unknown, not empty.
    #[test]
    fn absent_utilization_does_not_become_a_zero_window() {
        let window = ClaudeUsageWindow {
            utilization: None,
            resets_at: Some("2026-08-23T12:00:00Z".to_string()),
        };

        assert!(
            ClaudeWebApiFetcher::new()
                .to_rate_window(&window, Some(300), UtilizationScale::Percent)
                .is_none(),
            "null utilization must not fabricate 0%"
        );
    }

    #[test]
    fn omitted_five_hour_stays_unknown_on_the_web_snapshot() {
        let usage: ClaudeUsageResponse = serde_json::from_str(
            r#"{
                "seven_day": { "utilization": 31 }
            }"#,
        )
        .unwrap();

        let snapshot = web_snapshot(&usage);
        assert!(
            snapshot
                .inactive_rate_windows
                .iter()
                .any(|window| window.id == "claude-session"
                    && window.state == crate::core::EnforcementState::Unavailable),
            "omitted five_hour must stay unknown, not Session 0%"
        );
        assert_eq!(snapshot.secondary.expect("weekly").used_percent, 31.0);
    }

    #[test]
    fn null_five_hour_utilization_stays_unknown_on_the_web_snapshot() {
        let usage: ClaudeUsageResponse = serde_json::from_str(
            r#"{
                "five_hour": { "utilization": null },
                "seven_day": { "utilization": 31 }
            }"#,
        )
        .unwrap();

        let snapshot = web_snapshot(&usage);
        assert!(
            snapshot
                .inactive_rate_windows
                .iter()
                .any(|window| window.id == "claude-session"
                    && window.state == crate::core::EnforcementState::Unavailable),
            "null five_hour utilization must stay unknown, not Session 0%"
        );
        assert_eq!(snapshot.secondary.expect("weekly").used_percent, 31.0);
    }
}
