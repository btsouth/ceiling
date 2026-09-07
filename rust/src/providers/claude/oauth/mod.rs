//! Claude OAuth implementation
//!
//! Loads OAuth credentials from Claude CLI and fetches usage from the API.

use chrono::{DateTime, Utc};
use reqwest::Client;
use reqwest::header::{HeaderValue, RETRY_AFTER};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::UtilizationScale;
use super::usage_api::{ClaudeUsageResponse, ClaudeUsageWindow};
use crate::core::{ProviderError, ProviderFetchResult, RateWindow, UsageSnapshot};

mod credentials_store;
mod refresh;

pub(super) fn credentials_file_available(config_dir: Option<&std::path::Path>) -> bool {
    credentials_store::credentials_file_available(config_dir)
}

/// OAuth credentials from Claude CLI
#[derive(Debug, Clone)]
pub struct ClaudeOAuthCredentials {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: Vec<String>,
    pub rate_limit_tier: Option<String>,
    /// Claude Code's `subscriptionType` ("pro" / "max" / "free"), which names
    /// the plan when `rate_limit_tier` does not. Same field `ClaudeIdentity`
    /// reads for account labels, kept here so the plan name is derived from the
    /// credentials the reading was actually fetched with.
    pub subscription_type: Option<String>,
}

impl ClaudeOAuthCredentials {
    /// Check if the token is expired
    pub fn is_expired(&self) -> bool {
        self.is_expired_at(Utc::now())
    }

    fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        if let Some(expires_at) = self.expires_at {
            // Consider expired if within 5 minutes of expiry
            expires_at <= now + chrono::Duration::minutes(5)
        } else {
            // No expiry info = don't assume expired, try it
            false
        }
    }

    /// Check if the credentials have a specific scope
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }
}

/// OAuth and web usage now share one normalized response shape.
pub type OAuthUsageResponse = ClaudeUsageResponse;
pub type UsageWindow = ClaudeUsageWindow;

/// Claude OAuth fetcher
pub struct ClaudeOAuthFetcher {
    client: Client,
    /// Explicit `CLAUDE_CONFIG_DIR` for a Ceiling-managed account. When `None`
    /// the fetcher follows whichever account the CLI is signed in as.
    config_dir: Option<PathBuf>,
}

static RATE_LIMIT_BACKOFF_UNTIL: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

impl ClaudeOAuthFetcher {
    const USAGE_URL: &'static str = "https://api.anthropic.com/api/oauth/usage";
    const DEFAULT_RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(5 * 60);

    pub fn new() -> Self {
        Self {
            client: Client::new(),
            config_dir: None,
        }
    }

    /// Build a fetcher pinned to a specific `CLAUDE_CONFIG_DIR`, for tracking one
    /// of several configured Claude accounts.
    pub fn with_config_dir(config_dir: PathBuf) -> Self {
        Self {
            config_dir: Some(config_dir),
            ..Self::new()
        }
    }

    /// The same fetcher aimed at a different account. Reuses the underlying
    /// connection pool rather than building a second one per fetch.
    pub fn scoped(&self, config_dir: PathBuf) -> Self {
        Self {
            client: self.client.clone(),
            config_dir: Some(config_dir),
        }
    }

    fn config_dir(&self) -> Option<&std::path::Path> {
        self.config_dir.as_deref()
    }

    /// Load credentials and fetch usage, transparently refreshing an expired
    /// OAuth token first (like the Claude CLI does) so the panel stays green
    /// without the user having to re-run `claude`.
    pub async fn fetch(&self) -> Result<ProviderFetchResult, ProviderError> {
        let (credentials, source) = credentials_store::load_credentials(self.config_dir())?;
        let credentials = self.ensure_fresh_credentials(credentials, source).await;
        self.fetch_with_credentials(credentials).await
    }

    /// Fetch usage with an explicit OAuth access token.
    pub async fn fetch_with_access_token(
        &self,
        access_token: &str,
    ) -> Result<ProviderFetchResult, ProviderError> {
        let access_token = access_token.trim();
        if access_token.is_empty() {
            return Err(ProviderError::OAuth(
                "Claude OAuth access token is empty.".to_string(),
            ));
        }

        let credentials = ClaudeOAuthCredentials {
            access_token: access_token.to_string(),
            refresh_token: None,
            expires_at: None,
            scopes: vec!["user:profile".to_string()],
            rate_limit_tier: None,
            subscription_type: None,
        };

        self.fetch_with_credentials(credentials).await
    }

    async fn fetch_with_credentials(
        &self,
        credentials: ClaudeOAuthCredentials,
    ) -> Result<ProviderFetchResult, ProviderError> {
        let usage_response = self.fetch_usage(&credentials).await?;
        let mut usage = self.build_usage_snapshot(&usage_response, &credentials);

        // Stamp which account this reading belongs to. Capacity baselines are
        // scoped by email and organization, so without them two accounts share
        // one baseline and a switch inherits the previous seat's history.
        if let Some(identity) = self.identity() {
            if let Some(email) = identity.email {
                usage = usage.with_email(email);
            }
            if let Some(organization) = identity.organization_name {
                usage = usage.with_organization(organization);
            }
        }

        let mut result = ProviderFetchResult::new(usage, "oauth");
        if let Some(cost) = usage_response.extra_usage_cost() {
            result = result.with_cost(cost);
        }
        Ok(result)
    }

    /// Identity of the account this fetcher reads, for labeling and scoping.
    fn identity(&self) -> Option<crate::core::ClaudeIdentity> {
        use crate::core::AccountIdentity;
        let dir = self
            .config_dir
            .clone()
            .or_else(crate::core::ambient_claude_config_dir)?;
        crate::core::ClaudeIdentity::read(&dir)
    }

    /// If the token is expired (or about to expire), refresh it using the
    /// refresh token and persist the new token back to `.credentials.json`.
    /// Best-effort: on any failure the original credentials are returned so the
    /// caller falls back to the existing "expired" handling.
    async fn ensure_fresh_credentials(
        &self,
        mut credentials: ClaudeOAuthCredentials,
        source: credentials_store::CredentialSource,
    ) -> ClaudeOAuthCredentials {
        // Prefer an in-memory refreshed token if it is fresher than what we just
        // read from disk (covers a prior persist that failed to write). Scoped
        // to this credential's own source so a refresh cached for one source
        // (e.g. the credentials file) never shadows another (e.g. an
        // environment-provided token).
        if let Some(cached) = credentials_store::cached_refreshed_if_fresher(&source, &credentials)
        {
            credentials = cached;
        }

        if !credentials.is_expired() {
            return credentials;
        }

        // The credentials file is shared with the Claude Code CLI, which also
        // refreshes it. Re-read right before hitting the network: if the CLI (or
        // a concurrent poll) already refreshed the on-disk token, adopt it rather
        // than rotating a second refresh token against the same account.
        if let Ok((disk, disk_source)) = credentials_store::load_credentials(self.config_dir()) {
            if !disk.is_expired() {
                credentials_store::store_refreshed(&disk_source, &disk);
                return disk;
            }
            credentials = disk;
        }

        let Some(refresh_token) = credentials.refresh_token.clone() else {
            // Environment-provided tokens have no refresh token; nothing to do.
            return credentials;
        };

        match refresh::refresh_access_token(&self.client, &refresh_token, &credentials).await {
            Ok(refreshed) => {
                // Persist before caching. If another process rotated the token
                // we exchanged, ours is already retired, and caching it would
                // authenticate this poll with a token the server rejects.
                match credentials_store::persist_refreshed_for_source(
                    &refreshed,
                    &source,
                    &refresh_token,
                ) {
                    Ok(Some(live)) => {
                        tracing::debug!(
                            "Another process refreshed this Claude seat first; using its tokens"
                        );
                        credentials_store::store_refreshed(&source, &live);
                        live
                    }
                    Ok(None) => {
                        credentials_store::store_refreshed(&source, &refreshed);
                        tracing::debug!("Refreshed expired Claude OAuth token");
                        refreshed
                    }
                    Err(err) => {
                        tracing::debug!(
                            "Claude OAuth token refreshed but could not persist: {err}"
                        );
                        credentials_store::store_refreshed(&source, &refreshed);
                        refreshed
                    }
                }
            }
            Err(err) => {
                tracing::debug!("Claude OAuth token refresh failed: {err}");
                credentials
            }
        }
    }

    /// Fetch usage data using OAuth credentials
    pub async fn fetch_usage(
        &self,
        credentials: &ClaudeOAuthCredentials,
    ) -> Result<OAuthUsageResponse, ProviderError> {
        if credentials.is_expired() {
            return Err(ProviderError::OAuth(
                "OAuth token expired. Run `claude` to refresh.".to_string(),
            ));
        }

        // Check for required scope
        if !credentials.scopes.is_empty() && !credentials.has_scope("user:profile") {
            return Err(ProviderError::OAuth(format!(
                "OAuth token missing 'user:profile' scope (has: {}). Run `claude setup-token` to regenerate.",
                credentials.scopes.join(", ")
            )));
        }

        if let Some(remaining) = self.rate_limit_backoff_remaining() {
            return Err(Self::rate_limited_error(remaining));
        }

        let response = self
            .client
            .get(Self::USAGE_URL)
            .header(
                "Authorization",
                format!("Bearer {}", credentials.access_token),
            )
            .header("Accept", "application/json")
            .header("anthropic-beta", "oauth-2025-04-20")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let retry_after = Self::retry_after_duration(response.headers().get(RETRY_AFTER));
            let body = response.text().await.unwrap_or_default();

            if status.as_u16() == 401 {
                return Err(ProviderError::OAuth(
                    "OAuth token invalid or expired. Run `claude` to re-authenticate.".to_string(),
                ));
            }

            if status.as_u16() == 403 && body.contains("user:profile") {
                return Err(ProviderError::OAuth(
                    "OAuth token does not meet scope requirement 'user:profile'. Run `claude setup-token` to regenerate.".to_string(),
                ));
            }

            if status.as_u16() == 429 {
                self.record_rate_limit(retry_after);
                return Err(Self::rate_limited_error(retry_after));
            }

            return Err(ProviderError::OAuth(format!(
                "API error {}: {}",
                status,
                body.chars().take(200).collect::<String>()
            )));
        }

        let usage: OAuthUsageResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(format!("Failed to parse OAuth response: {}", e)))?;

        self.clear_rate_limit();
        Ok(usage)
    }

    /// Seat key for the OAuth 429 gate. Directory seats use the same
    /// `dir_key` identity as account isolation and post-SBS-1057 pace
    /// warnings: two `CLAUDE_CONFIG_DIR`s do not share backoff, even when
    /// they share a login email. Ambient (`None`) resolves to the CLI's
    /// current config dir so an explicit ambient path is the same seat.
    fn rate_limit_seat_key(&self) -> String {
        self.config_dir()
            .map(Path::to_path_buf)
            .or_else(crate::core::ambient_claude_config_dir)
            .map(|dir| crate::core::dir_key(&dir))
            .unwrap_or_default()
    }

    fn rate_limit_gate() -> &'static Mutex<HashMap<String, Instant>> {
        RATE_LIMIT_BACKOFF_UNTIL.get_or_init(Mutex::default)
    }

    fn rate_limit_backoff_remaining(&self) -> Option<Duration> {
        let mut guard = Self::rate_limit_gate().lock().ok()?;
        let key = self.rate_limit_seat_key();
        let until = *guard.get(&key)?;
        let now = Instant::now();
        if until <= now {
            guard.remove(&key);
            None
        } else {
            Some(until.saturating_duration_since(now))
        }
    }

    fn record_rate_limit(&self, duration: Duration) {
        if let Ok(mut guard) = Self::rate_limit_gate().lock() {
            guard.insert(self.rate_limit_seat_key(), Instant::now() + duration);
        }
    }

    fn clear_rate_limit(&self) {
        if let Ok(mut guard) = Self::rate_limit_gate().lock() {
            guard.remove(&self.rate_limit_seat_key());
        }
    }

    fn retry_after_duration(value: Option<&HeaderValue>) -> Duration {
        let Some(value) = value.and_then(|value| value.to_str().ok()) else {
            return Self::DEFAULT_RATE_LIMIT_BACKOFF;
        };

        if let Ok(seconds) = value.trim().parse::<u64>() {
            return Duration::from_secs(seconds);
        }

        if let Ok(date) = DateTime::parse_from_rfc2822(value.trim()) {
            let now = Utc::now();
            let date = date.with_timezone(&Utc);
            if date > now {
                return (date - now)
                    .to_std()
                    .unwrap_or(Self::DEFAULT_RATE_LIMIT_BACKOFF);
            }
        }

        Self::DEFAULT_RATE_LIMIT_BACKOFF
    }

    fn rate_limited_error(duration: Duration) -> ProviderError {
        ProviderError::OAuth(format!(
            "Claude OAuth usage endpoint is rate limited. Retrying in about {}s; credentials were preserved.",
            duration.as_secs().max(1)
        ))
    }

    /// Build UsageSnapshot from OAuth response
    fn build_usage_snapshot(
        &self,
        response: &OAuthUsageResponse,
        credentials: &ClaudeOAuthCredentials,
    ) -> UsageSnapshot {
        let mut usage = response.build_snapshot(Self::to_rate_window);

        // Plan name from the rate limit tier, falling back to the subscription
        // type when the tier is shared across plans (Pro and Free both report
        // `default_claude_ai`).
        match super::claude_plan_label_with_subscription(
            credentials.rate_limit_tier.as_deref(),
            credentials.subscription_type.as_deref(),
        ) {
            Some(plan) => usage = usage.with_login_method(plan),
            None => usage = usage.with_login_method("Claude (OAuth)"),
        }

        usage
    }

    /// Convert OAuth usage window to RateWindow
    fn to_rate_window(
        window: &UsageWindow,
        window_minutes: Option<u32>,
        scale: UtilizationScale,
    ) -> Option<RateWindow> {
        let utilization = scale.to_percent(window.utilization?);

        let resets_at = window
            .resets_at
            .as_ref()
            .and_then(|s| parse_iso8601_date(s));

        let reset_description = resets_at.map(format_reset_date);

        Some(RateWindow::with_details(
            utilization,
            window_minutes,
            resets_at,
            reset_description,
        ))
    }
}

impl Default for ClaudeOAuthFetcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse an ISO8601 date string
fn parse_iso8601_date(s: &str) -> Option<DateTime<Utc>> {
    // Try parsing with various formats
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|| {
            // Try without timezone
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|ndt| ndt.and_utc())
        })
}

/// Format a reset date for display
fn format_reset_date(date: DateTime<Utc>) -> String {
    date.format("%b %-d at %-I:%M%p").to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        ClaudeOAuthCredentials, ClaudeOAuthFetcher, OAuthUsageResponse, UsageWindow,
        UtilizationScale,
    };
    use chrono::{Duration as ChronoDuration, TimeZone, Utc};
    use reqwest::header::HeaderValue;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    fn rate_limit_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn test_credentials() -> ClaudeOAuthCredentials {
        ClaudeOAuthCredentials {
            access_token: "token".to_string(),
            refresh_token: None,
            expires_at: None,
            scopes: vec!["user:profile".to_string()],
            rate_limit_tier: Some("default_claude_ai".to_string()),
            subscription_type: None,
        }
    }

    #[test]
    fn token_expiry_honors_the_five_minute_refresh_skew() {
        let now = Utc.with_ymd_and_hms(2026, 8, 9, 12, 0, 0).unwrap();
        let mut credentials = test_credentials();

        credentials.expires_at = Some(now + ChronoDuration::minutes(6));
        assert!(!credentials.is_expired_at(now));

        credentials.expires_at = Some(now + ChronoDuration::minutes(5));
        assert!(credentials.is_expired_at(now));

        credentials.expires_at = Some(now + ChronoDuration::minutes(1));
        assert!(credentials.is_expired_at(now));

        credentials.expires_at = Some(now - ChronoDuration::seconds(1));
        assert!(credentials.is_expired_at(now));

        credentials.expires_at = None;
        assert!(!credentials.is_expired_at(now));
    }

    #[test]
    fn converts_fractional_utilization_to_percent() {
        let window = UsageWindow {
            utilization: Some(0.23),
            resets_at: None,
        };

        let rate =
            ClaudeOAuthFetcher::to_rate_window(&window, Some(300), UtilizationScale::Fraction)
                .expect("rate window");

        assert!((rate.used_percent - 23.0).abs() < f64::EPSILON);
    }

    #[test]
    fn preserves_existing_percentage_utilization() {
        let window = UsageWindow {
            utilization: Some(23.0),
            resets_at: None,
        };

        let rate =
            ClaudeOAuthFetcher::to_rate_window(&window, Some(300), UtilizationScale::Percent)
                .expect("rate window");

        assert!((rate.used_percent - 23.0).abs() < f64::EPSILON);
    }

    /// End to end for the reported bug: a Pro seat's credentials must reach the
    /// snapshot as "Claude Pro", not the raw shared tier.
    #[test]
    fn a_pro_seat_reaches_the_snapshot_as_claude_pro() {
        let response: OAuthUsageResponse =
            serde_json::from_str(r#"{"five_hour": {"utilization": 0}}"#).expect("response parses");

        let mut credentials = test_credentials();
        credentials.subscription_type = Some("pro".to_string());
        let usage = ClaudeOAuthFetcher::new().build_usage_snapshot(&response, &credentials);
        assert_eq!(usage.login_method.as_deref(), Some("Claude Pro"));

        // Without the subscription type the shared tier still reads as the
        // product rather than leaking `default_claude_ai` to the UI.
        let usage = ClaudeOAuthFetcher::new().build_usage_snapshot(&response, &test_credentials());
        assert_eq!(usage.login_method.as_deref(), Some("Claude AI"));
    }

    /// SOU-286: a freshly reset window reports `1` (1% used). Read per value it
    /// resolved to 100%, which also fired a false "limit reached" notification.
    #[test]
    fn freshly_reset_window_reporting_one_percent_is_not_full() {
        let response: OAuthUsageResponse = serde_json::from_str(
            r#"{
                "five_hour": {"utilization": 28, "resets_at": "2026-07-21T04:00:00Z"},
                "seven_day": {"utilization": 1, "resets_at": "2026-07-28T02:00:00Z"},
                "seven_day_oauth_apps": {"utilization": 0}
            }"#,
        )
        .expect("percentage OAuth response should parse");

        let usage = ClaudeOAuthFetcher::new().build_usage_snapshot(&response, &test_credentials());

        assert_eq!(response.utilization_scale(), UtilizationScale::Percent);
        assert!((usage.primary.used_percent - 28.0).abs() < 0.001);
        assert!(
            (usage.secondary.expect("weekly").used_percent - 1.0).abs() < 0.001,
            "a weekly window one hour past its reset must not read as full"
        );
    }

    /// The reported case end to end: an account whose only activity is 1% of
    /// its session, with every other window still at zero.
    ///
    /// The existing one-percent test above passes only because `five_hour: 28`
    /// settles the scale for the whole response. Nothing settles this one, and
    /// that is exactly the payload a lightly used account sends: it rendered a
    /// 1% session as 100% used and raised an exhausted alert for it.
    #[test]
    fn a_lone_one_percent_window_is_not_read_as_exhausted() {
        let response: OAuthUsageResponse = serde_json::from_str(
            r#"{
                "five_hour": {"utilization": 1, "resets_at": "2026-07-30T07:00:00Z"},
                "seven_day": {"utilization": 0, "resets_at": "2026-08-05T08:00:00Z"}
            }"#,
        )
        .expect("percentage OAuth response should parse");

        let usage = ClaudeOAuthFetcher::new().build_usage_snapshot(&response, &test_credentials());

        assert_eq!(response.utilization_scale(), UtilizationScale::Percent);
        assert!(
            (usage.primary.used_percent - 1.0).abs() < 0.001,
            "a 1% session read as {}%",
            usage.primary.used_percent
        );
        assert!((usage.secondary.expect("weekly").used_percent - 0.0).abs() < 0.001);
    }

    /// The same `1` still means 100% when the response is genuinely fractional.
    #[test]
    fn fractional_response_keeps_a_full_window_at_one_hundred() {
        let response: OAuthUsageResponse = serde_json::from_str(
            r#"{
                "five_hour": {"utilization": 0.28, "resets_at": "2026-07-21T04:00:00Z"},
                "seven_day": {"utilization": 1.0, "resets_at": "2026-07-28T02:00:00Z"}
            }"#,
        )
        .expect("fractional OAuth response should parse");

        let usage = ClaudeOAuthFetcher::new().build_usage_snapshot(&response, &test_credentials());

        assert_eq!(response.utilization_scale(), UtilizationScale::Fraction);
        assert!((usage.primary.used_percent - 28.0).abs() < 0.001);
        assert!((usage.secondary.expect("weekly").used_percent - 100.0).abs() < 0.001);
    }

    #[test]
    fn utilization_scale_detects_unit_from_the_whole_response() {
        assert_eq!(
            UtilizationScale::detect([0.0, 1.0, 95.0]),
            UtilizationScale::Percent,
            "any value above 1.0 can only be a percentage"
        );
        assert_eq!(
            UtilizationScale::detect([0.0, 0.14, 1.0]),
            UtilizationScale::Fraction,
            "a value between 0 and 1 can only be a fraction"
        );
    }

    /// The reported bug: a session at 1% rendered as 100% used, and alerted as
    /// exhausted, on an account whose other windows were all still at zero.
    ///
    /// The recorded history showed the session jump 0 -> 100 between two reads
    /// with nothing in between, while a second account on the same build and
    /// the same API climbed 91, 92, 93 - values only a percentage scale
    /// produces. The response was percentages; `1` meant 1%.
    #[test]
    fn a_barely_used_session_is_not_read_as_exhausted() {
        assert_eq!(
            UtilizationScale::detect([1.0, 0.0]),
            UtilizationScale::Percent,
            "1 alongside only zeros is 1%, not a maxed-out fraction"
        );
        assert_eq!(
            UtilizationScale::detect([1.0, 0.0]).to_percent(1.0),
            1.0,
            "a freshly used session must not render as exhausted"
        );

        // An empty response has no evidence either way and must not invent a
        // fraction scale that would multiply later readings by 100.
        assert_eq!(UtilizationScale::detect([]), UtilizationScale::Percent);
        assert_eq!(
            UtilizationScale::detect([0.0, 0.0]),
            UtilizationScale::Percent
        );
    }

    #[test]
    fn parses_current_snake_case_oauth_usage_response() {
        let response: OAuthUsageResponse = serde_json::from_str(
            r#"{
                "five_hour": {"utilization": 1.0, "resets_at": "2026-05-22T22:10:00Z"},
                "seven_day": {"utilization": 0.14, "resets_at": "2026-05-29T10:00:00Z"},
                "seven_day_oauth_apps": {"utilization": 0.0},
                "limits": [{
                    "kind": "weekly_scoped",
                    "group": "weekly",
                    "percent": 7,
                    "resets_at": "2026-05-29T10:00:00Z",
                    "scope": {"model": {"id": null, "display_name": "Fable"}},
                    "is_active": false
                }],
                "extra_usage": {"is_enabled": true, "used_credits": 0, "monthly_limit": 1000, "currency": "USD"}
            }"#,
        )
        .expect("snake_case OAuth response should parse");

        let credentials = ClaudeOAuthCredentials {
            access_token: "token".to_string(),
            refresh_token: None,
            expires_at: None,
            scopes: vec!["user:profile".to_string()],
            rate_limit_tier: Some("default_claude_ai".to_string()),
            subscription_type: None,
        };
        let usage = ClaudeOAuthFetcher::new().build_usage_snapshot(&response, &credentials);

        assert_eq!(usage.primary.used_percent, 100.0);
        assert!((usage.secondary.expect("weekly").used_percent - 14.0).abs() < 0.001);
        let scoped = usage
            .extra_rate_windows
            .iter()
            .find(|window| window.id == "claude-weekly-scoped-fable")
            .expect("Fable scoped weekly limit");
        assert_eq!(scoped.title, "Fable only");
        assert_eq!(scoped.window.used_percent, 7.0);
    }

    #[test]
    fn parses_retry_after_seconds() {
        let header = HeaderValue::from_static("17");
        let duration = ClaudeOAuthFetcher::retry_after_duration(Some(&header));

        assert_eq!(duration, Duration::from_secs(17));
    }

    #[test]
    fn invalid_retry_after_uses_default_backoff() {
        let header = HeaderValue::from_static("not-a-date");
        let duration = ClaudeOAuthFetcher::retry_after_duration(Some(&header));

        assert_eq!(duration, ClaudeOAuthFetcher::DEFAULT_RATE_LIMIT_BACKOFF);
    }

    #[test]
    fn rate_limit_gate_blocks_and_clears() {
        let _lock = rate_limit_test_lock();
        let fetcher = ClaudeOAuthFetcher::with_config_dir(PathBuf::from(
            r"C:\Users\person\.claude-rate-limit-gate",
        ));
        fetcher.clear_rate_limit();

        fetcher.record_rate_limit(Duration::from_secs(30));
        assert!(fetcher.rate_limit_backoff_remaining().is_some());

        fetcher.clear_rate_limit();
        assert!(fetcher.rate_limit_backoff_remaining().is_none());
    }

    /// SBS-1064: one directory seat's 429 must not pause the other. These
    /// seats can share a login email across orgs — the same isolation class
    /// as post-SBS-1057 predictive pace — so backoff is keyed by config dir.
    #[test]
    fn two_directory_seats_do_not_share_oauth_rate_limit_backoff() {
        let _lock = rate_limit_test_lock();
        let personal =
            ClaudeOAuthFetcher::with_config_dir(PathBuf::from(r"C:\Users\person\.claude-personal"));
        let work =
            ClaudeOAuthFetcher::with_config_dir(PathBuf::from(r"C:\Users\person\.claude-work"));
        personal.clear_rate_limit();
        work.clear_rate_limit();

        personal.record_rate_limit(Duration::from_secs(30));
        assert!(
            personal.rate_limit_backoff_remaining().is_some(),
            "the rate-limited seat must still honor its own backoff"
        );
        assert!(
            work.rate_limit_backoff_remaining().is_none(),
            "a 429 on one directory seat must not pause the other"
        );

        work.record_rate_limit(Duration::from_secs(45));
        personal.clear_rate_limit();
        assert!(
            personal.rate_limit_backoff_remaining().is_none(),
            "clearing one seat must not clear the other"
        );
        assert!(work.rate_limit_backoff_remaining().is_some());
    }

    #[test]
    fn rate_limit_seat_key_follows_directory_identity() {
        let personal =
            ClaudeOAuthFetcher::with_config_dir(PathBuf::from(r"C:\Users\person\.claude-personal"));
        let work =
            ClaudeOAuthFetcher::with_config_dir(PathBuf::from(r"C:\Users\person\.claude-work"));
        let same_personal = ClaudeOAuthFetcher::with_config_dir(PathBuf::from(
            r"C:\Users\person\.claude-personal\",
        ));

        assert_ne!(
            personal.rate_limit_seat_key(),
            work.rate_limit_seat_key(),
            "distinct CLAUDE_CONFIG_DIRs are distinct seats"
        );
        assert_eq!(
            personal.rate_limit_seat_key(),
            same_personal.rate_limit_seat_key(),
            "trailing separators must not split one seat"
        );
    }

    #[test]
    fn same_directory_seat_shares_oauth_rate_limit_backoff() {
        let _lock = rate_limit_test_lock();
        let first = ClaudeOAuthFetcher::with_config_dir(PathBuf::from(
            r"C:\Users\person\.claude-same-seat",
        ));
        let second = ClaudeOAuthFetcher::with_config_dir(PathBuf::from(
            r"C:\Users\person\.claude-same-seat\",
        ));
        first.clear_rate_limit();
        second.clear_rate_limit();

        first.record_rate_limit(Duration::from_secs(30));
        assert!(
            second.rate_limit_backoff_remaining().is_some(),
            "two fetchers for the same config dir are one seat"
        );
        second.clear_rate_limit();
        assert!(first.rate_limit_backoff_remaining().is_none());
    }

    #[test]
    fn rate_limited_error_preserves_credentials_language() {
        let error = ClaudeOAuthFetcher::rate_limited_error(Duration::from_secs(5));
        let message = error.to_string();

        assert!(message.contains("rate limited"));
        assert!(message.contains("credentials were preserved"));
    }

    /// SBS-1040: OAuth used to mint Session (5h) 0% when five_hour was omitted.
    #[test]
    fn omitted_five_hour_stays_unknown_not_zero() {
        let response: OAuthUsageResponse = serde_json::from_str(
            r#"{"seven_day": {"utilization": 14, "resets_at": "2026-08-30T10:00:00Z"}}"#,
        )
        .expect("response parses");

        let usage = ClaudeOAuthFetcher::new().build_usage_snapshot(&response, &test_credentials());
        let session = usage
            .inactive_rate_windows
            .iter()
            .find(|window| window.id == "claude-session")
            .expect("omitted five_hour must stay unknown");
        assert_eq!(session.title, "Session (5h)");
        assert_eq!(session.state, crate::core::EnforcementState::Unavailable);
        assert!((usage.secondary.expect("weekly").used_percent - 14.0).abs() < 0.001);
    }

    /// SBS-1040: a null utilization is not 0%.
    #[test]
    fn null_utilization_stays_unknown_not_zero() {
        let window = UsageWindow {
            utilization: None,
            resets_at: Some("2026-08-23T12:00:00Z".to_string()),
        };

        assert!(
            ClaudeOAuthFetcher::to_rate_window(&window, Some(300), UtilizationScale::Percent)
                .is_none(),
            "absent utilization must not become a 0% Session"
        );

        let response: OAuthUsageResponse = serde_json::from_str(
            r#"{"five_hour": {"utilization": null}, "seven_day": {"utilization": 9}}"#,
        )
        .expect("response parses");
        let usage = ClaudeOAuthFetcher::new().build_usage_snapshot(&response, &test_credentials());
        assert!(
            usage
                .inactive_rate_windows
                .iter()
                .any(|window| window.id == "claude-session"
                    && window.state == crate::core::EnforcementState::Unavailable)
        );
        assert!((usage.secondary.expect("weekly").used_percent - 9.0).abs() < 0.001);
    }
}
