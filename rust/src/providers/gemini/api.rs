//! Gemini API client for fetching quota information
//!
//! Uses Google Cloud Code Private API with OAuth tokens from ~/.gemini/oauth_creds.json

use crate::core::{FetchContext, ProviderError, RateWindow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const QUOTA_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";
const CODE_ASSIST_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const TOKEN_REFRESH_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// Refresh this far before expiry so a 5-minute auto-refresh never presents
/// Google with a token that dies at the exact expiry second (SBS-928).
const ACCESS_TOKEN_REFRESH_SKEW: chrono::TimeDelta = chrono::Duration::minutes(5);

/// Gemini API client
pub struct GeminiApi {
    client: reqwest::Client,
    /// The user's home, or `None` on a machine that reports none.
    ///
    /// Kept as an `Option` rather than resolved to a fallback at construction.
    /// A missing home is not configured: probing the working directory would
    /// load a checked-in `.gemini/oauth_creds.json` fixture and write live
    /// access tokens back into it (SBS-950). `client_config.json` has the same
    /// rule for the same reason.
    home_dir: Option<PathBuf>,
    quota_endpoint: String,
    code_assist_endpoint: String,
    token_refresh_endpoint: String,
}

impl GeminiApi {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            home_dir: dirs::home_dir(),
            quota_endpoint: QUOTA_ENDPOINT.to_string(),
            code_assist_endpoint: CODE_ASSIST_ENDPOINT.to_string(),
            token_refresh_endpoint: TOKEN_REFRESH_ENDPOINT.to_string(),
        }
    }

    #[cfg(test)]
    fn for_test(home_dir: PathBuf, quota: &str, code_assist: &str, token_refresh: &str) -> Self {
        Self::for_test_home(Some(home_dir), quota, code_assist, token_refresh)
    }

    #[cfg(test)]
    fn for_test_home(
        home_dir: Option<PathBuf>,
        quota: &str,
        code_assist: &str,
        token_refresh: &str,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            home_dir,
            quota_endpoint: quota.to_string(),
            code_assist_endpoint: code_assist.to_string(),
            token_refresh_endpoint: token_refresh.to_string(),
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
        // Gemini quota endpoint requires OAuth credentials (not API keys)
        // Always load OAuth credentials from ~/.gemini/oauth_creds.json
        let mut creds = self.load_credentials()?;
        let mut refreshed_this_poll = false;

        // Refresh early (skew) so a still-refreshable seat is not sent to Google
        // in the last minutes of the access token (SBS-928).
        //
        // A refresh that cannot be done, or that fails, must not cost the poll a
        // token Google would still accept: without a refresh token there is
        // nothing to refresh, and a token endpoint that times out says nothing
        // about the access token in hand. Only a token that is genuinely past
        // its expiry has nothing left to try.
        if creds.is_expired() && creds.has_refresh_token() {
            tracing::debug!("Gemini token expired or within refresh skew, refreshing...");
            match self.refresh_token(&creds).await {
                Ok(refreshed) => {
                    creds = refreshed;
                    refreshed_this_poll = true;
                }
                Err(error) if !creds.is_past_expiry() => {
                    tracing::debug!(
                        "Gemini early refresh failed ({error}); using the access token that is still in date"
                    );
                }
                Err(error) => return Err(error),
            }
        }

        match self.fetch_quota_with_creds(&creds).await {
            Ok(result) => Ok(result),
            Err(ProviderError::AuthRequired)
                if !refreshed_this_poll && creds.has_refresh_token() =>
            {
                // 401 is not AuthRequired when a refresh token still exists.
                // A revoked refresh token fails inside refresh_token().
                tracing::debug!("Gemini quota returned 401; refreshing once and retrying");
                creds = self.refresh_token(&creds).await?;
                self.fetch_quota_with_creds(&creds).await
            }
            Err(error) => Err(error),
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

    async fn refresh_token(
        &self,
        creds: &OAuthCredentials,
    ) -> Result<OAuthCredentials, ProviderError> {
        let refresh_token = creds
            .refresh_token
            .as_ref()
            .ok_or_else(|| ProviderError::AuthRequired)?;

        // Get OAuth client credentials from Gemini CLI
        let client_creds = self.extract_oauth_client_credentials()?;

        let params = [
            ("client_id", client_creds.client_id.as_str()),
            ("client_secret", client_creds.client_secret.as_str()),
            ("refresh_token", refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ];

        let response = self
            .client
            .post(&self.token_refresh_endpoint)
            .form(&params)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::AuthRequired);
        }

        let refresh_response: TokenRefreshResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))?;

        // Update stored credentials
        let mut new_creds = creds.clone();
        new_creds.access_token = Some(refresh_response.access_token.clone());
        if let Some(id_token) = &refresh_response.id_token {
            new_creds.id_token = Some(id_token.clone());
        }
        if let Some(expires_in) = refresh_response.expires_in {
            let expiry_ms = (chrono::Utc::now().timestamp() as f64 + expires_in) * 1000.0;
            new_creds.expiry_date = Some(expiry_ms);
        }

        // Persist after Google has already issued tokens. A locked or missing
        // file must not drop the in-memory refresh.
        if let Err(error) = self.save_credentials(&new_creds) {
            tracing::warn!(
                "Gemini token refreshed but could not persist oauth_creds.json: {error}"
            );
        }

        tracing::info!("Gemini token refreshed successfully");
        Ok(new_creds)
    }

    fn save_credentials(&self, creds: &OAuthCredentials) -> Result<(), ProviderError> {
        let creds_path = self
            .gemini_dir()
            .ok_or_else(Self::not_logged_in)?
            .join("oauth_creds.json");
        persist_refreshed_credentials(&creds_path, creds)
    }

    fn extract_oauth_client_credentials(&self) -> Result<OAuthClientCredentials, ProviderError> {
        self.user_client_config_credentials()
            .or_else(|| self.gemini_binary_oauth_credentials())
            .or_else(Self::platform_oauth_credentials)
            .or_else(Self::fnm_oauth_credentials)
            .map(Ok)
            .unwrap_or_else(Self::oauth_credentials_from_env)
    }

    /// The `.gemini` directory under the user's home, if this machine has one.
    ///
    /// No home means no user Gemini config. The working directory is not a
    /// substitute: a service or container started in a checkout that contains
    /// `.gemini/oauth_creds.json` would otherwise refresh against that fixture
    /// and write live tokens back into the tree (SBS-950).
    fn gemini_dir(&self) -> Option<PathBuf> {
        Some(self.home_dir.as_ref()?.join(".gemini"))
    }

    fn not_logged_in() -> ProviderError {
        ProviderError::NotInstalled(
            "Not logged in to Gemini. Run 'gemini' in Terminal to authenticate.".to_string(),
        )
    }

    fn user_client_config_credentials(&self) -> Option<OAuthClientCredentials> {
        Self::try_read_client_config(&self.gemini_dir()?.join("client_config.json"))
    }

    fn gemini_binary_oauth_credentials(&self) -> Option<OAuthClientCredentials> {
        let gemini_path = which::which("gemini").ok()?;
        let resolved = std::fs::canonicalize(&gemini_path).unwrap_or(gemini_path);
        let base_dir = resolved.parent()?;

        // Current Gemini CLI releases ship as a bundled npm package instead of
        // the older standalone gemini-cli-core tree. Search the largest bundle
        // chunks first; the OAuth constants live in a platform chunk and this
        // path is only needed when the stored access token expires.
        if let Some(creds) = Self::try_extract_oauth_from_bundle(
            &base_dir
                .join("node_modules")
                .join("@google")
                .join("gemini-cli")
                .join("bundle"),
        ) {
            return Some(creds);
        }

        Self::oauth_credentials_from_candidates(Self::binary_oauth_candidates(base_dir))
    }

    fn try_extract_oauth_from_bundle(bundle_dir: &Path) -> Option<OAuthClientCredentials> {
        let mut candidates = std::fs::read_dir(bundle_dir)
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("js") {
                    return None;
                }
                Some((entry.metadata().ok()?.len(), path))
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|candidate| std::cmp::Reverse(candidate.0));
        candidates
            .into_iter()
            .find_map(|(_, path)| Self::try_extract_oauth_from_js(&path))
    }

    fn oauth_credentials_from_candidates<I>(candidates: I) -> Option<OAuthClientCredentials>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        candidates
            .into_iter()
            .find_map(|candidate| Self::try_extract_oauth_from_js(&candidate))
    }

    fn binary_oauth_candidates(base_dir: &Path) -> Vec<PathBuf> {
        let oauth_subpath = Self::oauth_subpath();
        vec![
            // npm global: {bin}/../node_modules/@google/gemini-cli-core/...
            base_dir
                .join("..")
                .join("node_modules")
                .join(&oauth_subpath),
            // Homebrew: {bin}/../libexec/lib/node_modules/@google/gemini-cli/node_modules/...
            base_dir
                .join("..")
                .join("libexec")
                .join("lib")
                .join("node_modules")
                .join("@google")
                .join("gemini-cli")
                .join("node_modules")
                .join(&oauth_subpath),
            // Nix: {bin}/../share/gemini-cli/node_modules/...
            base_dir
                .join("..")
                .join("share")
                .join("gemini-cli")
                .join("node_modules")
                .join(&oauth_subpath),
            // Bun sibling
            base_dir
                .join("..")
                .join("gemini-cli-core")
                .join("dist")
                .join("src")
                .join("code_assist")
                .join("oauth2.js"),
        ]
    }

    fn oauth_subpath() -> PathBuf {
        Path::new("@google")
            .join("gemini-cli-core")
            .join("dist")
            .join("src")
            .join("code_assist")
            .join("oauth2.js")
    }

    #[cfg(windows)]
    fn platform_oauth_credentials() -> Option<OAuthClientCredentials> {
        #[cfg(windows)]
        if let Some(appdata) = dirs::data_dir() {
            let bundle = appdata
                .join("npm")
                .join("node_modules")
                .join("@google")
                .join("gemini-cli")
                .join("bundle");
            if let Some(creds) = Self::try_extract_oauth_from_bundle(&bundle) {
                return Some(creds);
            }
            let npm_path = appdata
                .join("npm")
                .join("node_modules")
                .join("@google")
                .join("gemini-cli-core")
                .join("dist")
                .join("src")
                .join("code_assist")
                .join("oauth2.js");
            if let Some(creds) = Self::try_extract_oauth_from_js(&npm_path) {
                return Some(creds);
            }
        }

        None
    }

    #[cfg(not(windows))]
    fn platform_oauth_credentials() -> Option<OAuthClientCredentials> {
        None
    }

    #[cfg(windows)]
    fn fnm_oauth_credentials() -> Option<OAuthClientCredentials> {
        #[cfg(windows)]
        if let Some(local_appdata) = dirs::data_local_dir() {
            let fnm_versions = local_appdata.join("fnm").join("node-versions");
            return Self::fnm_oauth_credentials_from(&fnm_versions);
        }

        None
    }

    #[cfg(not(windows))]
    fn fnm_oauth_credentials() -> Option<OAuthClientCredentials> {
        #[cfg(not(windows))]
        if let Some(data_dir) = dirs::data_dir() {
            let fnm_versions = data_dir.join("fnm").join("node-versions");
            return Self::fnm_oauth_credentials_from(&fnm_versions);
        }

        None
    }

    fn fnm_oauth_credentials_from(fnm_versions: &Path) -> Option<OAuthClientCredentials> {
        if !fnm_versions.is_dir() {
            return None;
        }

        let entries = std::fs::read_dir(fnm_versions).ok()?;
        let candidates = entries
            .flatten()
            .map(|entry| {
                entry
                    .path()
                    .join("installation")
                    .join("lib")
                    .join("node_modules")
            })
            .map(|node_modules| node_modules.join(Self::oauth_subpath()));

        Self::oauth_credentials_from_candidates(candidates)
    }

    fn oauth_credentials_from_env() -> Result<OAuthClientCredentials, ProviderError> {
        let client_id = std::env::var("GEMINI_CLIENT_ID")
            .map_err(|_| ProviderError::NotInstalled("GEMINI_CLIENT_ID not set. Install Gemini CLI or set GEMINI_CLIENT_ID/GEMINI_CLIENT_SECRET.".to_string()))?;
        let client_secret = std::env::var("GEMINI_CLIENT_SECRET")
            .map_err(|_| ProviderError::NotInstalled("GEMINI_CLIENT_SECRET not set".to_string()))?;

        Ok(OAuthClientCredentials {
            client_id,
            client_secret,
        })
    }

    fn try_read_client_config(path: &std::path::Path) -> Option<OAuthClientCredentials> {
        let content = std::fs::read_to_string(path).ok()?;
        let config: serde_json::Value = serde_json::from_str(&content).ok()?;
        let id = config.get("client_id")?.as_str()?;
        let secret = config.get("client_secret")?.as_str()?;
        Some(OAuthClientCredentials {
            client_id: id.to_string(),
            client_secret: secret.to_string(),
        })
    }

    fn try_extract_oauth_from_js(path: &std::path::Path) -> Option<OAuthClientCredentials> {
        let content = std::fs::read_to_string(path).ok()?;
        let id_re = regex_lite::Regex::new(r#"OAUTH_CLIENT_ID\s*=\s*['"](.*?)['"]"#).ok()?;
        let secret_re =
            regex_lite::Regex::new(r#"OAUTH_CLIENT_SECRET\s*=\s*['"](.*?)['"]"#).ok()?;
        let id = id_re.captures(&content)?.get(1)?.as_str().to_string();
        let secret = secret_re.captures(&content)?.get(1)?.as_str().to_string();
        if id.is_empty() || secret.is_empty() {
            return None;
        }
        Some(OAuthClientCredentials {
            client_id: id,
            client_secret: secret,
        })
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OAuthCredentials {
    access_token: Option<String>,
    id_token: Option<String>,
    refresh_token: Option<String>,
    expiry_date: Option<f64>, // milliseconds since epoch
}

impl OAuthCredentials {
    fn is_expired(&self) -> bool {
        self.is_expired_at(chrono::Utc::now())
    }

    fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        match self.expiry_datetime() {
            Some(expiry) => expiry <= now + ACCESS_TOKEN_REFRESH_SKEW,
            // Missing or unparseable expiry is unknown, not "still valid".
            // Try the access token; a 401 still refresh-retries when we can.
            None => false,
        }
    }

    /// Whether the access token is past its own expiry, skew aside.
    ///
    /// [`is_expired`](Self::is_expired) answers "should this be refreshed now",
    /// which is deliberately early. This answers "is this token dead", which is
    /// what decides whether a failed refresh leaves anything worth sending.
    fn is_past_expiry(&self) -> bool {
        self.is_past_expiry_at(chrono::Utc::now())
    }

    fn is_past_expiry_at(&self, now: DateTime<Utc>) -> bool {
        match self.expiry_datetime() {
            Some(expiry) => expiry <= now,
            // An unknown expiry is not a dead token.
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

    fn has_refresh_token(&self) -> bool {
        self.refresh_token
            .as_deref()
            .is_some_and(|token| !token.trim().is_empty())
    }
}

fn persist_refreshed_credentials(
    path: &Path,
    credentials: &OAuthCredentials,
) -> Result<(), ProviderError> {
    crate::secure_file::with_file_write_lock(path, || {
        let mut root = match std::fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(value) => value,
                // Gemini CLI still does a non-atomic write. A torn file is CLI
                // state, not a user setting; rebuild from the refresh we hold.
                Err(_) => serde_json::json!({}),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
            Err(error) => return Err(error),
        };
        if credential_identities_conflict(&root, credentials) {
            return Ok(());
        }
        apply_refresh_to_credentials_json(&mut root, credentials)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let encoded = serde_json::to_vec_pretty(&root).map_err(std::io::Error::other)?;
        crate::secure_file::atomic_write_preserving_permissions(path, &encoded)
    })
    .map_err(|error| ProviderError::Other(format!("Failed to update Gemini credentials: {error}")))
}

fn apply_refresh_to_credentials_json(
    root: &mut serde_json::Value,
    credentials: &OAuthCredentials,
) -> Result<(), String> {
    let object = root
        .as_object_mut()
        .ok_or_else(|| "Gemini credentials root is not an object".to_string())?;
    let access_token = credentials
        .access_token
        .as_ref()
        .ok_or_else(|| "refreshed Gemini access token is missing".to_string())?;
    object.insert(
        "access_token".to_string(),
        serde_json::Value::String(access_token.clone()),
    );
    if let Some(id_token) = &credentials.id_token {
        object.insert(
            "id_token".to_string(),
            serde_json::Value::String(id_token.clone()),
        );
    }
    if let Some(expiry_date) = credentials.expiry_date {
        let expiry = serde_json::Number::from_f64(expiry_date)
            .ok_or_else(|| "refreshed Gemini expiry is not finite".to_string())?;
        object.insert("expiry_date".to_string(), serde_json::Value::Number(expiry));
    }
    if usable_refresh_token(object.get("refresh_token")).is_none()
        && let Some(refresh_token) = &credentials.refresh_token
    {
        object.insert(
            "refresh_token".to_string(),
            serde_json::Value::String(refresh_token.clone()),
        );
    }
    Ok(())
}

fn usable_refresh_token(value: Option<&serde_json::Value>) -> Option<&str> {
    value
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

fn credential_identities_conflict(
    root: &serde_json::Value,
    credentials: &OAuthCredentials,
) -> bool {
    let disk_token = root.get("id_token").and_then(serde_json::Value::as_str);
    let memory_token = credentials.id_token.as_deref();
    let (Some(disk), Some(memory)) = (disk_token, memory_token) else {
        return false;
    };
    // Compare the stable JWT subject/email. When exactly one side carries a JWT
    // identity the accounts cannot be shown to match, so treat that as a
    // conflict rather than silently mixing accounts. Two opaque (non-JWT)
    // tokens carry no identity, so an ordinary token change is not a conflict.
    let disk_identity = jwt_identity(disk);
    let memory_identity = jwt_identity(memory);
    let conflict = match (disk_identity, memory_identity) {
        (Some(disk), Some(memory)) => disk != memory,
        (Some(_), None) | (None, Some(_)) => true,
        (None, None) => false,
    };
    if conflict {
        tracing::warn!(
            "skipping Gemini credential persist: on-disk account identity differs from the refreshed account"
        );
    }
    conflict
}

fn jwt_identity(token: &str) -> Option<String> {
    let payload = jwt_payload(token)?;
    jwt_text_claim(&payload, "sub").or_else(|| jwt_text_claim(&payload, "email"))
}

fn jwt_text_claim(payload: &serde_json::Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[derive(Debug)]
struct OAuthClientCredentials {
    client_id: String,
    client_secret: String,
}

#[derive(Debug, Deserialize)]
struct TokenRefreshResponse {
    access_token: String,
    id_token: Option<String>,
    expires_in: Option<f64>,
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

    fn refreshed_credentials(access_token: &str) -> OAuthCredentials {
        OAuthCredentials {
            access_token: Some(access_token.to_string()),
            id_token: Some(format!("id-{access_token}")),
            refresh_token: Some("stale-struct-refresh".to_string()),
            expiry_date: Some(1_800_000_000_000.0),
        }
    }

    fn test_id_token(sub: &str, email: &str) -> String {
        let payload = serde_json::json!({ "sub": sub, "email": email });
        let encoded = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            serde_json::to_vec(&payload).expect("encode jwt payload"),
        );
        format!("eyJhbGciOiJub25lIn0.{encoded}.sig")
    }

    #[test]
    fn refresh_persistence_preserves_unknown_and_unmodified_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth_creds.json");
        std::fs::write(
            &path,
            r#"{
                "access_token": "old-access",
                "id_token": "old-id",
                "refresh_token": "newer-disk-refresh",
                "expiry_date": 1,
                "token_type": "Bearer",
                "future_cli_state": {"keep": true}
            }"#,
        )
        .expect("seed credentials");

        persist_refreshed_credentials(&path, &refreshed_credentials("new-access"))
            .expect("persist refresh");

        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read result"))
                .expect("valid result");
        assert_eq!(stored["access_token"], "new-access");
        assert_eq!(stored["id_token"], "id-new-access");
        assert_eq!(stored["refresh_token"], "newer-disk-refresh");
        assert_eq!(stored["token_type"], "Bearer");
        assert_eq!(stored["future_cli_state"]["keep"], true);
    }

    #[test]
    fn concurrent_refresh_persistence_keeps_valid_complete_json() {
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth_creds.json");
        std::fs::write(
            &path,
            r#"{"access_token":"old","refresh_token":"keep","unknown":"keep"}"#,
        )
        .expect("seed credentials");
        let barrier = Arc::new(Barrier::new(3));
        let writers: Vec<_> = ["refresh-a", "refresh-b"]
            .into_iter()
            .map(|access_token| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    persist_refreshed_credentials(&path, &refreshed_credentials(access_token))
                        .expect("persist concurrent refresh");
                })
            })
            .collect();

        barrier.wait();
        for writer in writers {
            writer.join().expect("writer thread");
        }

        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read result"))
                .expect("valid result");
        assert!(matches!(
            stored["access_token"].as_str(),
            Some("refresh-a" | "refresh-b")
        ));
        assert_eq!(stored["refresh_token"], "keep");
        assert_eq!(stored["unknown"], "keep");
    }

    #[test]
    fn failed_refresh_persistence_leaves_original_file_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth_creds.json");
        std::fs::write(&path, b"{not valid json").expect("seed invalid credentials");

        persist_refreshed_credentials(&path, &refreshed_credentials("new-access"))
            .expect("rebuild torn Gemini credentials");

        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read result"))
                .expect("valid result");
        assert_eq!(stored["access_token"], "new-access");
        assert_eq!(stored["refresh_token"], "stale-struct-refresh");
    }

    #[test]
    fn persist_skips_when_disk_id_token_belongs_to_another_account() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth_creds.json");
        let account_b = test_id_token("account-b", "b@example.com");
        let original = serde_json::json!({
            "access_token": "b-access",
            "id_token": account_b,
            "refresh_token": "b-refresh"
        });
        let original_bytes = serde_json::to_vec_pretty(&original).expect("seed bytes");
        std::fs::write(&path, &original_bytes).expect("seed other-account credentials");

        let mut credentials = refreshed_credentials("a-access");
        credentials.id_token = Some(test_id_token("account-a", "a@example.com"));
        persist_refreshed_credentials(&path, &credentials).expect("skip mixed-account persist");

        assert_eq!(std::fs::read(&path).expect("read original"), original_bytes);
    }

    #[test]
    fn persist_skips_when_disk_id_token_is_not_a_jwt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth_creds.json");
        let original = serde_json::json!({
            "access_token": "b-access",
            "id_token": "opaque-non-jwt-b-token",
            "refresh_token": "b-refresh"
        });
        let original_bytes = serde_json::to_vec_pretty(&original).expect("seed bytes");
        std::fs::write(&path, &original_bytes).expect("seed other-account credentials");

        let mut credentials = refreshed_credentials("a-access");
        credentials.id_token = Some(test_id_token("account-a", "a@example.com"));
        persist_refreshed_credentials(&path, &credentials).expect("skip mixed-account persist");

        assert_eq!(std::fs::read(&path).expect("read original"), original_bytes);
    }

    #[test]
    fn persist_repairs_null_or_empty_refresh_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth_creds.json");

        for seed in [
            r#"{"access_token":"old","refresh_token":null}"#,
            r#"{"access_token":"old","refresh_token":""}"#,
            r#"{"access_token":"old","refresh_token":"   "}"#,
        ] {
            std::fs::write(&path, seed).expect("seed unusable refresh token");
            persist_refreshed_credentials(&path, &refreshed_credentials("new-access"))
                .expect("repair refresh token");
            let stored: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).expect("read result"))
                    .expect("valid result");
            assert_eq!(stored["access_token"], "new-access");
            assert_eq!(stored["refresh_token"], "stale-struct-refresh");
        }
    }

    #[test]
    fn persist_creates_credentials_file_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("oauth_creds.json");

        persist_refreshed_credentials(&path, &refreshed_credentials("new-access"))
            .expect("persist refresh into a missing file");

        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read result"))
                .expect("valid result");
        assert_eq!(stored["access_token"], "new-access");
        assert_eq!(stored["id_token"], "id-new-access");
        assert_eq!(stored["refresh_token"], "stale-struct-refresh");
    }

    #[test]
    fn extracts_oauth_constants_from_current_cli_bundle() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gemini.js"), "console.log('entry');").unwrap();
        std::fs::write(
            dir.path().join("chunk-platform.js"),
            r#"
                var OAUTH_CLIENT_ID = "bundle-client-id";
                var OAUTH_CLIENT_SECRET = "bundle-client-secret";
            "#,
        )
        .unwrap();

        let credentials = GeminiApi::try_extract_oauth_from_bundle(dir.path()).unwrap();
        assert_eq!(credentials.client_id, "bundle-client-id");
        assert_eq!(credentials.client_secret, "bundle-client-secret");
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

    fn api_without_home() -> GeminiApi {
        GeminiApi::for_test_home(
            None,
            "http://127.0.0.1/quota",
            "http://127.0.0.1/codeassist",
            "http://127.0.0.1/token",
        )
    }

    /// Pins SBS-950: a missing home is not the working directory. Returning
    /// `Some("./.gemini")` here is what loaded a checkout fixture and wrote
    /// live Google tokens back into it.
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
            "http://127.0.0.1/token",
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
        write_gemini_home(
            dir.path(),
            "live-access",
            Some("live-refresh"),
            1_800_000_000_000.0,
        );

        let with_home = GeminiApi::for_test(
            dir.path().to_path_buf(),
            "http://127.0.0.1/quota",
            "http://127.0.0.1/codeassist",
            "http://127.0.0.1/token",
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

    /// Pins SBS-950: no home refuses persist before a path is chosen, so
    /// this path cannot create `./.gemini/oauth_creds.json`.
    ///
    /// Deliberately does not move the process into a temp directory to prove
    /// it. `set_current_dir` is process-global and these tests run on several
    /// threads, so it would race every other test that touches a relative
    /// path. `gemini_dir_is_absent_when_there_is_no_home` pins the path
    /// choice itself, which is what the working-directory probe hung off.
    #[test]
    fn missing_home_does_not_write_credentials() {
        let api = api_without_home();
        assert_eq!(
            api.gemini_dir(),
            None,
            "there must be no directory to persist into"
        );

        let error = api
            .save_credentials(&refreshed_credentials("live-access"))
            .expect_err("no home must not persist tokens");
        assert!(matches!(error, ProviderError::NotInstalled(_)));
    }

    /// Pins SBS-928: at the exact expiry second the access token is already
    /// treated as expired, and the 5-minute skew matches the auto-refresh interval.
    #[test]
    fn token_expiry_honors_the_five_minute_refresh_skew() {
        let now = Utc::now();
        let creds = |expiry: DateTime<Utc>| OAuthCredentials {
            access_token: Some("access".to_string()),
            id_token: None,
            refresh_token: Some("refresh".to_string()),
            expiry_date: Some(expiry.timestamp_millis() as f64),
        };

        assert!(
            creds(now).is_expired_at(now),
            "exact expiry second must refresh"
        );
        assert!(creds(now + chrono::Duration::seconds(30)).is_expired_at(now));
        assert!(creds(now + chrono::Duration::minutes(5)).is_expired_at(now));
        assert!(
            !creds(now + chrono::Duration::minutes(5) + chrono::Duration::seconds(1))
                .is_expired_at(now)
        );

        let mut unknown = creds(now);
        unknown.expiry_date = None;
        assert!(
            !unknown.is_expired_at(now),
            "missing expiry is unknown, not expired"
        );
        unknown.expiry_date = Some(f64::NAN);
        assert!(
            !unknown.is_expired_at(now),
            "non-finite expiry is unknown, not expired"
        );
    }

    fn write_gemini_home(dir: &Path, access: &str, refresh: Option<&str>, expiry_ms: f64) {
        let gemini = dir.join(".gemini");
        std::fs::create_dir_all(&gemini).expect("gemini dir");
        let mut creds = serde_json::json!({
            "access_token": access,
            "id_token": test_id_token("user-1", "user@example.com"),
            "expiry_date": expiry_ms,
        });
        if let Some(refresh) = refresh {
            creds["refresh_token"] = serde_json::Value::String(refresh.to_string());
        }
        std::fs::write(
            gemini.join("oauth_creds.json"),
            serde_json::to_vec_pretty(&creds).expect("creds bytes"),
        )
        .expect("write creds");
        std::fs::write(
            gemini.join("client_config.json"),
            r#"{"client_id":"test-client","client_secret":"test-secret"}"#,
        )
        .expect("write client config");
    }

    fn api_for_server(home: &Path, server: &mockito::Server) -> GeminiApi {
        GeminiApi::for_test(
            home.to_path_buf(),
            &format!("{}/quota", server.url()),
            &format!("{}/codeassist", server.url()),
            &format!("{}/token", server.url()),
        )
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

    /// Pins SBS-928: a 401 on a still-refreshable seat refreshes once and
    /// completes the poll instead of surfacing AuthRequired.
    #[tokio::test]
    async fn quota_401_refreshes_once_and_retries_successfully() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expiry_ms = (Utc::now() + chrono::Duration::hours(1)).timestamp_millis() as f64;
        write_gemini_home(dir.path(), "stale-access", Some("refresh-me"), expiry_ms);

        let mut server = mockito::Server::new_async().await;
        let _code_assist = mock_code_assist(&mut server).await;
        let stale_quota = server
            .mock("POST", "/quota")
            .match_header("authorization", "Bearer stale-access")
            .with_status(401)
            .expect(1)
            .create_async()
            .await;
        let refresh = server
            .mock("POST", "/token")
            .with_status(200)
            .with_body(r#"{"access_token":"fresh-access","expires_in":3600}"#)
            .expect(1)
            .create_async()
            .await;
        let fresh_quota = server
            .mock("POST", "/quota")
            .match_header("authorization", "Bearer fresh-access")
            .with_status(200)
            .with_body(quota_ok_body())
            .expect(1)
            .create_async()
            .await;

        let api = api_for_server(dir.path(), &server);
        let (primary, _, _, plan) = api
            .fetch_quota(&FetchContext::default())
            .await
            .expect("refreshable 401 must succeed after refresh");

        stale_quota.assert_async().await;
        refresh.assert_async().await;
        fresh_quota.assert_async().await;
        assert!((primary.used_percent - 20.0).abs() < 0.01);
        assert_eq!(plan.as_deref(), Some("Paid"));
    }

    /// Pins SBS-928: a token 30s from expiry is inside the 5-minute skew, so
    /// the poll refreshes first and the user still gets a successful reading.
    #[tokio::test]
    async fn token_thirty_seconds_from_expiry_still_polls_after_refresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expiry_ms = (Utc::now() + chrono::Duration::seconds(30)).timestamp_millis() as f64;
        write_gemini_home(dir.path(), "stale-access", Some("refresh-me"), expiry_ms);

        let mut server = mockito::Server::new_async().await;
        let _code_assist = mock_code_assist(&mut server).await;
        let stale_quota = server
            .mock("POST", "/quota")
            .match_header("authorization", "Bearer stale-access")
            .with_status(401)
            .expect(0)
            .create_async()
            .await;
        let refresh = server
            .mock("POST", "/token")
            .with_status(200)
            .with_body(r#"{"access_token":"fresh-access","expires_in":3600}"#)
            .expect(1)
            .create_async()
            .await;
        let fresh_quota = server
            .mock("POST", "/quota")
            .match_header("authorization", "Bearer fresh-access")
            .with_status(200)
            .with_body(quota_ok_body())
            .expect(1)
            .create_async()
            .await;

        let api = api_for_server(dir.path(), &server);
        let (primary, _, _, _) = api
            .fetch_quota(&FetchContext::default())
            .await
            .expect("token 30s from expiry must refresh then succeed");

        stale_quota.assert_async().await;
        refresh.assert_async().await;
        fresh_quota.assert_async().await;
        assert!((primary.used_percent - 20.0).abs() < 0.01);
    }

    /// A refresh that cannot be reached must not cost a token Google would
    /// still accept.
    ///
    /// Refreshing early is a precaution. Failing the whole poll because the
    /// precaution failed, while holding an access token that is still in date,
    /// would turn a token-endpoint outage into a signed-out provider.
    #[tokio::test]
    async fn a_failed_early_refresh_still_polls_with_the_valid_access_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expiry_ms = (Utc::now() + chrono::Duration::seconds(30)).timestamp_millis() as f64;
        write_gemini_home(dir.path(), "stale-access", Some("refresh-me"), expiry_ms);

        let mut server = mockito::Server::new_async().await;
        let _code_assist = mock_code_assist(&mut server).await;
        let refresh = server
            .mock("POST", "/token")
            .with_status(503)
            .expect(1)
            .create_async()
            .await;
        let quota = server
            .mock("POST", "/quota")
            .match_header("authorization", "Bearer stale-access")
            .with_status(200)
            .with_body(quota_ok_body())
            .expect(1)
            .create_async()
            .await;

        let api = api_for_server(dir.path(), &server);
        let (primary, _, _, _) = api
            .fetch_quota(&FetchContext::default())
            .await
            .expect("a token still in date must be used when the refresh fails");

        refresh.assert_async().await;
        quota.assert_async().await;
        assert!((primary.used_percent - 20.0).abs() < 0.01);
    }

    /// With no refresh token there is nothing to refresh, so the near-expiry
    /// precaution is skipped rather than attempted and failed.
    #[tokio::test]
    async fn a_near_expiry_token_without_a_refresh_token_still_polls() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expiry_ms = (Utc::now() + chrono::Duration::seconds(30)).timestamp_millis() as f64;
        write_gemini_home(dir.path(), "only-access", None, expiry_ms);

        let mut server = mockito::Server::new_async().await;
        let _code_assist = mock_code_assist(&mut server).await;
        let refresh = server.mock("POST", "/token").expect(0).create_async().await;
        let quota = server
            .mock("POST", "/quota")
            .match_header("authorization", "Bearer only-access")
            .with_status(200)
            .with_body(quota_ok_body())
            .expect(1)
            .create_async()
            .await;

        let api = api_for_server(dir.path(), &server);
        api.fetch_quota(&FetchContext::default())
            .await
            .expect("a seat with no refresh token must still poll");

        refresh.assert_async().await;
        quota.assert_async().await;
    }

    /// A revoked refresh token is still AuthRequired. 401 is not collapsed
    /// into "signed out" until refresh itself fails.
    #[tokio::test]
    async fn revoked_refresh_token_on_401_is_auth_required() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expiry_ms = (Utc::now() + chrono::Duration::hours(1)).timestamp_millis() as f64;
        write_gemini_home(
            dir.path(),
            "stale-access",
            Some("revoked-refresh"),
            expiry_ms,
        );

        let mut server = mockito::Server::new_async().await;
        let _code_assist = mock_code_assist(&mut server).await;
        let stale_quota = server
            .mock("POST", "/quota")
            .match_header("authorization", "Bearer stale-access")
            .with_status(401)
            .expect(1)
            .create_async()
            .await;
        let refresh = server
            .mock("POST", "/token")
            .with_status(400)
            .with_body(r#"{"error":"invalid_grant"}"#)
            .expect(1)
            .create_async()
            .await;

        let api = api_for_server(dir.path(), &server);
        let error = api
            .fetch_quota(&FetchContext::default())
            .await
            .expect_err("revoked refresh must stay AuthRequired");

        stale_quota.assert_async().await;
        refresh.assert_async().await;
        assert!(matches!(error, ProviderError::AuthRequired));
    }

    /// Pins SBS-950: the quota poll is NotInstalled when there is no home,
    /// so it never reaches Google or persist.
    #[tokio::test]
    async fn fetch_quota_without_home_is_not_logged_in() {
        let error = api_without_home()
            .fetch_quota(&FetchContext::default())
            .await
            .expect_err("no home is not logged in");
        assert!(matches!(error, ProviderError::NotInstalled(_)));
    }

    #[tokio::test]
    async fn quota_401_without_refresh_token_is_auth_required() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expiry_ms = (Utc::now() + chrono::Duration::hours(1)).timestamp_millis() as f64;
        write_gemini_home(dir.path(), "stale-access", None, expiry_ms);

        let mut server = mockito::Server::new_async().await;
        let _code_assist = mock_code_assist(&mut server).await;
        let stale_quota = server
            .mock("POST", "/quota")
            .match_header("authorization", "Bearer stale-access")
            .with_status(401)
            .expect(1)
            .create_async()
            .await;
        let refresh = server.mock("POST", "/token").expect(0).create_async().await;

        let api = api_for_server(dir.path(), &server);
        let error = api
            .fetch_quota(&FetchContext::default())
            .await
            .expect_err("no refresh token is a hard AuthRequired");

        stale_quota.assert_async().await;
        refresh.assert_async().await;
        assert!(matches!(error, ProviderError::AuthRequired));
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
}
