//! `ceiling statusline` — a fast, cache-only usage line for editor status bars
//! (Claude Code's `statusLine`, and similar).
//!
//! It reads the widget snapshot the desktop app already persists and never
//! fetches or scans, so it stays far under a per-render budget. A status line
//! must never fail its host, so every path prints a short line and exits 0.

use clap::Args;
use std::io::{IsTerminal, Read};

use crate::core::{
    ProviderId, WidgetProviderEntry, WidgetSelectionStore, WidgetSnapshot, WidgetSnapshotStore,
};

#[derive(Args, Debug, Clone, Default)]
pub struct StatuslineArgs {
    /// Provider to show. Defaults to the model the editor pipes in, then the
    /// app's selected provider, then Claude.
    #[arg(short, long)]
    pub provider: Option<String>,

    /// Hide the estimated-spend segment.
    #[arg(long = "no-cost")]
    pub no_cost: bool,
}

pub async fn run(args: StatuslineArgs) -> anyhow::Result<()> {
    println!(
        "{}",
        build_line(&args, stdin_model(), WidgetSnapshotStore::load())
    );
    Ok(())
}

/// The model id the editor pipes on stdin (Claude Code sends a JSON blob).
/// `None` when stdin is a terminal (manual run) or carries no usable model.
fn stdin_model() -> Option<String> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).ok()?;
    model_from_json(raw.trim())
}

fn model_from_json(raw: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    value
        .get("model")
        .and_then(|model| model.get("id").or_else(|| model.get("display_name")))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn model_to_provider(model: &str) -> Option<ProviderId> {
    let m = model.to_ascii_lowercase();
    if m.contains("claude") || m.contains("sonnet") || m.contains("opus") || m.contains("haiku") {
        Some(ProviderId::Claude)
    } else if m.contains("codex") || m.contains("gpt") || m.contains("o3") || m.contains("o4") {
        Some(ProviderId::Codex)
    } else if m.contains("gemini") {
        Some(ProviderId::Gemini)
    } else if m.contains("grok") {
        Some(ProviderId::Grok)
    } else {
        None
    }
}

/// Pick the provider to display: explicit flag, then the piped model, then the
/// app's selected provider, then Claude, then whatever the snapshot has first.
fn choose_provider(
    args: &StatuslineArgs,
    stdin_model: Option<String>,
    snapshot: &WidgetSnapshot,
) -> Option<ProviderId> {
    let has = |p: ProviderId| snapshot.entry_for(p).is_some();

    if let Some(p) = args.provider.as_deref().and_then(ProviderId::from_cli_name) {
        return Some(p);
    }
    if let Some(p) = stdin_model
        .as_deref()
        .and_then(model_to_provider)
        .filter(|p| has(*p))
    {
        return Some(p);
    }
    if let Some(p) = WidgetSelectionStore::load_selected_provider().filter(|p| has(*p)) {
        return Some(p);
    }
    if has(ProviderId::Claude) {
        return Some(ProviderId::Claude);
    }
    snapshot.entries.first().map(|entry| entry.provider)
}

fn build_line(
    args: &StatuslineArgs,
    stdin_model: Option<String>,
    snapshot: Option<WidgetSnapshot>,
) -> String {
    let Some(snapshot) = snapshot else {
        return "Ceiling: open the app".to_string();
    };
    match choose_provider(args, stdin_model, &snapshot).and_then(|p| snapshot.entry_for(p)) {
        Some(entry) => format_entry(entry, !args.no_cost),
        None => "Ceiling: no data".to_string(),
    }
}

/// Compose the one-line status: `Claude 17% · resets 3h 59m · $2.30`.
fn format_entry(entry: &WidgetProviderEntry, show_cost: bool) -> String {
    let name = entry.provider.display_name();
    let mut parts: Vec<String> = Vec::new();

    match &entry.primary {
        Some(primary) => {
            let mut segment = format!("{name} {:.0}%", primary.used_percent);
            if let Some(reset) = primary.format_countdown() {
                segment.push_str(&format!(" · resets {reset}"));
            }
            parts.push(segment);
        }
        None => parts.push(name.to_string()),
    }

    if show_cost
        && let Some(cost) = entry
            .token_usage
            .as_ref()
            .and_then(|usage| usage.period_cost_usd)
            .filter(|cost| *cost > 0.0)
    {
        parts.push(format!("${cost:.2}"));
    }

    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{RateWindow, TokenUsageSummary};
    use chrono::Utc;

    fn entry(provider: ProviderId, primary: Option<RateWindow>) -> WidgetProviderEntry {
        WidgetProviderEntry {
            provider,
            updated_at: Utc::now(),
            primary,
            secondary: None,
            tertiary: None,
            extra_rate_windows: Vec::new(),
            credits_remaining: None,
            code_review_remaining_percent: None,
            token_usage: None,
            daily_usage: Vec::new(),
            account_email: None,
            login_method: None,
        }
    }

    fn window(used_percent: f64, resets_at: Option<chrono::DateTime<Utc>>) -> RateWindow {
        RateWindow {
            used_percent,
            window_minutes: Some(300),
            resets_at,
            reset_description: None,
        }
    }

    #[test]
    fn formats_provider_percent_without_reset() {
        let e = entry(ProviderId::Claude, Some(window(17.4, None)));
        assert_eq!(format_entry(&e, true), "Claude 17%");
    }

    #[test]
    fn includes_reset_countdown_and_cost() {
        let mut e = entry(
            ProviderId::Claude,
            Some(window(50.0, Some(Utc::now() + chrono::Duration::hours(2)))),
        );
        e.token_usage = Some(TokenUsageSummary::new().with_session(2.3, 1000));
        let line = format_entry(&e, true);
        assert!(line.starts_with("Claude 50% · resets "), "got: {line}");
        assert!(line.ends_with(" · $2.30"), "got: {line}");
    }

    #[test]
    fn hides_cost_when_disabled_or_zero() {
        let mut e = entry(ProviderId::Codex, Some(window(80.0, None)));
        e.token_usage = Some(TokenUsageSummary::new().with_session(5.0, 100));
        assert_eq!(format_entry(&e, false), "Codex 80%");

        let mut zero = entry(ProviderId::Codex, Some(window(80.0, None)));
        zero.token_usage = Some(TokenUsageSummary::new().with_session(0.0, 0));
        assert_eq!(format_entry(&zero, true), "Codex 80%");
    }

    #[test]
    fn build_line_falls_back_without_snapshot() {
        assert_eq!(
            build_line(&StatuslineArgs::default(), None, None),
            "Ceiling: open the app"
        );
    }

    #[test]
    fn choose_prefers_explicit_provider_then_model() {
        let snap = WidgetSnapshot::new(
            vec![
                entry(ProviderId::Codex, Some(window(10.0, None))),
                entry(ProviderId::Claude, Some(window(20.0, None))),
            ],
            Utc::now(),
        );
        // Explicit flag wins.
        let args = StatuslineArgs {
            provider: Some("codex".to_string()),
            no_cost: false,
        };
        assert_eq!(choose_provider(&args, None, &snap), Some(ProviderId::Codex));
        // Otherwise the piped model decides.
        assert_eq!(
            choose_provider(
                &StatuslineArgs::default(),
                Some("claude-sonnet-5".to_string()),
                &snap
            ),
            Some(ProviderId::Claude)
        );
    }

    #[test]
    fn model_from_json_reads_claude_code_shape() {
        let raw = r#"{"model":{"id":"claude-sonnet-5","display_name":"Sonnet"},"cwd":"/x"}"#;
        assert_eq!(model_from_json(raw).as_deref(), Some("claude-sonnet-5"));
        assert_eq!(
            model_to_provider("claude-sonnet-5"),
            Some(ProviderId::Claude)
        );
    }
}
