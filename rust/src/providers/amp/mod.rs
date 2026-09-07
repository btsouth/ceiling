//! Amp provider implementation
//!
//! Amp is Sourcegraph's AI coding assistant
//! Fetches usage data from Amp's local config or API

use async_trait::async_trait;
use std::path::PathBuf;

use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    RateWindow, SourceMode, UsageSnapshot,
};

/// Amp provider (Sourcegraph)
pub struct AmpProvider {
    metadata: ProviderMetadata,
}

impl AmpProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::Amp,
                display_name: "Amp",
                session_label: "Usage",
                weekly_label: "Monthly",
                supports_opus: false,
                supports_credits: true,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some("https://ampcode.com/settings/usage"),
                status_page_url: Some("https://sourcegraphstatus.com"),
            },
        }
    }

    /// Get Amp config directory
    fn get_amp_config_path() -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            dirs::config_dir().map(|p| p.join("amp"))
        }
        #[cfg(not(target_os = "windows"))]
        {
            dirs::home_dir().map(|p| p.join(".amp"))
        }
    }

    /// Get Sourcegraph/Cody config directory (Amp might use this)
    fn get_cody_config_path() -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            dirs::config_dir().map(|p| p.join("sourcegraph-cody"))
        }
        #[cfg(not(target_os = "windows"))]
        {
            dirs::home_dir().map(|p| p.join(".sourcegraph"))
        }
    }

    /// Read Amp/Sourcegraph access token
    async fn read_access_token(&self, ctx: &FetchContext) -> Result<String, ProviderError> {
        if let Some(token) = access_token_from_context(ctx) {
            return Ok(token);
        }

        if let Some(token) = access_token_from_environment() {
            return Ok(token);
        }

        if let Some(token) = Self::read_local_config_token().await {
            return Ok(token);
        }

        Err(ProviderError::AuthRequired)
    }

    async fn read_local_config_token() -> Option<String> {
        let amp_token = read_access_token_config(Self::get_amp_config_path()).await;
        if amp_token.is_some() {
            return amp_token;
        }

        read_access_token_config(Self::get_cody_config_path()).await
    }

    /// Fetch usage via Sourcegraph API
    async fn fetch_via_web(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let token = self.read_access_token(ctx).await?;

        let client = crate::core::credentialed_http_client_builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ProviderError::Other(e.to_string()))?;

        // Sourcegraph Cody usage API
        let resp = client
            .get("https://sourcegraph.com/.api/cody/current-user/usage")
            .header("Authorization", format!("token {}", token))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(ProviderError::AuthRequired);
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))?;

        usage_from_amp_payload(&json)
    }

    /// Probe for Amp installation. Configured credentials are not a usage reading.
    async fn probe_cli(&self, ctx: &FetchContext) -> Result<UsageSnapshot, ProviderError> {
        let has_api_key = ctx.api_key.as_ref().map(|k| !k.is_empty()).unwrap_or(false);

        let has_env =
            std::env::var("SRC_ACCESS_TOKEN").is_ok() || std::env::var("AMP_ACCESS_TOKEN").is_ok();

        let has_amp_config = Self::get_amp_config_path()
            .map(|p| p.join("config.json").exists())
            .unwrap_or(false);

        let has_cody_config = Self::get_cody_config_path()
            .map(|p| p.join("config.json").exists())
            .unwrap_or(false);

        usage_from_configured_probe(has_api_key || has_env || has_amp_config || has_cody_config)
    }
}

/// Amp JSON without used/limit is a decode miss, not 0% (SBS-1061).
fn usage_from_amp_payload(json: &serde_json::Value) -> Result<UsageSnapshot, ProviderError> {
    let used = json
        .get("completionsUsed")
        .or_else(|| json.get("used"))
        .and_then(|v| v.as_f64())
        .ok_or_else(|| {
            ProviderError::Parse("Amp usage response has no used reading".to_string())
        })?;

    let limit = json
        .get("completionsLimit")
        .or_else(|| json.get("limit"))
        .and_then(|v| v.as_f64())
        .ok_or_else(|| {
            ProviderError::Parse("Amp usage response has no limit reading".to_string())
        })?;

    let used_percent = if limit > 0.0 {
        (used / limit) * 100.0
    } else {
        return Err(ProviderError::Parse(
            "Amp usage response has a non-positive limit".to_string(),
        ));
    };

    let plan = json
        .get("plan")
        .or_else(|| json.get("tier"))
        .and_then(|v| v.as_str())
        .unwrap_or("Pro");

    let reset_time = json
        .get("resetAt")
        .or_else(|| json.get("periodEnd"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let primary_window = RateWindow::with_details(used_percent, None, None, reset_time);
    Ok(UsageSnapshot::new(primary_window).with_login_method(plan))
}

/// Auto used to fall through here and mint 0% when the fetch failed (SBS-1061).
fn usage_from_configured_probe(configured: bool) -> Result<UsageSnapshot, ProviderError> {
    if configured {
        Err(ProviderError::Other(
            "Amp is configured, but usage could not be fetched".to_string(),
        ))
    } else {
        Err(ProviderError::NotInstalled(
            "Amp not configured. Set SRC_ACCESS_TOKEN environment variable or configure Amp."
                .to_string(),
        ))
    }
}

fn access_token_from_context(ctx: &FetchContext) -> Option<String> {
    ctx.api_key
        .as_deref()
        .filter(|api_key| !api_key.is_empty())
        .map(str::to_string)
}

fn access_token_from_environment() -> Option<String> {
    std::env::var("SRC_ACCESS_TOKEN")
        .ok()
        .or_else(|| std::env::var("AMP_ACCESS_TOKEN").ok())
}

async fn read_access_token_config(config_dir: Option<PathBuf>) -> Option<String> {
    let config_file = config_dir?.join("config.json");
    if !config_file.exists() {
        return None;
    }

    let content = tokio::fs::read_to_string(config_file).await.ok()?;
    let json = serde_json::from_str::<serde_json::Value>(&content).ok()?;
    json.get("accessToken")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

impl Default for AmpProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for AmpProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Amp
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        tracing::debug!("Fetching Amp usage");

        match ctx.source_mode {
            SourceMode::Auto => {
                if let Ok(usage) = self.fetch_via_web(ctx).await {
                    return Ok(ProviderFetchResult::new(usage, "web"));
                }
                let usage = self.probe_cli(ctx).await?;
                Ok(ProviderFetchResult::new(usage, "cli"))
            }
            SourceMode::Web => {
                let usage = self.fetch_via_web(ctx).await?;
                Ok(ProviderFetchResult::new(usage, "web"))
            }
            SourceMode::Cli => {
                let usage = self.probe_cli(ctx).await?;
                Ok(ProviderFetchResult::new(usage, "cli"))
            }
            SourceMode::OAuth => Err(ProviderError::UnsupportedSource(SourceMode::OAuth)),
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::Web, SourceMode::Cli]
    }

    fn supports_web(&self) -> bool {
        true
    }

    fn supports_cli(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_points_to_current_usage_page() {
        assert_eq!(
            AmpProvider::new().metadata().dashboard_url,
            Some("https://ampcode.com/settings/usage")
        );
    }

    fn assert_failure_is_not_zero_percent(result: Result<UsageSnapshot, ProviderError>) {
        if let Ok(usage) = result {
            panic!(
                "failure must not be reported as {}% used",
                usage.primary.used_percent
            );
        }
    }

    /// SBS-1061: a payload with no used/limit used to become 0% of a guessed 500.
    #[test]
    fn missing_usage_fields_are_not_reported_as_zero_percent() {
        assert_failure_is_not_zero_percent(usage_from_amp_payload(&serde_json::json!({})));
        assert_failure_is_not_zero_percent(usage_from_amp_payload(&serde_json::json!({
            "plan": "Pro"
        })));
        let err = usage_from_amp_payload(&serde_json::json!({}))
            .expect_err("missing used/limit is a decode failure");
        assert!(
            matches!(err, ProviderError::Parse(_)),
            "missing fields must stay Parse, got {err:?}"
        );
    }

    /// A real 0/500 reading is still 0%. The bug is inventing that from absence.
    #[test]
    fn reported_zero_used_is_a_reading() {
        let usage = usage_from_amp_payload(&serde_json::json!({
            "used": 0.0,
            "limit": 500.0,
            "plan": "Pro"
        }))
        .expect("explicit zero is a reading");
        assert_eq!(usage.primary.used_percent, 0.0);
        assert_eq!(usage.login_method.as_deref(), Some("Pro"));
    }

    /// SBS-1061: Auto fell through to "configured → 0%" after a failed fetch.
    #[test]
    fn configured_probe_is_not_reported_as_zero_percent() {
        assert_failure_is_not_zero_percent(usage_from_configured_probe(true));
        assert_failure_is_not_zero_percent(usage_from_configured_probe(false));
    }
}
