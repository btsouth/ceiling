//! Codex API client for fetching usage information
//!
//! Uses OAuth tokens stored by the Codex CLI in ~/.codex/auth.json

use crate::core::{
    CostSnapshot, NamedRateWindow, ProviderError, RateWindow, UsageSnapshot,
    format_remaining_countdown,
};
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";
const USAGE_PATH: &str = "/wham/usage";
const RESET_CREDITS_PATH: &str = "/wham/rate-limit-reset-credits";
const CREDENTIAL_CACHE_TTL: Duration = Duration::from_secs(5);

/// Keyed by auth file path: with more than one Codex account configured, a
/// single shared slot would be evicted on every alternating fetch and each
/// account would re-read from disk instead of ever hitting the cache.
static CREDENTIAL_CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedCodexCredentials>>> =
    OnceLock::new();

/// Codex API client
pub struct CodexApi {
    client: reqwest::Client,
    /// Explicit `CODEX_HOME` for a Ceiling-managed account. When `None` the
    /// client resolves the home the CLI itself would use.
    codex_home: Option<PathBuf>,
}

impl CodexApi {
    /// Whether the local Codex CLI credential file contains usable auth
    /// material. This intentionally returns only a boolean so discovery UI
    /// never receives account identity or token values.
    pub fn has_local_credentials(&self) -> bool {
        self.load_credentials().is_ok()
    }

    pub fn new() -> Self {
        // Build client with proper TLS settings
        let client = crate::core::credentialed_http_client_builder()
            .use_rustls_tls()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            codex_home: None,
        }
    }

    /// Build a client pinned to a specific `CODEX_HOME`, for tracking one of
    /// several configured Codex accounts.
    pub fn with_codex_home(codex_home: PathBuf) -> Self {
        Self {
            codex_home: Some(codex_home),
            ..Self::new()
        }
    }

    /// The same client aimed at a different account. Reuses the underlying
    /// connection pool rather than building a second one per fetch.
    pub fn scoped(&self, codex_home: PathBuf) -> Self {
        Self {
            client: self.client.clone(),
            codex_home: Some(codex_home),
        }
    }

    /// The `CODEX_HOME` this client reads from.
    ///
    /// `None` when following the CLI and no home can be resolved. The working
    /// directory is not a substitute (SBS-1021 / SBS-950).
    fn codex_home(&self) -> Option<PathBuf> {
        self.codex_home
            .clone()
            .or_else(crate::core::ambient_codex_home)
    }

    /// Identity claims for this client's home, for labeling and for keying usage
    /// by account. Never includes token material.
    pub fn identity(&self) -> Option<crate::core::CodexIdentity> {
        use crate::core::AccountIdentity;
        crate::core::CodexIdentity::read(&self.codex_home()?)
    }

    /// Fetch usage information from Codex API
    /// Returns (UsageSnapshot, optional CostSnapshot)
    pub async fn fetch_usage(
        &self,
    ) -> Result<(UsageSnapshot, Option<CostSnapshot>), ProviderError> {
        // Load credentials
        let creds = self.load_credentials()?;

        // Build request URL
        let base_url = self.resolve_base_url();
        let url = format!("{}{}", base_url, USAGE_PATH);

        // Build request
        let mut request = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", creds.access_token))
            .header("User-Agent", "CodexBar")
            .header("Accept", "application/json")
            .timeout(std::time::Duration::from_secs(30));

        if let Some(account_id) = &creds.account_id
            && !account_id.is_empty()
        {
            request = request.header("ChatGPT-Account-Id", account_id);
        }

        let response = request.send().await?;

        if response.status() == 401 || response.status() == 403 {
            return Err(ProviderError::AuthRequired);
        }

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Codex API returned {}",
                response.status()
            )));
        }

        // Parse as raw JSON first for flexibility
        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))?;

        let (mut usage, cost) = self.build_result_from_json(&json)?;
        if let Ok(reset_credits) = self.fetch_rate_limit_reset_credits(&creds, &base_url).await {
            usage = with_reset_credits(usage, &reset_credits);
        }
        Ok((usage, cost))
    }

    async fn fetch_rate_limit_reset_credits(
        &self,
        creds: &CodexCredentials,
        base_url: &str,
    ) -> Result<ResetCredits, ProviderError> {
        let mut request = self
            .client
            .get(format!("{}{}", base_url, RESET_CREDITS_PATH))
            .header("Authorization", format!("Bearer {}", creds.access_token))
            .header("User-Agent", "CodexBar")
            .header("Accept", "application/json");
        if let Some(account_id) = &creds.account_id
            && !account_id.is_empty()
        {
            request = request.header("ChatGPT-Account-Id", account_id);
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Codex reset credits returned {}",
                response.status()
            )));
        }
        decode_reset_credits(&response.bytes().await?)
    }

    fn load_credentials(&self) -> Result<CodexCredentials, ProviderError> {
        let auth_path = self.get_auth_path().ok_or_else(|| {
            ProviderError::NotInstalled("Could not resolve Codex home directory.".to_string())
        })?;

        if !auth_path.exists() {
            return Err(ProviderError::NotInstalled(format!(
                "Codex auth.json not found in {}. Run `codex login` in a terminal to sign in.",
                auth_path.display()
            )));
        }

        let modified = std::fs::metadata(&auth_path)
            .ok()
            .and_then(|metadata| metadata.modified().ok());
        if let Some(cached) = Self::cached_credentials(&auth_path, modified) {
            return Ok(cached);
        }

        let content = std::fs::read_to_string(&auth_path).map_err(|e| {
            ProviderError::Other(format!("Failed to read Codex credentials: {}", e))
        })?;

        let credentials = Self::parse_credentials_json(&content)?;
        Self::store_cached_credentials(auth_path, modified, credentials.clone());
        Ok(credentials)
    }

    fn parse_credentials_json(content: &str) -> Result<CodexCredentials, ProviderError> {
        let json: serde_json::Value = serde_json::from_str(content)
            .map_err(|e| ProviderError::Parse(format!("Invalid Codex credentials JSON: {}", e)))?;

        // Check for OPENAI_API_KEY first
        if let Some(api_key) = json.get("OPENAI_API_KEY").and_then(|v| v.as_str()) {
            let trimmed = api_key.trim();
            if !trimmed.is_empty() {
                return Ok(CodexCredentials {
                    access_token: trimmed.to_string(),
                    account_id: None,
                });
            }
        }

        // Otherwise, look for tokens object
        let tokens = json.get("tokens").ok_or_else(|| {
            ProviderError::Parse("Codex auth.json exists but contains no tokens.".to_string())
        })?;

        let access_token = tokens
            .get("access_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ProviderError::Parse("Missing access_token in Codex credentials".to_string())
            })?
            .to_string();

        let account_id = tokens
            .get("account_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        Ok(CodexCredentials {
            access_token,
            account_id,
        })
    }

    fn credential_cache() -> &'static Mutex<HashMap<PathBuf, CachedCodexCredentials>> {
        CREDENTIAL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn cached_credentials(
        path: &std::path::Path,
        modified: Option<SystemTime>,
    ) -> Option<CodexCredentials> {
        let guard = Self::credential_cache().lock().ok()?;
        let cached = guard.get(path)?;
        if cached.modified == modified && cached.loaded_at.elapsed() <= CREDENTIAL_CACHE_TTL {
            return Some(cached.credentials.clone());
        }
        None
    }

    fn store_cached_credentials(
        path: PathBuf,
        modified: Option<SystemTime>,
        credentials: CodexCredentials,
    ) {
        if let Ok(mut guard) = Self::credential_cache().lock() {
            // Drop entries that can no longer be served, so a long-lived process
            // that cycles through homes does not grow this map without bound.
            guard.retain(|_, entry| entry.loaded_at.elapsed() <= CREDENTIAL_CACHE_TTL);
            guard.insert(
                path,
                CachedCodexCredentials {
                    modified,
                    loaded_at: Instant::now(),
                    credentials,
                },
            );
        }
    }

    fn get_auth_path(&self) -> Option<PathBuf> {
        Some(self.codex_home()?.join("auth.json"))
    }

    fn resolve_base_url(&self) -> String {
        // Each home carries its own config.toml, so a per-account base URL
        // override follows the account rather than leaking across them.
        let Some(home) = self.codex_home() else {
            return DEFAULT_BASE_URL.to_string();
        };
        let config_path = home.join("config.toml");

        if let Ok(content) = std::fs::read_to_string(&config_path)
            && let Some(base_url) = parse_chatgpt_base_url(&content)
        {
            let normalized = normalize_base_url(&base_url);
            match accepted_custom_base_url(&normalized) {
                Ok(()) => return normalized,
                Err(reason) => tracing::warn!(
                    "Ignoring custom chatgpt_base_url ({reason}): {}",
                    normalized
                ),
            }
        }

        DEFAULT_BASE_URL.to_string()
    }

    fn build_result_from_json(
        &self,
        json: &serde_json::Value,
    ) -> Result<(UsageSnapshot, Option<CostSnapshot>), ProviderError> {
        // Extract plan type
        let plan_type = json
            .get("plan_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Extract rate limit info - handle multiple possible structures
        let (primary, secondary, code_review, five_hour_not_enforced) =
            self.extract_rate_limits(json);

        // Build login method string
        let login_method = plan_type.as_ref().map(|pt| match pt.as_str() {
            "guest" => "Guest".to_string(),
            "free" => "ChatGPT Free".to_string(),
            "go" => "Codex Go".to_string(),
            "plus" => "ChatGPT Plus".to_string(),
            "pro" | "pro_lite" | "prolite" | "pro-lite" => {
                if pt == "pro" {
                    "ChatGPT Pro".to_string()
                } else {
                    "Pro Lite".to_string()
                }
            }
            "team" => "ChatGPT Team".to_string(),
            "business" => "ChatGPT Business".to_string(),
            "enterprise" => "ChatGPT Enterprise".to_string(),
            "education" | "edu" => "ChatGPT Education".to_string(),
            "free_workspace" | "freeWorkspace" => "Free Workspace".to_string(),
            "quorum" => "Codex Quorum".to_string(),
            "k12" => "Codex K12".to_string(),
            other => format!("ChatGPT {}", capitalize(other)),
        });

        let mut usage = UsageSnapshot::new(primary);
        if let Some(sec) = secondary {
            usage = usage.with_secondary(sec);
        }
        if let Some(cr) = code_review {
            usage = usage.with_model_specific(cr);
        }
        if five_hour_not_enforced {
            usage = usage.with_inactive_rate_window(
                "codex-five-hour",
                "5-hour",
                "Not currently enforced by OpenAI",
            );
        }
        for extra in self.extract_additional_rate_limits(json) {
            usage.extra_rate_windows.push(extra);
        }
        if let Some(method) = login_method {
            usage = usage.with_login_method(method);
        }

        // Extract credits if present
        let cost = self.extract_credits(json);
        if let Some(balance) = Self::credit_balance(json) {
            // `balance` is credit remaining, not money spent, so it gets its own
            // info-only line rather than being passed off as cost.
            usage = usage.with_extra_rate_window(
                "codex-credit-balance",
                "Credits",
                RateWindow::with_details(0.0, None, None, Some(format!("${balance:.2} left"))),
            );
        }

        Ok((usage, cost))
    }

    fn extract_rate_limits(
        &self,
        json: &serde_json::Value,
    ) -> (RateWindow, Option<RateWindow>, Option<RateWindow>, bool) {
        // Try rate_limit object
        if let Some(rate_limit) = json.get("rate_limit") {
            let primary_opt = rate_limit
                .get("primary_window")
                .filter(|window| !is_placeholder_window(window))
                .map(|w| self.parse_window(w));

            let secondary_opt = rate_limit
                .get("secondary_window")
                .filter(|window| !is_placeholder_window(window))
                .map(|w| self.parse_window(w));

            // Codex moved the code-review meter to a top-level
            // `code_review_rate_limit`; the nested `rate_limit.code_review_window`
            // is the older shape, so accept either.
            let code_review = rate_limit
                .get("code_review_window")
                .filter(|w| !is_placeholder_window(w))
                .map(|w| self.parse_window(w))
                .or_else(|| {
                    json.get("code_review_rate_limit")
                        .filter(|w| !w.is_null() && !is_placeholder_window(w))
                        .map(|w| self.parse_window(w))
                });

            let (primary, secondary, five_hour_not_enforced) =
                normalize_codex_windows(primary_opt, secondary_opt);

            return (primary, secondary, code_review, five_hour_not_enforced);
        }

        // Try rate_limits array
        if let Some(rate_limits) = json.get("rate_limits").and_then(|v| v.as_array())
            && let Some(first) = rate_limits.first()
        {
            let primary = self.parse_window(first);
            let secondary = rate_limits.get(1).map(|w| self.parse_window(w));
            let code_review = rate_limits.get(2).map(|w| self.parse_window(w));
            return (primary, secondary, code_review, false);
        }

        // Try direct fields
        let used_percent = json
            .get("used_percent")
            .or_else(|| json.get("usage_percent"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        (RateWindow::new(used_percent), None, None, false)
    }

    fn parse_window(&self, window: &serde_json::Value) -> RateWindow {
        let used_percent = window
            .get("used_percent")
            .or_else(|| window.get("usage_percent"))
            .and_then(json_f64)
            .unwrap_or(0.0);

        let window_minutes = window
            .get("limit_window_seconds")
            .and_then(json_i64)
            .map(|s| (s / 60) as u32);

        let reset_at = window
            .get("reset_at")
            .and_then(json_i64)
            .and_then(|ts| Utc.timestamp_opt(ts, 0).single())
            .or_else(|| reset_after_to_instant(window));

        RateWindow::with_details(
            used_percent,
            window_minutes,
            reset_at,
            format_reset_countdown(reset_at),
        )
    }

    fn extract_additional_rate_limits(&self, json: &serde_json::Value) -> Vec<NamedRateWindow> {
        json.get("additional_rate_limits")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .flat_map(|entry| self.parse_additional_rate_limit(entry))
            .collect()
    }

    fn parse_additional_rate_limit(&self, entry: &serde_json::Value) -> Vec<NamedRateWindow> {
        let metered_feature = entry
            .get("metered_feature")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty());
        let limit_name = entry
            .get("limit_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty());

        let rate_limit = entry.get("rate_limit").unwrap_or(entry);
        let primary = rate_limit
            .get("primary_window")
            .filter(|window| !is_placeholder_window(window));
        let secondary = rate_limit
            .get("secondary_window")
            .filter(|window| !is_placeholder_window(window));
        if primary.is_none() && secondary.is_none() {
            return Vec::new();
        }

        let feature = metered_feature.unwrap_or_default();
        let limit = limit_name.unwrap_or_default();
        // Spark's metered feature was renamed to `codex_bengalfox`, but its
        // limit name still carries "Spark"; accept either so the dedicated
        // Spark rows keep showing instead of degrading to a generic extra.
        let is_spark = feature.eq_ignore_ascii_case("codex_spark")
            || feature.eq_ignore_ascii_case("spark")
            || feature.eq_ignore_ascii_case("codex_bengalfox")
            || limit.to_ascii_lowercase().contains("spark");

        if is_spark {
            // Spark now reports a five-hour AND a weekly window in one entry;
            // surface both rather than dropping whichever is not primary.
            let mut windows = Vec::new();
            for window in [primary, secondary].into_iter().flatten() {
                let parsed = self.parse_window(window);
                let is_weekly = parsed
                    .window_minutes
                    .is_some_and(|minutes| minutes >= 7 * 24 * 60);
                let (id, title) = if is_weekly {
                    ("codex-spark-weekly", "Codex Spark Weekly")
                } else {
                    ("codex-spark", "Codex Spark 5-hour")
                };
                if !windows
                    .iter()
                    .any(|existing: &NamedRateWindow| existing.id == id)
                {
                    windows.push(NamedRateWindow::new(id, title, parsed));
                }
            }
            return windows;
        }

        let Some(label) = limit_name.or(metered_feature) else {
            return Vec::new();
        };
        let slug = slugify(label);
        if slug.is_empty() {
            return Vec::new();
        }
        let title = titleize_limit_label(label);

        let mut windows = Vec::new();
        if let Some(window) = primary {
            windows.push(NamedRateWindow::new(
                format!("codex-{slug}"),
                title.clone(),
                self.parse_window(window),
            ));
            if let Some(window) = secondary {
                windows.push(NamedRateWindow::new(
                    format!("codex-{slug}-weekly"),
                    format!("{title} Weekly"),
                    self.parse_window(window),
                ));
            }
        } else if let Some(window) = secondary {
            windows.push(NamedRateWindow::new(
                format!("codex-{slug}"),
                title,
                self.parse_window(window),
            ));
        }
        windows
    }

    /// The remaining credit balance, when the account has metered credits.
    ///
    /// This is what is *left*, never what was spent. Callers must not put it in
    /// a `CostSnapshot`'s `used` slot.
    fn credit_balance(json: &serde_json::Value) -> Option<f64> {
        let credits = json.get("credits")?;
        if !credits
            .get("has_credits")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return None;
        }
        if credits
            .get("unlimited")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return None;
        }
        credits.get("balance").and_then(json_f64)
    }

    /// Money actually spent against a credit allowance.
    ///
    /// Spend is only knowable when the account also reports a spend-control
    /// limit: `used = limit - balance`. A balance on its own says how much is
    /// left, so reporting it as `used` inverts the number — it would fall as
    /// the user spends and read $0 exactly when the account is exhausted. The
    /// balance is surfaced separately as its own line instead.
    fn extract_credits(&self, json: &serde_json::Value) -> Option<CostSnapshot> {
        let balance = Self::credit_balance(json)?;
        // OpenAI now nests the spend control under `spend_control`; the
        // top-level and `rate_limit` copies are older shapes. A present but
        // `null` value must not block the fallbacks.
        let limit = json
            .get("spend_control")
            .and_then(|spend_control| spend_control.get("individual_limit"))
            .filter(|limit| !limit.is_null())
            .or_else(|| {
                json.get("individual_limit")
                    .filter(|limit| !limit.is_null())
            })
            .or_else(|| {
                json.get("rate_limit")
                    .and_then(|r| r.get("individual_limit"))
                    .filter(|limit| !limit.is_null())
            })?;
        serde_json::from_value::<SpendControlLimitSnapshot>(limit.clone())
            .ok()?
            .to_cost_snapshot(balance)
    }

    fn build_result(
        &self,
        response: UsageResponse,
    ) -> Result<(UsageSnapshot, Option<CostSnapshot>), ProviderError> {
        let primary_window = response
            .rate_limit
            .as_ref()
            .and_then(|rate_limit| rate_limit.primary_window.as_ref())
            .map(rate_window_from_snapshot);
        let secondary_window = response
            .rate_limit
            .as_ref()
            .and_then(|rate_limit| rate_limit.secondary_window.as_ref())
            .map(rate_window_from_snapshot);
        let (primary, secondary, five_hour_not_enforced) =
            normalize_codex_windows(primary_window, secondary_window);

        // Extract code review rate window
        let code_review = response
            .rate_limit
            .as_ref()
            .and_then(|rl| rl.code_review_window.as_ref())
            .map(|window| {
                let reset_at = timestamp_to_datetime(window.reset_at);
                RateWindow::with_details(
                    window.used_percent as f64,
                    window.limit_window_seconds.map(|s| (s / 60) as u32),
                    reset_at,
                    format_reset_countdown(reset_at),
                )
            });

        // Build usage snapshot
        let login_method = response.plan_type.as_ref().map(|pt| match pt.as_str() {
            "guest" => "Guest".to_string(),
            "free" => "ChatGPT Free".to_string(),
            "go" => "ChatGPT Go".to_string(),
            "plus" => "ChatGPT Plus".to_string(),
            "pro" => "ChatGPT Pro".to_string(),
            "team" => "ChatGPT Team".to_string(),
            "business" => "ChatGPT Business".to_string(),
            "enterprise" => "ChatGPT Enterprise".to_string(),
            "education" | "edu" => "ChatGPT Education".to_string(),
            other => format!("ChatGPT {}", capitalize(other)),
        });

        let mut usage = UsageSnapshot::new(primary);
        if let Some(sec) = secondary {
            usage = usage.with_secondary(sec);
        }
        if let Some(cr) = code_review {
            usage = usage.with_model_specific(cr);
        }
        if five_hour_not_enforced {
            usage = usage.with_inactive_rate_window(
                "codex-five-hour",
                "5-hour",
                "Not currently enforced by OpenAI",
            );
        }
        if let Some(method) = login_method {
            usage = usage.with_login_method(method);
        }

        // Build cost snapshot if credits are present
        let credit_limit = response.individual_limit.as_ref().or_else(|| {
            response
                .rate_limit
                .as_ref()
                .and_then(|rate_limit| rate_limit.individual_limit.as_ref())
        });
        let cost = response.credits.as_ref().and_then(|credits| {
            if credits.has_credits() {
                let balance = credits.balance.unwrap_or(0.0);
                if credits.unlimited() {
                    None // Unlimited credits, no need to show
                } else {
                    // Without a limit there is no way to know what was spent;
                    // `balance` is what remains. Never report it as `used`.
                    credit_limit.and_then(|limit| limit.to_cost_snapshot(balance))
                }
            } else {
                None
            }
        });

        Ok((usage, cost))
    }
}

impl Default for CodexApi {
    fn default() -> Self {
        Self::new()
    }
}

// --- Data structures ---

#[derive(Clone)]
struct CodexCredentials {
    access_token: String,
    account_id: Option<String>,
}

struct CachedCodexCredentials {
    modified: Option<SystemTime>,
    loaded_at: Instant,
    credentials: CodexCredentials,
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    plan_type: Option<String>,
    rate_limit: Option<RateLimitDetails>,
    credits: Option<CreditDetails>,
    #[serde(default, alias = "individualLimit")]
    individual_limit: Option<SpendControlLimitSnapshot>,
}

#[derive(Debug, Deserialize)]
struct RateLimitDetails {
    primary_window: Option<WindowSnapshot>,
    secondary_window: Option<WindowSnapshot>,
    code_review_window: Option<WindowSnapshot>,
    #[serde(default, alias = "individualLimit")]
    individual_limit: Option<SpendControlLimitSnapshot>,
}

#[derive(Debug, Deserialize)]
struct WindowSnapshot {
    used_percent: i32,
    reset_at: Option<i64>,
    limit_window_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CreditDetails {
    has_credits: Option<bool>,
    unlimited: Option<bool>,
    balance: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct SpendControlLimitSnapshot {
    #[serde(default, deserialize_with = "flexible_number::f64")]
    limit: Option<f64>,
    #[serde(default, deserialize_with = "flexible_number::f64")]
    used: Option<f64>,
    #[serde(
        default,
        alias = "remainingPercent",
        deserialize_with = "flexible_number::f64"
    )]
    remaining_percent: Option<f64>,
    #[serde(default, alias = "resetsAt", deserialize_with = "flexible_number::i64")]
    resets_at: Option<i64>,
}

/// OpenAI mixes JSON numbers and decimal strings for the same field across
/// responses, so accept either shape when decoding money and reset instants.
mod flexible_number {
    use serde::{Deserialize, Deserializer};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumberOrString {
        Number(f64),
        String(String),
    }

    pub fn f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match Option::<NumberOrString>::deserialize(deserializer)? {
            Some(NumberOrString::Number(value)) => Some(value),
            Some(NumberOrString::String(value)) => value.trim().parse::<f64>().ok(),
            None => None,
        })
    }

    pub fn i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match Option::<NumberOrString>::deserialize(deserializer)? {
            Some(NumberOrString::Number(value)) => Some(value as i64),
            Some(NumberOrString::String(value)) => value.trim().parse::<i64>().ok(),
            None => None,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ResetCredits {
    #[serde(default)]
    credits: Vec<serde_json::Value>,
    #[serde(default)]
    available_count: u32,
}

fn decode_reset_credits(data: &[u8]) -> Result<ResetCredits, ProviderError> {
    serde_json::from_slice(data)
        .map_err(|e| ProviderError::Parse(format!("Failed to parse Codex reset credits: {e}")))
}

impl CreditDetails {
    // Helper to safely check has_credits
    fn has_credits(&self) -> bool {
        self.has_credits.unwrap_or(false)
    }

    fn unlimited(&self) -> bool {
        self.unlimited.unwrap_or(false)
    }
}

impl SpendControlLimitSnapshot {
    fn to_cost_snapshot(&self, balance: f64) -> Option<CostSnapshot> {
        let limit = self
            .limit
            .filter(|limit| limit.is_finite() && *limit >= 0.0)?;
        let used = self
            .used
            .filter(|used| used.is_finite() && *used >= 0.0)
            .or_else(|| {
                self.remaining_percent
                    .filter(|pct| pct.is_finite() && *pct >= 0.0)
                    .map(|remaining| limit * (1.0 - (remaining / 100.0)))
            })
            .unwrap_or_else(|| (limit - balance).max(0.0));
        let mut cost =
            CostSnapshot::new(used.clamp(0.0, limit), "USD", "Monthly credits").with_limit(limit);
        if let Some(resets_at) = timestamp_to_datetime(self.resets_at) {
            cost = cost.with_resets_at(resets_at);
        }
        Some(cost)
    }
}

// --- Helper functions ---

fn timestamp_to_datetime(timestamp: Option<i64>) -> Option<DateTime<Utc>> {
    timestamp.and_then(|ts| Utc.timestamp_opt(ts, 0).single())
}

fn with_reset_credits(mut usage: UsageSnapshot, reset_credits: &ResetCredits) -> UsageSnapshot {
    // Record the observed count even when it is zero. A successful fetch that
    // reports no banked resets is a real, trustworthy reading, and surfaces show
    // an explicit "0 resets" state so the feature stays discoverable. `None`
    // remains reserved for "not fetched / unknown" (e.g. the sub-request failed).
    usage.reset_credits_available = Some(reset_credits.available_count);
    usage
}

fn rate_window_from_snapshot(window: &WindowSnapshot) -> RateWindow {
    let reset_at = timestamp_to_datetime(window.reset_at);
    RateWindow::with_details(
        window.used_percent as f64,
        window
            .limit_window_seconds
            .map(|seconds| (seconds / 60) as u32),
        reset_at,
        format_reset_countdown(reset_at),
    )
}

fn is_five_hour_window(window: &RateWindow) -> bool {
    window
        .window_minutes
        .is_some_and(|minutes| minutes <= 12 * 60)
}

fn is_weekly_window(window: &RateWindow) -> bool {
    window
        .window_minutes
        .is_some_and(|minutes| minutes > 12 * 60 && minutes <= 14 * 24 * 60)
}

/// Keep Codex's semantic windows stable even when OpenAI omits or reorders
/// `primary_window` and `secondary_window` in an otherwise valid response.
fn normalize_codex_windows(
    primary: Option<RateWindow>,
    secondary: Option<RateWindow>,
) -> (RateWindow, Option<RateWindow>, bool) {
    match (primary, secondary) {
        (Some(primary), Some(secondary))
            if is_weekly_window(&primary) && is_five_hour_window(&secondary) =>
        {
            (secondary, Some(primary), false)
        }
        (Some(primary), secondary)
            if is_weekly_window(&primary)
                && !secondary.as_ref().is_some_and(is_five_hour_window) =>
        {
            (primary, secondary, true)
        }
        (Some(primary), secondary) => (primary, secondary, false),
        (None, Some(secondary)) => {
            let five_hour_not_enforced = is_weekly_window(&secondary);
            (secondary, None, five_hour_not_enforced)
        }
        (None, None) => (RateWindow::new(0.0), None, false),
    }
}

fn json_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|value| value as f64))
        .or_else(|| value.as_str()?.trim().parse::<f64>().ok())
}

/// Integers that OpenAI may encode as JSON numbers or as decimal strings.
fn json_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value as i64))
        .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
}

/// Derive an absolute reset instant when the API only reports a countdown.
fn reset_after_to_instant(window: &serde_json::Value) -> Option<DateTime<Utc>> {
    let seconds = window.get("reset_after_seconds").and_then(json_i64)?;
    (seconds > 0).then(|| Utc::now() + chrono::Duration::seconds(seconds))
}

fn is_placeholder_window(window: &serde_json::Value) -> bool {
    let has_usage = window
        .get("used_percent")
        .or_else(|| window.get("usage_percent"))
        .and_then(json_f64)
        .is_some();
    let has_duration = window
        .get("limit_window_seconds")
        .and_then(json_i64)
        .is_some();
    let has_reset = window.get("reset_at").is_some() || window.get("reset_after_seconds").is_some();

    !has_usage && !has_duration && !has_reset
}

fn slugify(label: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;

    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash && !slug.is_empty() {
            slug.push('-');
            previous_dash = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

fn titleize_limit_label(label: &str) -> String {
    label
        .split(['_', '-', ' '])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first
                    .to_uppercase()
                    .chain(chars.flat_map(char::to_lowercase))
                    .collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

fn format_reset_countdown(reset_at: Option<DateTime<Utc>>) -> Option<String> {
    let dt = reset_at?;
    let now = Utc::now();
    if dt <= now {
        return Some("now".to_string());
    }
    Some(format_remaining_countdown(dt - now))
}

fn parse_chatgpt_base_url(config_content: &str) -> Option<String> {
    for line in config_content.lines() {
        // Skip comments
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        // Look for chatgpt_base_url = "..."
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if key == "chatgpt_base_url" {
                let mut value = value.trim();
                // Remove quotes
                if (value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\''))
                {
                    value = &value[1..value.len() - 1];
                }
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

/// Whether a custom `chatgpt_base_url` may receive the Codex access token.
///
/// Every usage refresh sends `Authorization: Bearer {access_token}` to this
/// URL, so the gate is exfiltration-critical and must be host-based. The
/// previous prefix test (`starts_with("http://localhost")`) admitted
/// `http://localhost@evil.example`, where `localhost` is userinfo and the real
/// host is remote, and `http://localhost.evil.example`, where it is merely a
/// label of a remote name. Both then received the token in cleartext.
fn accepted_custom_base_url(raw: &str) -> Result<(), &'static str> {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return Err("not a valid URL");
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs with user info are not accepted");
    }
    let Some(host) = url.host_str() else {
        return Err("must include a host");
    };
    if host.contains('%') || host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("host contains encoded or invalid characters");
    }
    match url.scheme() {
        "https" => Ok(()),
        // Plaintext is tolerated only for a proxy on this machine, where the
        // token never leaves the host.
        "http" if crate::providers::is_loopback_url(&url) => Ok(()),
        "http" => Err("must be HTTPS unless the host is loopback"),
        _ => Err("must use HTTP or HTTPS"),
    }
}

fn normalize_base_url(url: &str) -> String {
    let mut trimmed = url.trim().to_string();
    if trimmed.is_empty() {
        return DEFAULT_BASE_URL.to_string();
    }

    // Remove trailing slashes
    while trimmed.ends_with('/') {
        trimmed.pop();
    }

    // Add /backend-api if needed
    if (trimmed.starts_with("https://chatgpt.com")
        || trimmed.starts_with("https://chat.openai.com"))
        && !trimmed.contains("/backend-api")
    {
        trimmed.push_str("/backend-api");
    }

    trimmed
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().chain(chars).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const AUTH_JSON: &str =
        r#"{"tokens":{"access_token":"from-account-dir","account_id":"acct-scoped"}}"#;

    /// SBS-632: the old gate was `starts_with`, so a remote host that merely
    /// began with a loopback literal received the Bearer access token in
    /// cleartext on every usage refresh.
    #[test]
    fn a_remote_host_wearing_a_loopback_prefix_is_rejected() {
        for raw in [
            // `localhost` / `127.0.0.1` as userinfo; the host is remote.
            "http://localhost@evil.example",
            "http://127.0.0.1@evil.example",
            "http://localhost:pass@evil.example",
            // Loopback literal as a label of a remote name.
            "http://localhost.evil.example",
            "http://127.0.0.1.evil.example",
            // Plain remote cleartext, the case the gate always meant to catch.
            "http://evil.example",
            // Non-HTTP schemes must not slip through either.
            "file:///etc/passwd",
        ] {
            assert!(
                accepted_custom_base_url(raw).is_err(),
                "{raw} must not be accepted as a custom base URL"
            );
        }
    }

    /// A local proxy is the reason plaintext is tolerated at all, so the
    /// legitimate forms must keep working.
    #[test]
    fn loopback_and_https_bases_are_still_accepted() {
        for raw in [
            "http://127.0.0.1",
            "http://127.0.0.1:8080",
            "http://127.0.0.1:8080/backend-api",
            "http://localhost",
            "http://localhost:1455/backend-api",
            "http://[::1]:8080",
            "https://chatgpt.com/backend-api",
            "https://proxy.internal.example/backend-api",
        ] {
            assert!(
                accepted_custom_base_url(raw).is_ok(),
                "{raw} must remain a usable custom base URL"
            );
        }
    }

    /// The rejected URL must not reach the network: resolve_base_url falls back
    /// to the default backend rather than returning the attacker's host.
    #[test]
    fn a_rejected_base_url_falls_back_to_the_default_backend() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            home.path().join("config.toml"),
            "chatgpt_base_url = \"http://localhost@evil.example\"\n",
        )
        .expect("write config");

        let api = CodexApi::new().scoped(home.path().to_path_buf());
        assert_eq!(api.resolve_base_url(), DEFAULT_BASE_URL);
    }

    #[test]
    fn a_loopback_base_url_is_used_as_written() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            home.path().join("config.toml"),
            "chatgpt_base_url = \"http://127.0.0.1:1455/backend-api\"\n",
        )
        .expect("write config");

        let api = CodexApi::new().scoped(home.path().to_path_buf());
        assert_eq!(api.resolve_base_url(), "http://127.0.0.1:1455/backend-api");
    }

    #[test]
    fn a_scoped_client_reads_its_own_account_directory() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::write(home.path().join("auth.json"), AUTH_JSON).expect("write auth");

        let api = CodexApi::new().scoped(home.path().to_path_buf());
        let credentials = api.load_credentials().expect("credentials");

        assert_eq!(credentials.access_token, "from-account-dir");
        assert_eq!(credentials.account_id.as_deref(), Some("acct-scoped"));
        assert_eq!(api.get_auth_path(), Some(home.path().join("auth.json")));
    }

    #[test]
    fn a_scoped_client_reports_its_own_account_identity() {
        // Payload: {"email":"work@example.com",
        //   "https://api.openai.com/auth":{"chatgpt_plan_type":"team"}}
        let payload = "eyJlbWFpbCI6IndvcmtAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9wbGFuX3R5cGUiOiJ0ZWFtIn19";
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            home.path().join("auth.json"),
            format!(
                r#"{{"tokens":{{"id_token":"h.{payload}.s","access_token":"secret","account_id":"acct-work"}}}}"#
            ),
        )
        .expect("write auth");

        // This is what stamps `account_email` on the snapshot, which is what
        // capacity baselines and enforcement expectations are scoped by.
        let identity = CodexApi::new()
            .scoped(home.path().to_path_buf())
            .identity()
            .expect("identity");

        assert_eq!(identity.email.as_deref(), Some("work@example.com"));
        assert_eq!(identity.plan_type.as_deref(), Some("team"));
    }

    #[test]
    fn an_account_directory_without_credentials_reports_itself() {
        let home = tempfile::tempdir().expect("tempdir");

        // `CodexCredentials` has no `Debug` on purpose, so unwrap the error by hand.
        let Err(error) = CodexApi::new()
            .scoped(home.path().to_path_buf())
            .load_credentials()
        else {
            panic!("a directory with no auth.json cannot resolve credentials");
        };

        // Naming the directory matters once several accounts exist: "not signed
        // in" is useless if it does not say which account.
        assert!(
            error
                .to_string()
                .contains(&home.path().display().to_string()),
            "error should name the account directory, got: {error}"
        );
    }

    /// Guards correctness under alternation, which is what a refresh cycle does
    /// with several accounts configured. Note this does not by itself prove the
    /// per-path credential cache: the previous single-slot cache also returned
    /// the right token here, it just never hit.
    #[test]
    fn alternating_between_accounts_always_serves_each_accounts_own_token() {
        let personal = tempfile::tempdir().expect("tempdir");
        let work = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            personal.path().join("auth.json"),
            r#"{"tokens":{"access_token":"personal-token","account_id":"acct-personal"}}"#,
        )
        .expect("write personal");
        std::fs::write(
            work.path().join("auth.json"),
            r#"{"tokens":{"access_token":"work-token","account_id":"acct-work"}}"#,
        )
        .expect("write work");

        let api = CodexApi::new();
        let personal_api = api.scoped(personal.path().to_path_buf());
        let work_api = api.scoped(work.path().to_path_buf());

        for _ in 0..3 {
            assert_eq!(
                personal_api
                    .load_credentials()
                    .expect("personal")
                    .access_token,
                "personal-token"
            );
            assert_eq!(
                work_api.load_credentials().expect("work").access_token,
                "work-token"
            );
        }
    }

    #[test]
    fn parses_codex_credentials_without_retaining_refresh_token() {
        let credentials = CodexApi::parse_credentials_json(
            r#"{
                "tokens": {
                    "access_token": "access",
                    "refresh_token": "refresh",
                    "account_id": "acct_123"
                }
            }"#,
        )
        .expect("credentials");

        assert_eq!(credentials.access_token, "access");
        assert_eq!(credentials.account_id.as_deref(), Some("acct_123"));
    }

    #[test]
    fn decodes_reset_credits() {
        let credits = decode_reset_credits(br#"{"available_count":2,"credits":[{"id":"a"}]}"#)
            .expect("reset credits");
        assert_eq!(credits.available_count, 2);
        assert_eq!(credits.credits.len(), 1);
    }

    #[test]
    fn zero_reset_credits_are_recorded_as_a_known_zero() {
        // A successful fetch reporting no banked resets must surface as Some(0),
        // not None, so the UI can show an explicit, discoverable "0 resets"
        // state instead of hiding the feature entirely.
        let usage = with_reset_credits(
            UsageSnapshot::new(RateWindow::default()),
            &ResetCredits {
                credits: Vec::new(),
                available_count: 0,
            },
        );
        assert_eq!(usage.reset_credits_available, Some(0));
    }

    #[test]
    fn maps_codex_spark_additional_rate_limits() {
        let api = CodexApi::new();
        let (usage, _) = api
            .build_result_from_json(&json!({
                "plan_type": "pro",
                "rate_limit": {
                    "primary_window": { "used_percent": 20, "limit_window_seconds": 18000 },
                    "secondary_window": { "used_percent": 40, "limit_window_seconds": 604800 }
                },
                "additional_rate_limits": [
                    {
                        "limit_name": "Codex Spark",
                        "metered_feature": "codex_spark",
                        "rate_limit": {
                            "primary_window": { "used_percent": "17", "limit_window_seconds": 18000 }
                        }
                    },
                    {
                        "limit_name": "Codex Spark Weekly",
                        "metered_feature": "codex_spark",
                        "rate_limit": {
                            "secondary_window": { "used_percent": 62, "limit_window_seconds": 604800 }
                        }
                    }
                ]
            }))
            .expect("codex usage");

        assert_eq!(usage.extra_rate_windows.len(), 2);
        assert_eq!(usage.extra_rate_windows[0].id, "codex-spark");
        assert_eq!(usage.extra_rate_windows[0].title, "Codex Spark 5-hour");
        assert_eq!(usage.extra_rate_windows[0].window.used_percent, 17.0);
        assert_eq!(usage.extra_rate_windows[1].id, "codex-spark-weekly");
        assert_eq!(usage.extra_rate_windows[1].title, "Codex Spark Weekly");
        assert_eq!(usage.extra_rate_windows[1].window.used_percent, 62.0);
    }

    #[test]
    fn preserves_a_lifted_five_hour_window_when_only_weekly_is_reported() {
        let api = CodexApi::new();
        let (usage, _) = api
            .build_result_from_json(&json!({
                "plan_type": "pro",
                "rate_limit": {
                    "secondary_window": {
                        "used_percent": 24,
                        "limit_window_seconds": 604800
                    }
                }
            }))
            .expect("codex usage");

        assert_eq!(usage.primary.window_minutes, Some(604800 / 60));
        assert_eq!(usage.primary.used_percent, 24.0);
        assert_eq!(usage.inactive_rate_windows.len(), 1);
        assert_eq!(usage.inactive_rate_windows[0].id, "codex-five-hour");
        assert_eq!(
            usage.inactive_rate_windows[0].description,
            "Not currently enforced by OpenAI"
        );
    }

    #[test]
    fn codex_window_fixtures_preserve_normal_lifted_and_restored_states() {
        let api = CodexApi::new();
        let normal_json: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/codex/normal.json"))
                .expect("normal fixture");
        let weekly_only_json: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/codex/weekly-only.json"))
                .expect("weekly-only fixture");
        let restored_json: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/codex/restored.json"))
                .expect("restored fixture");

        let (normal, _) = api
            .build_result_from_json(&normal_json)
            .expect("normal usage");
        assert_eq!(normal.primary.window_minutes, Some(300));
        assert_eq!(
            normal
                .secondary
                .as_ref()
                .and_then(|window| window.window_minutes),
            Some(10_080)
        );
        assert!(normal.inactive_rate_windows.is_empty());
        assert_eq!(normal.extra_rate_windows[0].id, "codex-spark");

        let (weekly_only, _) = api
            .build_result_from_json(&weekly_only_json)
            .expect("weekly-only usage");
        assert_eq!(weekly_only.primary.window_minutes, Some(10_080));
        assert!(weekly_only.secondary.is_none());
        assert_eq!(weekly_only.inactive_rate_windows.len(), 1);
        assert_eq!(weekly_only.inactive_rate_windows[0].id, "codex-five-hour");
        assert_eq!(weekly_only.extra_rate_windows[0].id, "codex-spark-weekly");

        let weekly_only = with_reset_credits(
            weekly_only,
            &ResetCredits {
                credits: vec![json!({ "id": "credit-1" })],
                available_count: 1,
            },
        );
        assert_eq!(weekly_only.reset_credits_available, Some(1));
        assert_eq!(weekly_only.extra_rate_windows.len(), 1);
        assert_eq!(weekly_only.extra_rate_windows[0].id, "codex-spark-weekly");

        let (restored, _) = api
            .build_result_from_json(&restored_json)
            .expect("restored usage");
        assert_eq!(restored.primary.window_minutes, Some(300));
        assert_eq!(
            restored
                .secondary
                .as_ref()
                .and_then(|window| window.window_minutes),
            Some(10_080)
        );
        assert!(restored.inactive_rate_windows.is_empty());
        assert_eq!(restored.extra_rate_windows[0].id, "codex-spark-weekly");
    }

    #[test]
    fn astra_era_payload_surfaces_weekly_primary_and_both_spark_windows() {
        let api = CodexApi::new();
        let json: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/codex/astra-era.json"))
                .expect("astra-era fixture");

        let (usage, cost) = api.build_result_from_json(&json).expect("codex usage");

        // The rate-limited account reports only the weekly window; it stays the
        // reading, and the lifted five-hour window stays explicit.
        assert_eq!(usage.primary.window_minutes, Some(10_080));
        assert_eq!(usage.primary.used_percent, 100.0);
        assert!(usage.secondary.is_none());
        assert_eq!(usage.inactive_rate_windows.len(), 1);
        assert_eq!(usage.inactive_rate_windows[0].id, "codex-five-hour");

        // Spark reports a five-hour and a weekly window in one entry; both rows
        // must survive instead of only the primary one.
        let spark: Vec<_> = usage
            .extra_rate_windows
            .iter()
            .filter(|window| window.id.starts_with("codex-spark"))
            .collect();
        assert_eq!(spark.len(), 2, "got {:?}", usage.extra_rate_windows);
        let five_hour = spark
            .iter()
            .find(|window| window.id == "codex-spark")
            .expect("spark five-hour row");
        assert_eq!(five_hour.window.window_minutes, Some(300));
        let weekly = spark
            .iter()
            .find(|window| window.id == "codex-spark-weekly")
            .expect("spark weekly row");
        assert_eq!(weekly.window.window_minutes, Some(10_080));

        assert!(
            cost.is_none(),
            "no credits and no spend limit means no cost"
        );
    }

    #[test]
    fn string_encoded_numbers_and_spend_control_still_produce_a_cost() {
        let api = CodexApi::new();
        let (usage, cost) = api
            .build_result_from_json(&json!({
                "plan_type": "pro",
                "rate_limit": {
                    "primary_window": {
                        "used_percent": "12.5",
                        "limit_window_seconds": "604800"
                    }
                },
                "credits": { "has_credits": true, "unlimited": false, "balance": "10.0" },
                "spend_control": {
                    "reached": false,
                    "individual_limit": { "limit": "50", "resetsAt": "1790113323" }
                }
            }))
            .expect("codex usage");

        assert_eq!(usage.primary.used_percent, 12.5);
        assert_eq!(usage.primary.window_minutes, Some(10_080));

        let cost = cost.expect("spend_control.individual_limit must be honored");
        assert!((cost.used - 40.0).abs() < 0.01, "used = limit - balance");
        assert_eq!(cost.limit, Some(50.0));
        assert!(cost.resets_at.is_some());
    }

    #[test]
    fn null_nested_spend_limit_falls_back_to_the_top_level_limit() {
        let api = CodexApi::new();
        let (_, cost) = api
            .build_result_from_json(&json!({
                "rate_limit": {
                    "primary_window": { "used_percent": 10, "limit_window_seconds": 18000 }
                },
                "credits": { "has_credits": true, "unlimited": false, "balance": 10.0 },
                "spend_control": { "reached": false, "individual_limit": null },
                "individual_limit": { "limit": 50.0 }
            }))
            .expect("codex usage");

        let cost = cost.expect("a null nested limit must not hide the top-level one");
        assert!((cost.used - 40.0).abs() < 0.01);
        assert_eq!(cost.limit, Some(50.0));
    }

    #[test]
    fn top_level_code_review_rate_limit_is_surfaced() {
        let api = CodexApi::new();
        let (usage, _) = api
            .build_result_from_json(&json!({
                "rate_limit": {
                    "primary_window": { "used_percent": 10, "limit_window_seconds": 18000 }
                },
                "code_review_rate_limit": {
                    "used_percent": 30,
                    "limit_window_seconds": 604800
                }
            }))
            .expect("codex usage");

        let code_review = usage.model_specific.expect("code review window");
        assert_eq!(code_review.used_percent, 30.0);
        assert_eq!(code_review.window_minutes, Some(10_080));
    }

    #[test]
    fn reset_countdown_only_payload_still_has_a_reset_instant() {
        let api = CodexApi::new();
        let (usage, _) = api
            .build_result_from_json(&json!({
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 5,
                        "limit_window_seconds": 18000,
                        "reset_after_seconds": 3600
                    }
                }
            }))
            .expect("codex usage");

        assert!(usage.primary.resets_at.is_some());
    }

    #[test]
    fn normalizes_reordered_codex_windows_by_cadence() {
        let weekly = RateWindow::with_details(40.0, Some(10_080), None, None);
        let five_hour = RateWindow::with_details(10.0, Some(300), None, None);

        let (primary, secondary, lifted) = normalize_codex_windows(Some(weekly), Some(five_hour));

        assert_eq!(primary.window_minutes, Some(300));
        assert_eq!(
            secondary.and_then(|window| window.window_minutes),
            Some(10_080)
        );
        assert!(!lifted);
    }

    #[test]
    fn credit_balance_is_never_reported_as_money_spent() {
        // `balance` is what is LEFT. Reporting it as `used` inverted the
        // number: it fell as the user spent and read $0 at exhaustion.
        let api = CodexApi::new();
        let (usage, cost) = api
            .build_result_from_json(&json!({
                "rate_limit": {
                    "primary_window": { "used_percent": 10, "limit_window_seconds": 18000 }
                },
                "credits": { "has_credits": true, "unlimited": false, "balance": 10.0 }
            }))
            .expect("codex usage");

        // No spend-control limit, so spend is unknowable and none is claimed.
        assert!(cost.is_none(), "a balance alone is not spend");
        // The balance is still shown, as its own line.
        let credits = usage
            .extra_rate_windows
            .iter()
            .find(|w| w.id == "codex-credit-balance")
            .expect("credit balance line");
        assert_eq!(credits.title, "Credits");
        assert_eq!(
            credits.window.reset_description.as_deref(),
            Some("$10.00 left")
        );
    }

    #[test]
    fn credit_spend_is_derived_from_the_spend_control_limit() {
        // With a limit, real spend is limit - balance: $40 of $50.
        let api = CodexApi::new();
        let (_, cost) = api
            .build_result_from_json(&json!({
                "rate_limit": {
                    "primary_window": { "used_percent": 10, "limit_window_seconds": 18000 }
                },
                "credits": { "has_credits": true, "unlimited": false, "balance": 10.0 },
                "individual_limit": { "limit": 50.0 }
            }))
            .expect("codex usage");

        let cost = cost.expect("cost from spend-control limit");
        assert!((cost.used - 40.0).abs() < 0.01, "used = limit - balance");
        assert_eq!(cost.limit, Some(50.0));
    }

    #[test]
    fn unlimited_credits_report_neither_spend_nor_balance() {
        let api = CodexApi::new();
        let (usage, cost) = api
            .build_result_from_json(&json!({
                "rate_limit": {
                    "primary_window": { "used_percent": 10, "limit_window_seconds": 18000 }
                },
                "credits": { "has_credits": true, "unlimited": true, "balance": 10.0 }
            }))
            .expect("codex usage");

        assert!(cost.is_none());
        assert!(
            !usage
                .extra_rate_windows
                .iter()
                .any(|w| w.id == "codex-credit-balance")
        );
    }

    #[test]
    fn ignores_placeholder_additional_rate_limits() {
        let api = CodexApi::new();
        let (usage, _) = api
            .build_result_from_json(&json!({
                "rate_limit": {
                    "primary_window": { "used_percent": 0, "limit_window_seconds": 18000 }
                },
                "additional_rate_limits": [
                    {
                        "limit_name": "placeholder",
                        "metered_feature": "placeholder",
                        "rate_limit": { "primary_window": {} }
                    }
                ]
            }))
            .expect("codex usage");

        assert!(usage.extra_rate_windows.is_empty());
    }

    #[test]
    fn maps_top_level_individual_credit_limit_to_cost_snapshot() {
        let api = CodexApi::new();
        let (_, cost) = api
            .build_result(UsageResponse {
                plan_type: None,
                rate_limit: None,
                credits: Some(CreditDetails {
                    has_credits: Some(true),
                    unlimited: Some(false),
                    balance: Some(7.5),
                }),
                individual_limit: Some(SpendControlLimitSnapshot {
                    limit: Some(20.0),
                    used: Some(12.5),
                    remaining_percent: None,
                    resets_at: Some(1783036800),
                }),
            })
            .expect("codex result");
        let cost = cost.expect("cost");
        assert_eq!(cost.used, 12.5);
        assert_eq!(cost.limit, Some(20.0));
        assert!(cost.resets_at.is_some());
    }

    #[test]
    fn maps_nested_individual_credit_limit_to_cost_snapshot() {
        let api = CodexApi::new();
        let (_, cost) = api
            .build_result(UsageResponse {
                plan_type: None,
                rate_limit: Some(RateLimitDetails {
                    primary_window: None,
                    secondary_window: None,
                    code_review_window: None,
                    individual_limit: Some(SpendControlLimitSnapshot {
                        limit: Some(100.0),
                        used: None,
                        remaining_percent: Some(60.0),
                        resets_at: None,
                    }),
                }),
                credits: Some(CreditDetails {
                    has_credits: Some(true),
                    unlimited: Some(false),
                    balance: Some(60.0),
                }),
                individual_limit: None,
            })
            .expect("codex result");
        let cost = cost.expect("cost");
        assert_eq!(cost.used, 40.0);
        assert_eq!(cost.limit, Some(100.0));
    }
}
