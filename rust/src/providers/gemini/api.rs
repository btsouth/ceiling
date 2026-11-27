//! Gemini API client for fetching quota information
//!
//! Uses Google Cloud Code Private API with OAuth tokens from ~/.gemini/oauth_creds.json.
//!
//! Ceiling only reads that file. It never refreshes the Gemini CLI session or
//! writes tokens back: the Gemini CLI owns its sign-in, and refreshing would
//! require Ceiling to scrape the CLI's OAuth client secret from its bundle.

use crate::core::{FetchContext, ProviderError, RateWindow};
use chrono::{DateTime, Utc};
use serde::Deserialize;
#[cfg(any(windows, test))]
use std::path::Path;
use std::path::PathBuf;

const QUOTA_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";
const CODE_ASSIST_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";

/// Shown when the Gemini CLI access token has expired or Google rejects it.
const SIGN_IN_EXPIRED_MESSAGE: &str =
    "Gemini CLI sign-in expired. Run `gemini` to refresh your session, then retry.";

/// Gemini API client
pub struct GeminiApi {
    client: reqwest::Client,
    /// The user's home, or `None` on a machine that reports none.
    ///
    /// Kept as an `Option` rather than resolved to a fallback at construction.
    /// A missing home is not configured: probing the working directory would
    /// load a checked-in `.gemini/oauth_creds.json` fixture (SBS-950).
    home_dir: Option<PathBuf>,
    quota_endpoint: String,
    code_assist_endpoint: String,
}

impl GeminiApi {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            home_dir: dirs::home_dir(),
            quota_endpoint: QUOTA_ENDPOINT.to_string(),
            code_assist_endpoint: CODE_ASSIST_ENDPOINT.to_string(),
        }
    }

    #[cfg(test)]
    fn for_test(home_dir: PathBuf, quota: &str, code_assist: &str) -> Self {
        Self::for_test_home(Some(home_dir), quota, code_assist)
    }

    #[cfg(test)]
    fn for_test_home(home_dir: Option<PathBuf>, quota: &str, code_assist: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            home_dir,
            quota_endpoint: quota.to_string(),
            code_assist_endpoint: code_assist.to_string(),
        }
    }

    /// Fetch quota information from the Gemini API
    /// Returns (primary RateWindow, optional model-specific RateWindow, optional email, optional plan)
    /// Note: Gemini quota API requires OAuth tokens, not API keys
    pub async fn fetch_quota(
        &self,
        _ctx: &FetchContext,
    ) -> Result<
        (
            RateWindow,
            Option<RateWindow>,
            Option<String>,
            Option<String>,
        ),
        ProviderError,
    > {
        // Gemini quota endpoint requires OAuth credentials (not API keys).
        // Read-only: ~/.gemini/oauth_creds.json belongs to the Gemini CLI.
        let creds = self.load_credentials()?;

        if creds.is_past_expiry() {
            tracing::debug!("Gemini CLI access token is past its expiry; not refreshing");
            return Err(Self::sign_in_expired());
        }

        match self.fetch_quota_with_creds(&creds).await {
            Err(ProviderError::AuthRequired) => Err(Self::sign_in_expired()),
            other => other,
        }
    }

    async fn fetch_quota_with_creds(
        &self,
        creds: &OAuthCredentials,
    ) -> Result<
        (
            RateWindow,
            Option<RateWindow>,
            Option<String>,
            Option<String>,
        ),
        ProviderError,
    > {
        let access_token = creds
            .access_token
            .clone()
            .ok_or_else(|| ProviderError::AuthRequired)?;

        let code_assist = self.load_code_assist_status(&access_token).await;

        let response = self
            .client
            .post(&self.quota_endpoint)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .body("{}")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        if response.status() == 401 {
            return Err(ProviderError::AuthRequired);
        }

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Gemini API returned {}",
                response.status()
            )));
        }

        let quota_response: QuotaResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))?;

        let (primary, model_specific, email) =
            self.parse_quota_response(quota_response, Some(creds))?;
        let hosted_domain = creds
            .id_token
            .as_deref()
            .and_then(extract_hosted_domain_from_jwt);
        let plan = resolve_account_plan(&code_assist, hosted_domain.as_deref());

        Ok((primary, model_specific, email, plan))
    }

    async fn load_code_assist_status(&self, access_token: &str) -> CodeAssistStatus {
        let response = self
            .client
            .post(&self.code_assist_endpoint)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .body(r#"{"metadata":{"ideType":"GEMINI_CLI","pluginType":"GEMINI"}}"#)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await;

        let response = match response {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                tracing::warn!(status = %response.status(), "Gemini loadCodeAssist request failed");
                return CodeAssistStatus::default();
            }
            Err(error) => {
                tracing::warn!(%error, "Gemini loadCodeAssist request failed");
                return CodeAssistStatus::default();
            }
        };

        match response.text().await {
            Ok(body) => parse_code_assist_status(&body),
            Err(error) => {
                tracing::warn!(%error, "Gemini loadCodeAssist response was invalid");
                CodeAssistStatus::default()
            }
        }
    }

    fn load_credentials(&self) -> Result<OAuthCredentials, ProviderError> {
        let creds_path = self
            .gemini_dir()
            .ok_or_else(Self::not_logged_in)?
            .join("oauth_creds.json");

        if !creds_path.exists() {
            return Err(Self::not_logged_in());
        }

        let content = std::fs::read_to_string(&creds_path).map_err(|e| {
            ProviderError::Other(format!("Failed to read Gemini credentials: {}", e))
        })?;

        serde_json::from_str(&content)
            .map_err(|e| ProviderError::Parse(format!("Invalid Gemini credentials: {}", e)))
    }

    /// The `.gemini` directory under the user's home, if this machine has one.
    ///
    /// No home means no user Gemini config. The working directory is not a
    /// substitute: a service or container started in a checkout that contains
    /// `.gemini/oauth_creds.json` would otherwise read that fixture (SBS-950).
    fn gemini_dir(&self) -> Option<PathBuf> {
        Some(self.home_dir.as_ref()?.join(".gemini"))
    }

    fn not_logged_in() -> ProviderError {
        ProviderError::NotInstalled(
            "Not logged in to Gemini. Run 'gemini' in Terminal to authenticate.".to_string(),
        )
    }

    fn sign_in_expired() -> ProviderError {
        ProviderError::OAuth(SIGN_IN_EXPIRED_MESSAGE.to_string())
    }

    fn parse_quota_response(
        &self,
        response: QuotaResponse,
        creds: Option<&OAuthCredentials>,
    ) -> Result<(RateWindow, Option<RateWindow>, Option<String>), ProviderError> {
        let buckets = response
            .buckets
            .ok_or_else(|| ProviderError::Parse("No quota buckets in response".to_string()))?;

        if buckets.is_empty() {
            return Err(ProviderError::Parse("Empty quota buckets".to_string()));
        }

        // Group quotas by model, keeping lowest per model
        let mut model_quotas: std::collections::HashMap<String, (f64, Option<String>)> =
            std::collections::HashMap::new();

        for bucket in buckets {
            if let (Some(model_id), Some(fraction)) = (bucket.model_id, bucket.remaining_fraction) {
                let entry = model_quotas.entry(model_id).or_insert((1.0, None));
                if fraction < entry.0 {
                    *entry = (fraction, bucket.reset_time);
                }
            }
        }

        // Find Flash and Pro quotas
        let flash_quota = model_quotas
            .iter()
            .filter(|(k, _)| k.to_lowercase().contains("flash"))
            .min_by(|a, b| {
                a.1.0
                    .partial_cmp(&b.1.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

        let pro_quota = model_quotas
            .iter()
            .filter(|(k, _)| k.to_lowercase().contains("pro"))
            .min_by(|a, b| {
                a.1.0
                    .partial_cmp(&b.1.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

        // Build primary RateWindow from the most constrained quota
        let (primary_fraction, primary_reset) = if let Some((_, (frac, reset))) = pro_quota {
            (*frac, reset.clone())
        } else if let Some((_, (frac, reset))) = flash_quota {
            (*frac, reset.clone())
        } else if let Some((_, (frac, reset))) = model_quotas.iter().next() {
            (*frac, reset.clone())
        } else {
            (1.0, None)
        };

        let primary_percent_used = (1.0 - primary_fraction) * 100.0;
        let primary_reset_at = primary_reset.as_ref().and_then(|s| parse_iso_date(s));

        let primary = RateWindow::with_details(
            primary_percent_used,
            Some(1440), // 24 hours
            primary_reset_at,
            None,
        );

        // Model-specific window for Flash if Pro is primary
        let model_specific = if pro_quota.is_some() {
            flash_quota.map(|(_, (frac, reset))| {
                let percent_used = (1.0 - frac) * 100.0;
                let reset_at = reset.as_ref().and_then(|s| parse_iso_date(s));
                RateWindow::with_details(percent_used, Some(1440), reset_at, None)
            })
        } else {
            None
        };

        // Extract email from ID token
        let email = creds
            .and_then(|c| c.id_token.as_ref())
            .and_then(|token| extract_email_from_jwt(token));

        Ok((primary, model_specific, email))
    }
}

impl Default for GeminiApi {
    fn default() -> Self {
        Self::new()
    }
}

// --- Data structures ---

#[derive(Debug, Clone, Deserialize)]
struct OAuthCredentials {
    access_token: Option<String>,
    id_token: Option<String>,
    expiry_date: Option<f64>, // milliseconds since epoch
}

impl OAuthCredentials {
    /// Whether the access token is past its own expiry.
    ///
    /// Ceiling does not refresh, so a token still in date is always tried; a
    /// missing or unparseable expiry is unknown, not dead, and a 401 from
    /// Google still reports the expired sign-in.
    fn is_past_expiry(&self) -> bool {
        self.is_past_expiry_at(chrono::Utc::now())
    }

    fn is_past_expiry_at(&self, now: DateTime<Utc>) -> bool {
        match self.expiry_datetime() {
            Some(expiry) => expiry <= now,
            None => false,
        }
    }

    fn expiry_datetime(&self) -> Option<DateTime<Utc>> {
        let expiry_ms = self.expiry_date?;
        if !expiry_ms.is_finite() {
            return None;
        }
        DateTime::<Utc>::from_timestamp_millis(expiry_ms.round() as i64)
    }
}

#[derive(Debug, Deserialize)]
struct QuotaResponse {
    buckets: Option<Vec<QuotaBucket>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaBucket {
    remaining_fraction: Option<f64>,
    reset_time: Option<String>,
    model_id: Option<String>,
    token_type: Option<String>,
}

#[derive(Default)]
struct CodeAssistStatus {
    tier: Option<GeminiUserTier>,
    paid_tier_name: Option<String>,
}

#[derive(Clone, Copy)]
enum GeminiUserTier {
    Free,
    Legacy,
    Standard,
}

fn parse_code_assist_status(body: &str) -> CodeAssistStatus {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return CodeAssistStatus::default();
    };

    let tier = json
        .get("currentTier")
        .and_then(|tier| tier.get("id"))
        .and_then(serde_json::Value::as_str)
        .and_then(|tier| match tier {
            "free-tier" => Some(GeminiUserTier::Free),
            "legacy-tier" => Some(GeminiUserTier::Legacy),
            "standard-tier" => Some(GeminiUserTier::Standard),
            _ => None,
        });
    let paid_tier_name = json
        .get("paidTier")
        .and_then(|tier| tier.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .map(str::to_owned)
        .filter(|name| !name.is_empty());

    CodeAssistStatus {
        tier,
        paid_tier_name,
    }
}

fn resolve_account_plan(status: &CodeAssistStatus, hosted_domain: Option<&str>) -> Option<String> {
    if let Some(plan) = &status.paid_tier_name {
        return Some(plan.clone());
    }

    match status.tier {
        Some(GeminiUserTier::Standard) => Some("Paid".to_string()),
        Some(GeminiUserTier::Free) if hosted_domain.is_some() => Some("Workspace".to_string()),
        Some(GeminiUserTier::Free) => Some("Free".to_string()),
        Some(GeminiUserTier::Legacy) => Some("Legacy".to_string()),
        None => None,
    }
}

// --- Helper functions ---

fn parse_iso_date(s: &str) -> Option<DateTime<Utc>> {
    // Try with fractional seconds first
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }

    // Try without fractional seconds
    if let Ok(dt) = chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ") {
        return Some(dt.with_timezone(&Utc));
    }

    None
}

fn extract_email_from_jwt(token: &str) -> Option<String> {
    jwt_payload(token)?
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

fn extract_hosted_domain_from_jwt(token: &str) -> Option<String> {
    jwt_payload(token)?
        .get("hd")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|hd| !hd.is_empty())
        .map(str::to_owned)
}

/// Returns `true` when `path` (`~/.gemini/oauth_creds.json`) is either absent
/// or contains a Google OAuth `id_token` for a personal consumer account
/// (non-empty `email` and no Workspace `hd` hosted-domain claim).
///
/// Fails safe to `false` when the file exists with a Workspace `hd` claim,
/// cannot be read or parsed, or has no verifiable `id_token`, so organization
/// Gemini Code Assist accounts are never silently disabled.
#[cfg(any(windows, test))]
pub(crate) fn is_absent_or_personal_gemini_creds(path: &Path) -> bool {
    if !path.exists() {
        return true;
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(creds) = serde_json::from_str::<OAuthCredentials>(&raw) else {
        return false;
    };
    let Some(id_token) = creds.id_token.as_deref() else {
        return false;
    };
    let Some(payload) = jwt_payload(id_token) else {
        return false;
    };
    let has_email = payload
        .get("email")
        .and_then(|v| v.as_str())
        .is_some_and(|e| !e.trim().is_empty());
    let has_hosted_domain = payload
        .get("hd")
        .and_then(|v| v.as_str())
        .is_some_and(|hd| !hd.trim().is_empty());
    has_email && !has_hosted_domain
}

fn jwt_payload(token: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return None;
    }

    // Decode base64url payload
    let mut payload = parts[1].replace('-', "+").replace('_', "/");

    // Add padding if needed
    let remainder = payload.len() % 4;
    if remainder > 0 {
        payload.push_str(&"=".repeat(4 - remainder));
    }

    let decoded =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &payload).ok()?;

    serde_json::from_slice(&decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn test_id_token(sub: &str, email: &str) -> String {
        let payload = serde_json::json!({ "sub": sub, "email": email });
        let encoded = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            serde_json::to_vec(&payload).expect("encode jwt payload"),
        );
        format!("eyJhbGciOiJub25lIn0.{encoded}.sig")
    }

    #[test]
    fn paid_tier_name_overrides_generic_tier_fallbacks() {
        let status = parse_code_assist_status(
            r#"{
                "currentTier": { "id": "free-tier" },
                "paidTier": { "name": "Gemini Code Assist in Google One AI Pro" }
            }"#,
        );

        assert_eq!(
            resolve_account_plan(&status, Some("example.com")),
            Some("Gemini Code Assist in Google One AI Pro".to_string())
        );

        let standard = parse_code_assist_status(
            r#"{
                "currentTier": { "id": "standard-tier" },
                "paidTier": { "name": "Plus" }
            }"#,
        );

        assert_eq!(
            resolve_account_plan(&standard, None),
            Some("Plus".to_string())
        );
    }

    #[test]
    fn generic_tier_fallbacks_remain_when_paid_tier_is_absent() {
        let free_tier = parse_code_assist_status(r#"{"currentTier":{"id":"free-tier"}}"#);
        let paid = parse_code_assist_status(r#"{"currentTier":{"id":"standard-tier"}}"#);

        assert_eq!(
            resolve_account_plan(&free_tier, Some("example.com")),
            Some("Workspace".to_string())
        );
        assert_eq!(
            resolve_account_plan(&free_tier, None),
            Some("Free".to_string())
        );
        assert_eq!(resolve_account_plan(&paid, None), Some("Paid".to_string()));
    }

    #[test]
    fn invalid_code_assist_response_does_not_create_a_generic_plan() {
        let status = parse_code_assist_status("not json");

        assert_eq!(resolve_account_plan(&status, Some("example.com")), None);
    }

    #[test]
    fn malformed_paid_tier_preserves_current_tier_fallback() {
        let status =
            parse_code_assist_status(r#"{"currentTier":{"id":"free-tier"},"paidTier":[]}"#);

        assert_eq!(
            resolve_account_plan(&status, Some("example.com")),
            Some("Workspace".to_string())
        );
    }

    /// Pins SBS-950: a missing home is not the working directory. Returning
    /// `Some("./.gemini")` here is what loaded a checkout fixture.
    #[test]
    fn gemini_dir_is_absent_when_there_is_no_home() {
        assert_eq!(api_without_home().gemini_dir(), None);
    }

    #[test]
    fn gemini_dir_joins_the_home_when_one_is_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let api = GeminiApi::for_test(
            dir.path().to_path_buf(),
            "http://127.0.0.1/quota",
            "http://127.0.0.1/codeassist",
        );
        assert_eq!(api.gemini_dir(), Some(dir.path().join(".gemini")));
    }

    /// Pins SBS-950: no home is NotInstalled before any credentials path
    /// is joined, so cwd is never probed for `oauth_creds.json`.
    ///
    /// The same fixture is loaded through a real home first. Without that,
    /// the refusal below would also hold for a build that still probed the
    /// working directory and simply found nothing there.
    #[test]
    fn missing_home_does_not_load_credentials() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_gemini_home(dir.path(), "live-access", 1_800_000_000_000.0);

        let with_home = GeminiApi::for_test(
            dir.path().to_path_buf(),
            "http://127.0.0.1/quota",
            "http://127.0.0.1/codeassist",
        );
        assert_eq!(
            with_home
                .load_credentials()
                .expect("a real home must load the fixture")
                .access_token
                .as_deref(),
            Some("live-access"),
            "the fixture must be loadable, or the refusal below proves nothing"
        );

        let error = api_without_home()
            .load_credentials()
            .expect_err("no home is not logged in");
        assert!(matches!(error, ProviderError::NotInstalled(_)));
    }

    fn quota_ok_body() -> &'static str {
        r#"{
            "buckets": [
                {
                    "remainingFraction": 0.8,
                    "resetTime": "2026-08-18T00:00:00Z",
                    "modelId": "gemini-2.5-pro"
                }
            ]
        }"#
    }

    async fn mock_code_assist(server: &mut mockito::Server) -> mockito::Mock {
        server
            .mock("POST", "/codeassist")
            .with_status(200)
            .with_body(r#"{"currentTier":{"id":"standard-tier"}}"#)
            .expect_at_least(0)
            .create_async()
            .await
    }

    /// Pins SBS-950: the quota poll is NotInstalled when there is no home,
    /// so it never reaches Google.
    #[tokio::test]
    async fn fetch_quota_without_home_is_not_logged_in() {
        let error = api_without_home()
            .fetch_quota(&FetchContext::default())
            .await
            .expect_err("no home is not logged in");
        assert!(matches!(error, ProviderError::NotInstalled(_)));
    }

    #[test]
    fn personal_gemini_creds_check_migrates_missing_and_consumer_but_preserves_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let creds_path = dir.path().join("oauth_creds.json");

        // Missing file -> eligible for migration
        assert!(is_absent_or_personal_gemini_creds(&creds_path));

        // Consumer account (email, no hd claim) -> eligible for migration
        let consumer_jwt = test_id_token("sub-1", "user@gmail.com");
        std::fs::write(
            &creds_path,
            serde_json::json!({ "id_token": consumer_jwt }).to_string(),
        )
        .unwrap();
        assert!(is_absent_or_personal_gemini_creds(&creds_path));

        // Workspace / Enterprise account (hd claim present) -> fail-safe: do NOT migrate
        let payload = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            r#"{"sub":"sub-2","email":"dev@company.com","hd":"company.com"}"#,
        );
        let workspace_jwt = format!("eyJhbGciOiJSUzI1NiJ9.{payload}.sig");
        std::fs::write(
            &creds_path,
            serde_json::json!({ "id_token": workspace_jwt }).to_string(),
        )
        .unwrap();
        assert!(!is_absent_or_personal_gemini_creds(&creds_path));

        // Missing id_token or malformed JSON -> fail-safe: do NOT migrate
        std::fs::write(&creds_path, r#"{"access_token":"tok"}"#).unwrap();
        assert!(!is_absent_or_personal_gemini_creds(&creds_path));
        std::fs::write(&creds_path, "not json").unwrap();
        assert!(!is_absent_or_personal_gemini_creds(&creds_path));
    }

    fn api_without_home() -> GeminiApi {
        GeminiApi::for_test_home(
            None,
            "http://127.0.0.1/quota",
            "http://127.0.0.1/codeassist",
        )
    }

    fn write_gemini_home(dir: &Path, access: &str, expiry_ms: f64) -> PathBuf {
        let gemini = dir.join(".gemini");
        std::fs::create_dir_all(&gemini).expect("gemini dir");
        let creds = serde_json::json!({
            "access_token": access,
            "id_token": test_id_token("user-1", "user@example.com"),
            "refresh_token": "cli-owned-refresh",
            "expiry_date": expiry_ms,
        });
        let path = gemini.join("oauth_creds.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&creds).expect("creds bytes"),
        )
        .expect("write creds");
        path
    }

    fn api_for_server(home: &Path, server: &mockito::Server) -> GeminiApi {
        GeminiApi::for_test(
            home.to_path_buf(),
            &format!("{}/quota", server.url()),
            &format!("{}/codeassist", server.url()),
        )
    }

    fn assert_sign_in_expired(error: &ProviderError) {
        match error {
            ProviderError::OAuth(message) => {
                assert_eq!(message, SIGN_IN_EXPIRED_MESSAGE);
                assert!(message.contains("`gemini`"));
            }
            other => panic!("expected an expired Gemini sign-in, got {other:?}"),
        }
    }

    #[test]
    fn expiry_is_the_token_expiry_without_refresh_skew() {
        let now = Utc::now();
        let creds = |expiry: Option<f64>| OAuthCredentials {
            access_token: Some("access".to_string()),
            id_token: None,
            expiry_date: expiry,
        };
        let at = |time: DateTime<Utc>| Some(time.timestamp_millis() as f64);

        assert!(creds(at(now)).is_past_expiry_at(now));
        assert!(creds(at(now - chrono::Duration::seconds(1))).is_past_expiry_at(now));
        assert!(!creds(at(now + chrono::Duration::seconds(30))).is_past_expiry_at(now));
        assert!(
            !creds(None).is_past_expiry_at(now),
            "missing expiry is unknown, not expired"
        );
        assert!(
            !creds(Some(f64::NAN)).is_past_expiry_at(now),
            "non-finite expiry is unknown, not expired"
        );
    }

    #[test]
    fn credentials_parse_with_cli_only_fields_present() {
        let creds: OAuthCredentials = serde_json::from_str(
            r#"{"access_token":"a","refresh_token":"r","token_type":"Bearer","scope":"x","expiry_date":1}"#,
        )
        .expect("Gemini CLI credential files carry extra fields");
        assert_eq!(creds.access_token.as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn valid_token_polls_without_touching_the_credentials_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expiry_ms = (Utc::now() + chrono::Duration::seconds(30)).timestamp_millis() as f64;
        let path = write_gemini_home(dir.path(), "live-access", expiry_ms);
        let original = std::fs::read(&path).expect("seed bytes");

        let mut server = mockito::Server::new_async().await;
        let _code_assist = mock_code_assist(&mut server).await;
        let quota = server
            .mock("POST", "/quota")
            .match_header("authorization", "Bearer live-access")
            .with_status(200)
            .with_body(quota_ok_body())
            .expect(1)
            .create_async()
            .await;

        let api = api_for_server(dir.path(), &server);
        let (primary, _, email, plan) = api
            .fetch_quota(&FetchContext::default())
            .await
            .expect("an in-date token polls");

        quota.assert_async().await;
        assert!((primary.used_percent - 20.0).abs() < 0.01);
        assert_eq!(email.as_deref(), Some("user@example.com"));
        assert_eq!(plan.as_deref(), Some("Paid"));
        assert_eq!(std::fs::read(&path).expect("read back"), original);
    }

    #[tokio::test]
    async fn expired_token_reports_sign_in_expired_without_network() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expiry_ms = (Utc::now() - chrono::Duration::minutes(1)).timestamp_millis() as f64;
        let path = write_gemini_home(dir.path(), "dead-access", expiry_ms);
        let original = std::fs::read(&path).expect("seed bytes");

        let mut server = mockito::Server::new_async().await;
        let code_assist = server
            .mock("POST", "/codeassist")
            .expect(0)
            .create_async()
            .await;
        let quota = server.mock("POST", "/quota").expect(0).create_async().await;

        let api = api_for_server(dir.path(), &server);
        let error = api
            .fetch_quota(&FetchContext::default())
            .await
            .expect_err("an expired Gemini CLI token is not refreshed");

        assert_sign_in_expired(&error);
        code_assist.assert_async().await;
        quota.assert_async().await;
        assert_eq!(std::fs::read(&path).expect("read back"), original);
    }

    #[tokio::test]
    async fn quota_401_reports_sign_in_expired_and_leaves_file_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expiry_ms = (Utc::now() + chrono::Duration::hours(1)).timestamp_millis() as f64;
        let path = write_gemini_home(dir.path(), "revoked-access", expiry_ms);
        let original = std::fs::read(&path).expect("seed bytes");

        let mut server = mockito::Server::new_async().await;
        let _code_assist = mock_code_assist(&mut server).await;
        let quota = server
            .mock("POST", "/quota")
            .match_header("authorization", "Bearer revoked-access")
            .with_status(401)
            .expect(1)
            .create_async()
            .await;

        let api = api_for_server(dir.path(), &server);
        let error = api
            .fetch_quota(&FetchContext::default())
            .await
            .expect_err("a rejected token is an expired sign-in");

        quota.assert_async().await;
        assert_sign_in_expired(&error);
        assert_eq!(std::fs::read(&path).expect("read back"), original);
    }
}
