//! OpenCode Go provider implementation
//!
//! Separate workspace surface that shares the `opencode.ai` cookie domain with
//! the OpenCode provider. Tries the current Console workspace surface first;
//! falls back to the legacy `/workspace/<id>/go` scraper only when a legacy
//! `auth` cookie is present and the Console attempt fails recoverably.
//! Ported from Win-CodexBar's Console fallback (upstream v0.64.0) and scoped
//! down to this fork's simpler cookie-scrape-only provider (no local-SQLite
//! or API-key source modes).

mod console;
mod legacy;

use async_trait::async_trait;
use reqwest::Client;
use std::time::Duration;

use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    RateWindow, SourceMode, UsageSnapshot,
};

const BASE_URL: &str = "https://opencode.ai";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
/// Timeout for Console API calls; the legacy scraper uses the client's own
/// (30s) default instead.
const CONSOLE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct OpenCodeGoProvider {
    metadata: ProviderMetadata,
    client: Client,
}

/// Which fallback paths a cookie header can support, sniffed once per fetch.
#[derive(Clone, Copy)]
struct CookieCapabilities {
    console: bool,
    legacy: bool,
}

/// Why the Console-first attempt produced no snapshot. `NoSubscription` is
/// terminal: the Console answered authoritatively, so it is surfaced instead
/// of retried against the legacy scraper.
enum ConsoleError {
    NoSubscription,
    Failed(ProviderError),
}

fn cookie_capabilities(cookie_header: &str) -> CookieCapabilities {
    let has_cookie = |names: &[&str]| {
        cookie_header.split(';').any(|part| {
            let Some((name, value)) = part.trim().split_once('=') else {
                return false;
            };
            names.contains(&name.trim()) && !value.trim().is_empty()
        })
    };
    CookieCapabilities {
        console: has_cookie(&["__Host-console_session"]),
        legacy: has_cookie(&["auth", "__Host-auth"]),
    }
}

/// Errors worth retrying against the legacy scraper rather than surfacing
/// directly — anything that looks like the Console surface itself is
/// unreachable or misbehaving, as opposed to "no subscription", which is
/// authoritative and not retried.
fn is_recoverable(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::AuthRequired
            | ProviderError::Parse(_)
            | ProviderError::Other(_)
            | ProviderError::Timeout
            | ProviderError::Network(_)
    )
}

impl OpenCodeGoProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::OpenCodeGo,
                display_name: "OpenCode Go",
                session_label: "Rolling (5h)",
                weekly_label: "Weekly",
                supports_opus: false,
                supports_credits: false,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some("https://opencode.ai"),
                status_page_url: None,
            },
            client: crate::core::credentialed_http_client_builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    fn workspace_id_from_context(workspace_id: Option<&str>) -> Option<String> {
        console::normalize_workspace_id(workspace_id)
    }

    /// The legacy scraper addresses `/workspace/<id>/go`, which only accepts
    /// `wrk_` IDs, so an `org_` override (including one normalized out of a
    /// Console URL) still falls back to discovery.
    fn legacy_workspace_id_from_context(workspace_id: Option<&str>) -> Option<String> {
        Self::workspace_id_from_context(workspace_id).filter(|id| id.starts_with("wrk_"))
    }

    /// Two auth failures are still an auth failure, so the browser-cookie path
    /// can end in the sign-in prompt; any other pair keeps both causes.
    fn combine_fallback_errors(
        console_error: ProviderError,
        legacy_error: ProviderError,
    ) -> ProviderError {
        match (&console_error, &legacy_error) {
            (ProviderError::AuthRequired, ProviderError::AuthRequired) => {
                ProviderError::AuthRequired
            }
            _ => ProviderError::Other(format!("Console: {console_error}; Legacy: {legacy_error}")),
        }
    }

    /// Zen balance is credit *remaining*, so it is attached as an info-only
    /// extra window and deliberately never turned into a `CostSnapshot`:
    /// doing so would make spend fall as the user spends, and read $0 at
    /// exactly the moment the balance runs out (#208).
    fn attach_zen_balance(usage: UsageSnapshot, balance: f64) -> UsageSnapshot {
        usage.with_extra_rate_window(
            "zen-balance",
            "Zen balance",
            RateWindow::with_details(0.0, None, None, Some(format!("${balance:.2}"))),
        )
    }

    async fn fetch_console(
        &self,
        cookie_header: &str,
        workspace_override: Option<&str>,
    ) -> Result<ProviderFetchResult, ConsoleError> {
        let workspace_id = match Self::workspace_id_from_context(workspace_override) {
            Some(id) => id,
            None => console::fetch_workspace_id(&self.client, cookie_header, CONSOLE_TIMEOUT)
                .await
                .map_err(ConsoleError::Failed)?,
        };
        match console::fetch_usage(&self.client, &workspace_id, cookie_header, CONSOLE_TIMEOUT)
            .await
            .map_err(ConsoleError::Failed)?
        {
            console::ConsoleUsage::Snapshot(usage) => {
                let mut usage = *usage;
                if let Ok(Some(balance)) = console::fetch_balance(
                    &self.client,
                    &workspace_id,
                    cookie_header,
                    CONSOLE_TIMEOUT,
                )
                .await
                {
                    usage = Self::attach_zen_balance(usage, balance);
                }
                Ok(ProviderFetchResult::new(usage, "web"))
            }
            console::ConsoleUsage::NoSubscription => Err(ConsoleError::NoSubscription),
        }
    }

    async fn fetch_legacy(
        &self,
        cookie_header: &str,
        workspace_override: Option<&str>,
    ) -> Result<ProviderFetchResult, ProviderError> {
        let workspace_id = match Self::legacy_workspace_id_from_context(workspace_override) {
            Some(id) => id,
            None => legacy::discover_workspace_id(&self.client, cookie_header).await?,
        };
        let legacy::LegacyUsage {
            usage,
            embedded_balance,
        } = legacy::fetch_usage(&self.client, cookie_header, &workspace_id).await?;

        let balance = match embedded_balance {
            Some(balance) => Some(balance),
            None => {
                legacy::fetch_balance(&self.client, cookie_header, &workspace_id, CONSOLE_TIMEOUT)
                    .await
                    .ok()
                    .flatten()
            }
        };
        let usage = match balance {
            Some(balance) => Self::attach_zen_balance(usage, balance),
            None => usage,
        };
        Ok(ProviderFetchResult::new(usage, "web"))
    }

    async fn fetch_with_cookies(
        &self,
        cookie_header: &str,
        workspace_id_override: Option<&str>,
    ) -> Result<ProviderFetchResult, ProviderError> {
        let capabilities = cookie_capabilities(cookie_header);

        match self
            .fetch_console(cookie_header, workspace_id_override)
            .await
        {
            Ok(result) => Ok(result),
            Err(ConsoleError::NoSubscription) => Err(ProviderError::Parse(
                "No OpenCode Go subscription is available".to_string(),
            )),
            Err(ConsoleError::Failed(console_error))
                if capabilities.legacy && is_recoverable(&console_error) =>
            {
                match self
                    .fetch_legacy(cookie_header, workspace_id_override)
                    .await
                {
                    Ok(result) => Ok(result),
                    // Both paths failed — surface both causes. Silently
                    // preferring one used to hide the real failure behind
                    // whichever path happened to fail with a vaguer message.
                    Err(legacy_error) => {
                        Err(Self::combine_fallback_errors(console_error, legacy_error))
                    }
                }
            }
            Err(ConsoleError::Failed(error)) => Err(error),
        }
    }
}

impl Default for OpenCodeGoProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for OpenCodeGoProvider {
    fn id(&self) -> ProviderId {
        ProviderId::OpenCodeGo
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        tracing::debug!("Fetching OpenCode Go usage");

        match ctx.source_mode {
            SourceMode::Auto | SourceMode::Web => {
                if let Some(ref cookie_header) = ctx.manual_cookie_header {
                    return self
                        .fetch_with_cookies(cookie_header, ctx.workspace_id.as_deref())
                        .await;
                }

                match crate::providers::browser_cookie_header(&["opencode.ai"]) {
                    Ok(cookie_header) => match self
                        .fetch_with_cookies(&cookie_header, ctx.workspace_id.as_deref())
                        .await
                    {
                        Ok(result) => return Ok(result),
                        Err(ProviderError::AuthRequired) => {}
                        Err(e) => return Err(e),
                    },
                    Err(ProviderError::NoCookies) => {}
                    Err(e) => return Err(e),
                }

                Err(ProviderError::AuthRequired)
            }
            SourceMode::Cli => Err(ProviderError::UnsupportedSource(SourceMode::Cli)),
            SourceMode::OAuth => Err(ProviderError::UnsupportedSource(SourceMode::OAuth)),
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::Web]
    }

    fn supports_web(&self) -> bool {
        true
    }

    fn supports_cli(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zen_balance_is_shown_as_remaining_not_as_spend() {
        // `currentBalance` is credit left. It used to be reported as
        // CostSnapshot.used, which inverted it: spend fell as the user spent
        // and read $0 exactly when the balance ran out.
        let usage = UsageSnapshot::new(RateWindow::with_details(40.0, Some(300), None, None));
        let usage = OpenCodeGoProvider::attach_zen_balance(usage, 12.50);
        let result = ProviderFetchResult::new(usage, "web");

        assert!(result.cost.is_none(), "a balance is not money spent");
        let zen = result
            .usage
            .extra_rate_windows
            .iter()
            .find(|w| w.id == "zen-balance")
            .expect("zen balance line");
        assert_eq!(zen.window.reset_description.as_deref(), Some("$12.50"));
        assert_eq!(zen.window.used_percent, 0.0, "info-only, not a meter");
    }

    #[test]
    fn uses_context_workspace_id_before_discovery() {
        assert_eq!(
            OpenCodeGoProvider::workspace_id_from_context(Some("wrk_override")),
            Some("wrk_override".to_string())
        );
        assert_eq!(
            OpenCodeGoProvider::workspace_id_from_context(Some("")),
            None
        );
    }

    #[test]
    fn legacy_uses_wrk_override_and_discovers_for_other_ids() {
        assert_eq!(
            OpenCodeGoProvider::legacy_workspace_id_from_context(Some("wrk_override")),
            Some("wrk_override".to_string())
        );
        assert_eq!(
            OpenCodeGoProvider::legacy_workspace_id_from_context(Some(
                "https://opencode.ai/workspace/wrk_url123/go"
            )),
            Some("wrk_url123".to_string())
        );
        // An org ID is a valid Console workspace but not a legacy one.
        assert_eq!(
            OpenCodeGoProvider::legacy_workspace_id_from_context(Some("org_123")),
            None
        );
        assert_eq!(
            OpenCodeGoProvider::legacy_workspace_id_from_context(None),
            None
        );
    }

    #[test]
    fn double_auth_required_stays_auth_required() {
        assert!(matches!(
            OpenCodeGoProvider::combine_fallback_errors(
                ProviderError::AuthRequired,
                ProviderError::AuthRequired
            ),
            ProviderError::AuthRequired
        ));
    }

    #[test]
    fn mixed_double_failure_keeps_both_causes() {
        let error = OpenCodeGoProvider::combine_fallback_errors(
            ProviderError::Parse("console broke".to_string()),
            ProviderError::Other("legacy broke".to_string()),
        );
        match error {
            ProviderError::Other(message) => {
                assert!(message.contains("console broke"));
                assert!(message.contains("legacy broke"));
            }
            other => panic!("expected combined Other, got {other:?}"),
        }
    }

    #[test]
    fn console_cookie_is_detected_independently_of_legacy() {
        let capabilities = cookie_capabilities("__Host-console_session=abc; other=1");
        assert!(capabilities.console);
        assert!(!capabilities.legacy);

        let capabilities = cookie_capabilities("auth=xyz");
        assert!(!capabilities.console);
        assert!(capabilities.legacy);

        let capabilities = cookie_capabilities("unrelated=1");
        assert!(!capabilities.console);
        assert!(!capabilities.legacy);
    }

    #[test]
    fn recoverable_errors_allow_legacy_fallback() {
        assert!(is_recoverable(&ProviderError::AuthRequired));
        assert!(is_recoverable(&ProviderError::Parse("x".into())));
        assert!(is_recoverable(&ProviderError::Timeout));
        assert!(!is_recoverable(&ProviderError::UnsupportedSource(
            SourceMode::Cli
        )));
    }
}
