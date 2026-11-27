//! Codex (OpenAI/ChatGPT) provider implementation
//!
//! Fetches usage data from ChatGPT's backend API using OAuth credentials
//! stored by the Codex CLI in ~/.codex/auth.json

mod api;

use async_trait::async_trait;

use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    SourceMode,
};

pub use api::CodexApi;

/// Detect a usable local Codex CLI sign-in without exposing credential data.
pub fn local_credentials_available() -> bool {
    CodexApi::new().has_local_credentials()
}

pub fn cli_installed() -> bool {
    which_codex().is_some()
}

/// Codex provider for fetching AI usage limits
pub struct CodexProvider {
    metadata: ProviderMetadata,
    api: CodexApi,
}

impl CodexProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::Codex,
                display_name: "Codex",
                session_label: "Session",
                weekly_label: "Weekly",
                supports_opus: false,
                supports_credits: true,
                default_enabled: true,
                is_primary: true,
                dashboard_url: Some("https://chatgpt.com/codex/settings/usage"),
                status_page_url: Some("https://status.openai.com"),
            },
            api: CodexApi::new(),
        }
    }
}

impl Default for CodexProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for CodexProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Codex
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        tracing::debug!("Fetching Codex usage via OAuth API");

        // A configured account pins the credential to its own CODEX_HOME;
        // otherwise follow whichever account the CLI is signed in as.
        let scoped;
        let api = match &ctx.account_config_dir {
            Some(dir) => {
                scoped = self.api.scoped(dir.clone());
                &scoped
            }
            None => &self.api,
        };

        match api.fetch_usage().await {
            Ok((mut usage, cost)) => {
                // Stamp which account this reading belongs to. Capacity baselines
                // are scoped by it, so without it two accounts share one baseline
                // and a switch inherits the previous seat's history.
                if let Some(identity) = api.identity() {
                    if let Some(email) = identity.email {
                        usage = usage.with_email(email);
                    } else if let Some(account_id) = identity.account_id {
                        // No email claim, but the account id still separates seats.
                        usage = usage.with_email(account_id);
                    }
                }

                let mut result = ProviderFetchResult::new(usage, "oauth");
                if let Some(c) = cost {
                    result = result.with_cost(c);
                }
                Ok(result)
            }
            Err(e) => {
                tracing::warn!("Codex API fetch failed: {}", e);
                Err(e)
            }
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::OAuth, SourceMode::Cli]
    }

    fn supports_oauth(&self) -> bool {
        true
    }

    fn supports_cli(&self) -> bool {
        true
    }

    fn detect_version(&self) -> Option<String> {
        detect_codex_version()
    }
}

/// Try to find the codex CLI binary
fn which_codex() -> Option<std::path::PathBuf> {
    // Check common locations on Windows
    let possible_paths = [
        // In PATH
        which::which("codex").ok(),
        // npm global install
        dirs::data_dir().map(|p| p.join("npm").join("codex.cmd")),
        // AppData locations
        dirs::data_local_dir().map(|p| p.join("Programs").join("codex").join("codex.exe")),
    ];

    possible_paths.into_iter().flatten().find(|p| p.exists())
}

/// Detect the version of the codex CLI
fn detect_codex_version() -> Option<String> {
    let codex_path = which_codex()?;

    let mut cmd = std::process::Command::new(codex_path);
    cmd.args(["--version"]);
    let output = crate::providers::cli_probe::output_with_timeout(
        &mut cmd,
        crate::providers::cli_probe::PROBE_TIMEOUT,
    )?;

    if output.status.success() {
        let version_str = String::from_utf8_lossy(&output.stdout);
        extract_version(&version_str)
    } else {
        None
    }
}

/// Extract version number from a string like "codex 1.2.3"
fn extract_version(s: &str) -> Option<String> {
    let re = regex_lite::Regex::new(r"(\d+(?:\.\d+)+)").ok()?;
    re.find(s).map(|m| m.as_str().to_string())
}
