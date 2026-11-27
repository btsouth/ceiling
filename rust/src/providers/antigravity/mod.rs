//! Antigravity provider implementation.
//!
//! Fetches usage data exclusively via the official read-only Antigravity CLI
//! `/usage` command.

#[cfg(windows)]
mod cli_quota;

use async_trait::async_trait;

use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    SourceMode,
};

#[cfg(windows)]
const NOT_INSTALLED_MESSAGE: &str = "Antigravity CLI (`agy`) is not installed. Install Antigravity CLI 1.2.13 or later, sign in, then retry.";

#[cfg(not(windows))]
const UNSUPPORTED_PLATFORM_MESSAGE: &str =
    "Antigravity CLI quota lookup is only supported on Windows.";

/// Antigravity provider
pub struct AntigravityProvider {
    metadata: ProviderMetadata,
}

#[cfg(windows)]
pub(crate) fn cli_quota_configured() -> bool {
    cli_quota::binary().is_some()
}

impl AntigravityProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::Antigravity,
                display_name: "Antigravity",
                session_label: "Gemini 5-hour",
                weekly_label: "Gemini Weekly",
                supports_opus: false,
                supports_credits: false,
                default_enabled: false,
                is_primary: false,
                dashboard_url: None,
                status_page_url: None,
            },
        }
    }
}

impl Default for AntigravityProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for AntigravityProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Antigravity
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, _ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        #[cfg(windows)]
        {
            let binary = cli_quota::binary()
                .ok_or_else(|| ProviderError::NotInstalled(NOT_INSTALLED_MESSAGE.to_string()))?;
            tracing::debug!("Fetching Antigravity usage via official CLI");
            cli_quota::fetch(binary).await
        }
        #[cfg(not(windows))]
        {
            Err(ProviderError::Other(
                UNSUPPORTED_PLATFORM_MESSAGE.to_string(),
            ))
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::Cli]
    }

    fn supports_cli(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_exposes_cli_antigravity_provider() {
        let provider = AntigravityProvider::new();
        assert_eq!(provider.id(), ProviderId::Antigravity);
        assert_eq!(provider.metadata().display_name, "Antigravity");
        assert!(provider.supports_cli());
        assert_eq!(
            provider.available_sources(),
            vec![SourceMode::Auto, SourceMode::Cli]
        );
    }
}
