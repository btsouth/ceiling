//! `codexbar mcp` — local-first MCP server for usage and spend.
//!
//! Speaks Model Context Protocol over stdio so Claude Code / Codex can query
//! remaining quota and estimated spend mid-conversation. Quota comes from the
//! desktop widget snapshot (cache-only, no network). `get_spend` walks local
//! Codex, Claude, and Grok session logs via [`crate::cost_scanner`] for today,
//! 7 days, and 30 days. `get_status` reuses the snapshot and a 1-day spend
//! scan so a cap check does not pay for that 30-day walk.

use clap::Args;
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities,
        ServerInfo,
    },
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use serde_json::json;

use crate::core::{
    NamedRateWindow, ProviderId, RateWindow, WidgetProviderEntry, WidgetSnapshot,
    WidgetSnapshotStore,
};
use crate::cost_scanner::{CostScanner, CostSummary, get_cost_usage_report};
use crate::settings::Settings;

#[derive(Args, Debug, Clone, Default)]
pub struct McpArgs {
    /// Include account email and login method in tool output.
    /// Off by default; `codexbar serve` uses the same privacy default.
    #[arg(long)]
    pub include_identity: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ProviderFilter {
    /// Optional provider id (`claude`, `codex`, `cursor`, …). Omit for all.
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Clone)]
struct CeilingMcp {
    tool_router: rmcp::handler::server::router::tool::ToolRouter<CeilingMcp>,
    include_identity: bool,
}

#[tool_router]
impl CeilingMcp {
    fn new(include_identity: bool) -> Self {
        Self {
            tool_router: Self::tool_router(),
            include_identity,
        }
    }

    #[tool(
        description = "List providers with cached quota and whether local spend scanning is supported (Codex/Claude/Grok)."
    )]
    fn list_providers(&self) -> Result<CallToolResult, McpError> {
        Ok(json_tool_result(list_providers_payload(
            WidgetSnapshotStore::load().as_ref(),
            &enabled_provider_ids(),
        )))
    }

    #[tool(
        description = "Get remaining quota windows from the desktop widget snapshot (cache-only, no network). Requires Ceiling desktop to have refreshed recently. period_cost_usd is the provider's billed/current-period CostSnapshot.used (plus cost_period), not a single conversation's spend. Use get_spend / get_status today_spend for local estimated log spend."
    )]
    fn get_usage(
        &self,
        Parameters(ProviderFilter { provider }): Parameters<ProviderFilter>,
    ) -> Result<CallToolResult, McpError> {
        Ok(json_tool_result(usage_payload(
            WidgetSnapshotStore::load().as_ref(),
            provider.as_deref(),
            self.include_identity,
        )))
    }

    #[tool(
        description = "Get estimated API-value spend from local Codex/Claude/Grok logs for today, 7 days, and 30 days. Local-only; not a bill."
    )]
    fn get_spend(
        &self,
        Parameters(ProviderFilter { provider }): Parameters<ProviderFilter>,
    ) -> Result<CallToolResult, McpError> {
        Ok(json_tool_result(spend_payload(
            provider.as_deref(),
            &enabled_provider_ids(),
        )))
    }

    #[tool(
        description = "Cheap compact status: remaining quota from the desktop snapshot plus today's estimated spend from a 1-day local log scan (not the 30-day get_spend walk). Good for 'am I about to hit my cap?' checks. remaining_percent is the same window the desktop strip shows: Claude/Codex rank primary/secondary/tertiary (exhausted first, then highest used %); Cursor uses cursorStripWindow (hottest Auto/API with room, then on-demand when included lanes are gone or already billing, Plan only as fallback). usage.extra_rate_windows includes cursor-api and cursor-on-demand. usage.period_cost_usd is billed/current-period cost, not session spend; today_spend is local estimated log spend for today. Use get_spend for 7/30-day totals."
    )]
    fn get_status(
        &self,
        Parameters(ProviderFilter { provider }): Parameters<ProviderFilter>,
    ) -> Result<CallToolResult, McpError> {
        Ok(json_tool_result(status_payload(
            WidgetSnapshotStore::load().as_ref(),
            provider.as_deref(),
            &enabled_provider_ids(),
            self.include_identity,
        )))
    }
}

#[tool_handler]
impl ServerHandler for CeilingMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ceiling", env!("CARGO_PKG_VERSION")))
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "Ceiling local usage/spend tools. get_usage reads the desktop widget snapshot \
(cache-only). get_spend scans local Codex/Claude/Grok logs for today, 7 days, and 30 days. \
Prefer get_status for a cheap remaining-quota + today-$ check before starting a large job; \
it does not run the 30-day spend scan. remaining_percent is the same window \
the desktop strip shows: Claude/Codex rank primary/secondary/tertiary \
(exhausted first, then highest used %); Cursor uses cursorStripWindow so Plan \
does not outrank Auto and cursor-api / on-demand can bind the number. \
usage.period_cost_usd is the \
provider billed/current-period figure (CostSnapshot.used), not this conversation's spend; \
today_spend / get_spend are estimated API value from local logs, never a billed invoice. \
Account email and login method are omitted unless the server was started with \
--include-identity."
                    .to_string(),
            )
    }
}

pub async fn run(args: McpArgs) -> anyhow::Result<()> {
    tracing::info!("Starting Ceiling MCP server (stdio)");
    let service = CeilingMcp::new(args.include_identity)
        .serve(stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

fn json_tool_result(value: serde_json::Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(value.to_string())])
}

fn enabled_provider_ids() -> Vec<ProviderId> {
    Settings::load().get_enabled_provider_ids()
}

fn can_scan_local_spend(cli: &str) -> bool {
    ProviderId::from_cli_name(cli).is_some_and(CostScanner::supports_local_scan)
}

fn local_spend_supported(cli: &str, enabled: &[ProviderId]) -> bool {
    can_scan_local_spend(cli)
        && ProviderId::from_cli_name(cli).is_some_and(|id| enabled.contains(&id))
}

fn local_spend_cli_names(enabled: &[ProviderId]) -> Vec<&'static str> {
    enabled
        .iter()
        .copied()
        .filter(|&id| CostScanner::supports_local_scan(id))
        .map(|id| id.cli_name())
        .collect()
}

fn list_providers_payload(
    snapshot: Option<&WidgetSnapshot>,
    enabled: &[ProviderId],
) -> serde_json::Value {
    let snapshot_providers: Vec<String> = snapshot
        .map(|s| {
            s.entries
                .iter()
                .map(|e| e.provider.cli_name().to_string())
                .collect()
        })
        .unwrap_or_default();

    let mut providers = Vec::new();
    for id in ProviderId::all() {
        let cli = id.cli_name();
        let in_snapshot = snapshot_providers.iter().any(|p| p == cli);
        providers.push(json!({
            "id": cli,
            "display_name": id.display_name(),
            "has_quota_snapshot": in_snapshot,
            "local_spend_supported": local_spend_supported(cli, enabled),
        }));
    }

    json!({
        "snapshot_present": snapshot.is_some(),
        "snapshot_generated_at": snapshot.map(|s| s.generated_at.to_rfc3339()),
        "providers": providers,
    })
}

fn usage_payload(
    snapshot: Option<&WidgetSnapshot>,
    provider: Option<&str>,
    include_identity: bool,
) -> serde_json::Value {
    let Some(snapshot) = snapshot else {
        return json!({
            "ok": false,
            "error": "No widget snapshot found. Open Ceiling desktop so it can refresh and persist quota.",
            "providers": []
        });
    };

    let entries: Vec<&WidgetProviderEntry> = match provider {
        Some(name) => match ProviderId::from_cli_name(name) {
            Some(id) => snapshot.entry_for(id).into_iter().collect(),
            None => {
                return json!({
                    "ok": false,
                    "error": format!("Unknown provider '{name}'"),
                    "providers": []
                });
            }
        },
        None => snapshot.entries.iter().collect(),
    };

    let providers: Vec<_> = entries
        .iter()
        .map(|e| entry_usage_json(e, include_identity))
        .collect();
    json!({
        "ok": true,
        "source": "widget-snapshot",
        "generated_at": snapshot.generated_at.to_rfc3339(),
        "providers": providers,
    })
}

fn entry_usage_json(entry: &WidgetProviderEntry, include_identity: bool) -> serde_json::Value {
    json!({
        "provider": entry.provider.cli_name(),
        "display_name": entry.provider.display_name(),
        "updated_at": entry.updated_at.to_rfc3339(),
        "account_email": include_identity.then(|| entry.account_email.clone()).flatten(),
        "login_method": include_identity.then(|| entry.login_method.clone()).flatten(),
        "credits_remaining": entry.credits_remaining,
        "primary": window_json(entry.primary.as_ref()),
        "secondary": window_json(entry.secondary.as_ref()),
        "tertiary": window_json(entry.tertiary.as_ref()),
        "extra_rate_windows": entry
            .extra_rate_windows
            .iter()
            .map(extra_window_json)
            .collect::<Vec<_>>(),
        "period_cost_usd": entry.token_usage.as_ref().and_then(|t| t.period_cost_usd),
        "cost_period": entry.token_usage.as_ref().and_then(|t| t.cost_period.clone()),
    })
}

fn window_json(window: Option<&RateWindow>) -> serde_json::Value {
    let Some(window) = window else {
        return serde_json::Value::Null;
    };
    json!({
        "used_percent": window.used_percent,
        "remaining_percent": window.remaining_percent(),
        "window_minutes": window.window_minutes,
        "resets_at": window.resets_at.map(|dt| dt.to_rfc3339()),
        "reset_countdown": window.format_countdown(),
        "is_exhausted": window.is_exhausted(),
    })
}

fn extra_window_json(extra: &NamedRateWindow) -> serde_json::Value {
    let mut value = window_json(Some(&extra.window));
    if let Some(obj) = value.as_object_mut() {
        obj.insert("id".into(), json!(extra.id));
        obj.insert("title".into(), json!(extra.title));
        obj.insert(
            "amount".into(),
            extra
                .amount
                .as_ref()
                .map_or(serde_json::Value::Null, |amount| {
                    json!({
                        "used": amount.used,
                        "limit": amount.limit,
                        "currency_code": amount.currency_code,
                    })
                }),
        );
    }
    value
}

/// Inclusive local calendar days `get_spend` walks for today / 7-day / 30-day totals.
const GET_SPEND_DAYS: u32 = 30;

/// Inclusive local calendar days `get_status` walks for `today_spend`.
///
/// SBS-1033: agents treat get_status as a cheap cap check. Filling today's
/// dollars with the 30-day chart scan made that entry point as expensive as
/// get_spend. One day is enough for `today` and matches Codex/Claude
/// `--days 1` (plus the scanner's usual one-day timezone padding).
const STATUS_TODAY_SPEND_DAYS: u32 = 1;

fn spend_payload(provider: Option<&str>, enabled: &[ProviderId]) -> serde_json::Value {
    let targets: Vec<&str> = match provider {
        Some(name) => {
            let cli = ProviderId::from_cli_name(name)
                .map(|id| id.cli_name())
                .unwrap_or(name);
            // Explicit `provider=` matches CLI `--provider`: scan even if disabled.
            if !can_scan_local_spend(cli) {
                return json!({
                    "ok": false,
                    "error": format!(
                        "Local spend scanning is only available for Codex, Claude, and Grok (got '{cli}')"
                    ),
                    "providers": []
                });
            }
            vec![cli]
        }
        None => local_spend_cli_names(enabled),
    };

    let mut providers = Vec::new();
    for cli in targets {
        match get_cost_usage_report(cli, GET_SPEND_DAYS) {
            Some(report) => providers.push(json!({
                "provider": cli,
                "supported": true,
                "note": "Estimated API-rate value from local logs, not a bill.",
                "today": summary_json(&report.today),
                "seven_days": summary_json(&report.seven_days),
                "thirty_days": summary_json(&report.thirty_days),
            })),
            None => providers.push(json!({
                "provider": cli,
                "supported": false,
                "error": "Local cost scanning not available for this provider",
            })),
        }
    }

    json!({
        "ok": true,
        "source": "local-logs",
        "providers": providers,
    })
}

fn summary_json(summary: &CostSummary) -> serde_json::Value {
    let total_tokens = summary.input_tokens
        + summary.output_tokens
        + summary.cache_read_tokens
        + summary.cache_write_tokens;
    json!({
        "total_usd": summary.total_cost_usd,
        "input_tokens": summary.input_tokens,
        "output_tokens": summary.output_tokens,
        "cache_read_tokens": summary.cache_read_tokens,
        "cache_write_tokens": summary.cache_write_tokens,
        "total_tokens": total_tokens,
        "sessions_count": summary.sessions_count,
        "by_model": summary.by_model,
        "unknown_models": summary.unknown_models.iter().cloned().collect::<Vec<_>>(),
        "has_data": summary.sessions_count > 0
            || summary.total_cost_usd > 0.0
            || total_tokens > 0,
    })
}

fn status_payload(
    snapshot: Option<&WidgetSnapshot>,
    provider: Option<&str>,
    enabled: &[ProviderId],
    include_identity: bool,
) -> serde_json::Value {
    status_payload_with_spend(
        snapshot,
        provider,
        enabled,
        include_identity,
        today_spend_summary,
    )
}

fn status_payload_with_spend(
    snapshot: Option<&WidgetSnapshot>,
    provider: Option<&str>,
    enabled: &[ProviderId],
    include_identity: bool,
    mut spend_for: impl FnMut(&str) -> Option<serde_json::Value>,
) -> serde_json::Value {
    if let Some(name) = provider
        && ProviderId::from_cli_name(name).is_none()
    {
        return json!({
            "ok": false,
            "error": format!("Unknown provider '{name}'"),
        });
    }

    let chosen = choose_status_provider(snapshot, provider, enabled);
    let chosen_entry = match (&chosen, snapshot) {
        (Some(id), Some(snap)) => snap.entry_for(*id),
        _ => None,
    };
    let usage = chosen_entry.map(|entry| entry_usage_json(entry, include_identity));

    let spend = chosen.and_then(|id| {
        let cli = id.cli_name();
        let allowed = if provider.is_some() {
            can_scan_local_spend(cli)
        } else {
            local_spend_supported(cli, enabled)
        };
        if !allowed {
            return None;
        }
        spend_for(cli)
    });

    // SBS-1055 / SBS-1076: remaining_percent is the advertised cap-check sink.
    // Copying only usage.primary hid an exhausted Claude/Codex Weekly. Generic
    // window_outranks then let Cursor Plan outrank Auto. Use the same ranking
    // as the desktop strip, including cursor-api / on-demand extras.
    let remaining = chosen_entry
        .and_then(WidgetProviderEntry::constraining_rate_window)
        .map(RateWindow::remaining_percent);

    json!({
        "ok": usage.is_some() || spend.is_some(),
        "provider": chosen.map(|id| id.cli_name()),
        "remaining_percent": remaining,
        "usage": usage,
        "today_spend": spend,
        "hint": if snapshot.is_none() {
            Some("Open Ceiling desktop to refresh quota snapshot.")
        } else {
            None
        },
    })
}

fn today_spend_summary(cli: &str) -> Option<serde_json::Value> {
    get_cost_usage_report(cli, STATUS_TODAY_SPEND_DAYS).map(|report| summary_json(&report.today))
}

fn choose_status_provider(
    snapshot: Option<&WidgetSnapshot>,
    provider: Option<&str>,
    enabled: &[ProviderId],
) -> Option<ProviderId> {
    if let Some(name) = provider {
        return ProviderId::from_cli_name(name);
    }
    let snapshot = snapshot?;
    for preferred in [ProviderId::Claude, ProviderId::Codex] {
        if enabled.contains(&preferred) && snapshot.entry_for(preferred).is_some() {
            return Some(preferred);
        }
    }
    snapshot
        .entries
        .iter()
        .find(|e| enabled.contains(&e.provider))
        .map(|e| e.provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{NamedRateWindow, RateWindow, TokenUsageSummary, WindowAmount};
    use chrono::Utc;

    fn sample_snapshot() -> WidgetSnapshot {
        let entry = WidgetProviderEntry::new(ProviderId::Claude, Utc::now())
            .with_primary(RateWindow::with_details(
                42.0,
                Some(300),
                Some(Utc::now() + chrono::Duration::hours(3)),
                Some("in 3h".into()),
            ))
            .with_login_method("Claude Pro")
            .with_account_email("user@example.com")
            .with_token_usage(TokenUsageSummary::new().with_period(12.5, "Monthly"));
        WidgetSnapshot::new(vec![entry], Utc::now())
    }

    fn default_enabled() -> Vec<ProviderId> {
        Settings::default().get_enabled_provider_ids()
    }

    #[test]
    fn list_providers_marks_snapshot_and_spend_support() {
        let payload = list_providers_payload(Some(&sample_snapshot()), &default_enabled());
        assert_eq!(payload["snapshot_present"], true);
        let providers = payload["providers"].as_array().unwrap();
        let claude = providers
            .iter()
            .find(|p| p["id"] == "claude")
            .expect("claude");
        assert_eq!(claude["has_quota_snapshot"], true);
        assert_eq!(claude["local_spend_supported"], true);
        let cursor = providers
            .iter()
            .find(|p| p["id"] == "cursor")
            .expect("cursor");
        assert_eq!(cursor["local_spend_supported"], false);
    }

    #[test]
    fn usage_requires_snapshot() {
        let payload = usage_payload(None, None, false);
        assert_eq!(payload["ok"], false);
    }

    #[test]
    fn usage_filters_provider() {
        let snap = sample_snapshot();
        let payload = usage_payload(Some(&snap), Some("claude"), false);
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["providers"].as_array().unwrap().len(), 1);
        assert_eq!(payload["providers"][0]["primary"]["used_percent"], 42.0);
        assert_eq!(
            payload["providers"][0]["primary"]["remaining_percent"],
            58.0
        );
        assert!(payload["providers"][0]["account_email"].is_null());
        assert!(payload["providers"][0]["login_method"].is_null());
    }

    #[test]
    fn usage_includes_identity_only_when_requested() {
        let snap = sample_snapshot();
        let redacted = usage_payload(Some(&snap), Some("claude"), false);
        assert!(redacted["providers"][0]["account_email"].is_null());
        assert!(redacted["providers"][0]["login_method"].is_null());
        let full = usage_payload(Some(&snap), Some("claude"), true);
        assert_eq!(full["providers"][0]["account_email"], "user@example.com");
        assert_eq!(full["providers"][0]["login_method"], "Claude Pro");
    }

    /// SBS-1031: billed/period CostSnapshot.used must not be labeled session spend.
    #[test]
    fn usage_and_status_name_period_cost_not_session() {
        let snap = sample_snapshot();
        let usage = usage_payload(Some(&snap), Some("claude"), false);
        let provider = &usage["providers"][0];
        assert_eq!(provider["period_cost_usd"], 12.5);
        assert_eq!(provider["cost_period"], "Monthly");
        assert!(
            provider.get("session_cost_usd").is_none(),
            "legacy session_cost_usd must not appear: {provider}"
        );

        let status = status_payload(Some(&snap), Some("claude"), &default_enabled(), false);
        assert_eq!(status["usage"]["period_cost_usd"], 12.5);
        assert_eq!(status["usage"]["cost_period"], "Monthly");
        assert!(
            status["usage"].get("session_cost_usd").is_none(),
            "legacy session_cost_usd must not appear: {}",
            status["usage"]
        );
    }

    #[test]
    fn spend_rejects_unsupported_provider() {
        let payload = spend_payload(Some("cursor"), &default_enabled());
        assert_eq!(payload["ok"], false);
        assert!(
            payload["error"]
                .as_str()
                .unwrap_or_default()
                .contains("Grok"),
            "error must name Grok: {payload}"
        );
    }

    /// SBS-934: get_cost_usage_report already scans Grok; MCP still advertised
    /// Codex/Claude only and rejected `provider=grok`.
    #[test]
    fn list_providers_and_spend_include_grok() {
        let payload = list_providers_payload(None, &default_enabled());
        let grok = payload["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "grok")
            .expect("grok");
        assert_eq!(grok["local_spend_supported"], true);
        let spend = spend_payload(Some("grok"), &default_enabled());
        assert_eq!(spend["ok"], true);
        assert_eq!(spend["providers"][0]["provider"], "grok");
        assert_eq!(spend["providers"][0]["supported"], true);
    }

    fn status_without_spend_scan(
        snapshot: Option<&WidgetSnapshot>,
        provider: Option<&str>,
        enabled: &[ProviderId],
    ) -> serde_json::Value {
        status_payload_with_spend(snapshot, provider, enabled, false, |_| None)
    }

    #[test]
    fn status_rejects_unknown_provider() {
        let payload = status_without_spend_scan(
            Some(&sample_snapshot()),
            Some("not-a-provider"),
            &default_enabled(),
        );
        assert_eq!(payload["ok"], false);
        assert!(
            payload["error"]
                .as_str()
                .unwrap_or_default()
                .contains("Unknown provider"),
            "payload: {payload}"
        );
    }

    #[test]
    fn status_prefers_claude_when_present() {
        let snap = sample_snapshot();
        let payload = status_without_spend_scan(Some(&snap), None, &default_enabled());
        assert_eq!(payload["provider"], "claude");
        assert_eq!(payload["remaining_percent"], 58.0);
    }

    #[test]
    fn spend_and_list_honor_enabled_providers() {
        let enabled = vec![ProviderId::Claude, ProviderId::Codex];
        let listed = list_providers_payload(None, &enabled);
        let grok = listed["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "grok")
            .expect("grok");
        assert_eq!(grok["local_spend_supported"], false);

        let spend = spend_payload(None, &enabled);
        let names: Vec<&str> = spend["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["provider"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["claude", "codex"]);

        let explicit = spend_payload(Some("grok"), &enabled);
        assert_eq!(explicit["ok"], true);
        assert_eq!(explicit["providers"][0]["provider"], "grok");
    }

    #[test]
    fn status_skips_disabled_snapshot_providers() {
        let snap = sample_snapshot();
        let payload = status_without_spend_scan(Some(&snap), None, &[ProviderId::Codex]);
        assert!(payload["provider"].is_null(), "payload: {payload}");
        assert!(payload["usage"].is_null(), "payload: {payload}");

        let explicit = status_without_spend_scan(Some(&snap), Some("claude"), &[ProviderId::Codex]);
        assert_eq!(explicit["provider"], "claude");
        assert_eq!(explicit["remaining_percent"], 58.0);
    }

    /// SBS-1033: get_status is advertised as a cheap cap check. A 30-day
    /// local-log walk just to fill `today_spend` made it as expensive as
    /// get_spend. Today's dollars only need one inclusive local day.
    #[test]
    fn status_today_spend_uses_one_day_window_not_thirty() {
        assert_eq!(STATUS_TODAY_SPEND_DAYS, 1);
        assert_eq!(GET_SPEND_DAYS, 30);
        assert_ne!(
            STATUS_TODAY_SPEND_DAYS, GET_SPEND_DAYS,
            "get_status must not reuse get_spend's 30-day scan window"
        );
    }

    #[test]
    fn status_includes_today_spend_from_one_day_lookup() {
        let snap = sample_snapshot();
        let mut scanned = Vec::new();
        let payload = status_payload_with_spend(
            Some(&snap),
            Some("claude"),
            &default_enabled(),
            false,
            |cli| {
                scanned.push(cli.to_string());
                Some(json!({
                    "total_usd": 1.25,
                    "has_data": true,
                }))
            },
        );
        assert_eq!(scanned, vec!["claude"]);
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["today_spend"]["total_usd"], 1.25);
        assert_eq!(payload["today_spend"]["has_data"], true);
    }

    #[test]
    fn status_skips_today_spend_for_providers_without_local_logs() {
        let snap = sample_snapshot();
        let mut scanned = 0u32;
        let payload = status_payload_with_spend(
            Some(&snap),
            Some("cursor"),
            &default_enabled(),
            false,
            |_| {
                scanned += 1;
                Some(json!({ "total_usd": 9.99 }))
            },
        );
        assert_eq!(scanned, 0);
        assert!(payload["today_spend"].is_null(), "payload: {payload}");
    }

    fn dual_window_snapshot(
        provider: ProviderId,
        primary_used: f64,
        secondary_used: f64,
    ) -> WidgetSnapshot {
        let entry = WidgetProviderEntry::new(provider, Utc::now())
            .with_primary(RateWindow::new(primary_used))
            .with_secondary(RateWindow::new(secondary_used));
        WidgetSnapshot::new(vec![entry], Utc::now())
    }

    /// SBS-1055: get_status remaining_percent copied only usage.primary, so a
    /// healthy 5-hour session hid an exhausted Claude/Codex Weekly.
    #[test]
    fn status_remaining_surfaces_exhausted_weekly_over_healthy_session() {
        for provider in [ProviderId::Claude, ProviderId::Codex] {
            let snap = dual_window_snapshot(provider, 42.0, 100.0);
            let payload = status_without_spend_scan(
                Some(&snap),
                Some(provider.cli_name()),
                &default_enabled(),
            );
            assert_eq!(
                payload["remaining_percent"],
                0.0,
                "exhausted Weekly must bind remaining_percent for {}: {payload}",
                provider.cli_name()
            );
            assert_eq!(payload["usage"]["primary"]["remaining_percent"], 58.0);
            assert_eq!(payload["usage"]["secondary"]["remaining_percent"], 0.0);
            assert_eq!(payload["usage"]["secondary"]["is_exhausted"], true);
        }
    }

    #[test]
    fn status_remaining_uses_hotter_weekly_when_session_is_fresher() {
        let snap = dual_window_snapshot(ProviderId::Claude, 34.0, 91.0);
        let payload = status_without_spend_scan(Some(&snap), Some("claude"), &default_enabled());
        assert_eq!(payload["remaining_percent"], 9.0);
    }

    #[test]
    fn status_remaining_keeps_hotter_session_over_quiet_weekly() {
        let snap = dual_window_snapshot(ProviderId::Claude, 92.0, 40.0);
        let payload = status_without_spend_scan(Some(&snap), Some("claude"), &default_enabled());
        assert_eq!(payload["remaining_percent"], 8.0);
    }

    #[test]
    fn status_remaining_surfaces_exhausted_tertiary_over_healthy_primary() {
        let entry = WidgetProviderEntry::new(ProviderId::Claude, Utc::now())
            .with_primary(RateWindow::new(10.0))
            .with_tertiary(RateWindow::new(100.0));
        let snap = WidgetSnapshot::new(vec![entry], Utc::now());
        let payload = status_without_spend_scan(Some(&snap), Some("claude"), &default_enabled());
        assert_eq!(payload["remaining_percent"], 0.0);
    }

    #[test]
    fn constraining_window_breaks_used_ties_by_soonest_reset() {
        let soon = Utc::now() + chrono::Duration::hours(1);
        let later = Utc::now() + chrono::Duration::hours(24);
        let entry = WidgetProviderEntry::new(ProviderId::Claude, Utc::now())
            .with_primary(RateWindow::with_details(50.0, Some(300), Some(later), None))
            .with_secondary(RateWindow::with_details(
                50.0,
                Some(10_080),
                Some(soon),
                None,
            ));
        let window = entry.constraining_rate_window().expect("window");
        assert_eq!(window.window_minutes, Some(10_080));
        assert_eq!(window.remaining_percent(), 50.0);
    }

    fn cursor_entry(
        plan: f64,
        auto: Option<f64>,
        extras: Vec<NamedRateWindow>,
    ) -> WidgetProviderEntry {
        let mut entry = WidgetProviderEntry::new(ProviderId::Cursor, Utc::now())
            .with_primary(RateWindow::new(plan));
        if let Some(auto) = auto {
            entry = entry.with_secondary(RateWindow::new(auto));
        }
        entry.with_extra_rate_windows(extras)
    }

    /// SBS-1076: generic window_outranks picks Plan 95% over Auto 55%.
    #[test]
    fn status_remaining_prefers_cursor_auto_over_hotter_plan() {
        let snap = WidgetSnapshot::new(vec![cursor_entry(95.0, Some(55.0), vec![])], Utc::now());
        let payload = status_without_spend_scan(Some(&snap), Some("cursor"), &default_enabled());
        assert_eq!(
            payload["remaining_percent"], 45.0,
            "Auto must bind remaining_percent, not hotter Plan: {payload}"
        );
        assert_eq!(payload["usage"]["primary"]["remaining_percent"], 5.0);
        assert_eq!(payload["usage"]["secondary"]["remaining_percent"], 45.0);
    }

    /// SBS-1076: exhausted-first ranking would hide API room behind Auto 100%.
    #[test]
    fn status_remaining_prefers_cursor_api_with_room_over_exhausted_auto() {
        let extras = vec![NamedRateWindow::new(
            "cursor-api",
            "API",
            RateWindow::new(40.0),
        )];
        let snap = WidgetSnapshot::new(vec![cursor_entry(40.0, Some(100.0), extras)], Utc::now());
        let payload = status_without_spend_scan(Some(&snap), Some("cursor"), &default_enabled());
        assert_eq!(
            payload["remaining_percent"], 60.0,
            "API with room must bind remaining_percent: {payload}"
        );
        assert_eq!(
            payload["usage"]["extra_rate_windows"][0]["id"],
            "cursor-api"
        );
        assert_eq!(
            payload["usage"]["extra_rate_windows"][0]["remaining_percent"],
            60.0
        );
    }

    /// SBS-1076: on-demand is the strip window once included lanes are gone.
    #[test]
    fn status_remaining_surfaces_cursor_on_demand_after_included_exhausted() {
        let extras = vec![
            NamedRateWindow::new("cursor-api", "API", RateWindow::new(100.0)),
            NamedRateWindow::new("cursor-on-demand", "On-demand", RateWindow::new(56.0))
                .with_amount(WindowAmount::new(1_002.16, "USD").with_limit(1_800.0)),
        ];
        let snap = WidgetSnapshot::new(vec![cursor_entry(100.0, Some(100.0), extras)], Utc::now());
        let payload = status_without_spend_scan(Some(&snap), Some("cursor"), &default_enabled());
        assert_eq!(payload["remaining_percent"], 44.0, "payload: {payload}");
        let on_demand = payload["usage"]["extra_rate_windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["id"] == "cursor-on-demand")
            .expect("on-demand extra");
        assert_eq!(on_demand["remaining_percent"], 44.0);
        assert_eq!(on_demand["amount"]["used"], 1002.16);
        assert_eq!(on_demand["amount"]["limit"], 1800.0);
    }

    #[test]
    fn usage_includes_cursor_api_and_on_demand_extras() {
        let extras = vec![
            NamedRateWindow::new("cursor-api", "API", RateWindow::new(12.0)),
            NamedRateWindow::new("cursor-on-demand", "On-demand", RateWindow::new(0.0))
                .with_amount(WindowAmount::new(0.0, "USD").with_limit(1_800.0)),
        ];
        let snap = WidgetSnapshot::new(vec![cursor_entry(40.0, Some(20.0), extras)], Utc::now());
        let payload = usage_payload(Some(&snap), Some("cursor"), false);
        let extras = payload["providers"][0]["extra_rate_windows"]
            .as_array()
            .expect("extras");
        let ids: Vec<&str> = extras.iter().map(|w| w["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["cursor-api", "cursor-on-demand"]);
    }

    #[test]
    fn status_remaining_is_null_when_no_measured_window_exists() {
        let entry = WidgetProviderEntry::new(ProviderId::Claude, Utc::now());
        let snap = WidgetSnapshot::new(vec![entry], Utc::now());
        let payload = status_without_spend_scan(Some(&snap), Some("claude"), &default_enabled());

        assert!(payload["remaining_percent"].is_null());
    }
}
