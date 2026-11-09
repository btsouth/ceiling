//! Usage command implementation

use clap::Args;
use futures::stream::{self, StreamExt};
use serde::Serialize;

use crate::core::{
    AccountTarget, ConfiguredAccounts, CostSnapshot, FetchContext, ProviderFetchResult, ProviderId,
    RateWindow, SourceMode, UsagePace, UsageSnapshot, instantiate_provider,
};
use crate::settings::Settings;
use crate::status::{ProviderStatus as StatusInfo, StatusLevel, fetch_provider_status};

pub const PROVIDER_ARG_HELP: &str = "Provider to query (for example: codex, claude, gemini, antigravity/agy, nanogpt, deepseek, codebuff, windsurf, all, both)";
const MAX_CONCURRENT_ACCOUNT_FETCHES: usize = 4;

/// Arguments for the usage command
#[derive(Args, Debug, Default)]
pub struct UsageArgs {
    #[arg(short, long, help = PROVIDER_ARG_HELP)]
    pub provider: Option<String>,

    /// Output format: text or json
    #[arg(short, long, default_value = "text")]
    pub format: OutputFormat,

    /// Shorthand for --format json
    #[arg(long)]
    pub json: bool,

    /// Skip credits line in output
    #[arg(long = "no-credits")]
    pub no_credits: bool,

    /// Disable ANSI colors in text output
    #[arg(long = "no-color")]
    pub no_color: bool,

    /// Pretty-print JSON output
    #[arg(long)]
    pub pretty: bool,

    /// Fetch and include provider status pages
    #[arg(long)]
    pub status: bool,

    /// Fetch all token accounts where supported
    #[arg(long = "all-accounts")]
    pub all_accounts: bool,

    /// Data source: auto, oauth, web, cli
    #[arg(long, default_value = "auto", value_parser = ["auto", "web", "cli", "oauth"])]
    pub source: String,

    /// Web fetch timeout in seconds
    #[arg(long = "web-timeout", default_value = "60")]
    pub web_timeout: u64,

    /// Save HTML snapshots to temp dir when data is missing (debug)
    #[arg(long = "web-debug-dump-html")]
    pub web_debug_dump_html: bool,

    /// Send Antigravity planInfo fields to stderr (debug)
    #[arg(long = "antigravity-plan-debug")]
    pub antigravity_plan_debug: bool,

    /// Print one compact line per provider
    #[arg(long)]
    pub brief: bool,
}

/// Output format enum
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

impl std::str::FromStr for OutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "text" => Ok(OutputFormat::Text),
            "json" => Ok(OutputFormat::Json),
            _ => Err(format!("Invalid format: {}. Use 'text' or 'json'", s)),
        }
    }
}

/// Provider selection from CLI args
#[derive(Debug, Clone)]
pub enum ProviderSelection {
    Single(ProviderId),
    Both,
    All,
    /// Providers enabled in Settings (the documented default).
    Enabled,
}

impl ProviderSelection {
    pub fn from_arg(arg: Option<&str>) -> anyhow::Result<Self> {
        match arg.map(|s| s.to_lowercase()).as_deref() {
            Some("all") => Ok(ProviderSelection::All),
            Some("both") => Ok(ProviderSelection::Both),
            Some(name) => {
                if let Some(id) = ProviderId::from_cli_name(name) {
                    Ok(ProviderSelection::Single(id))
                } else {
                    anyhow::bail!(
                        "Unknown provider: '{}'. Use --help to see available providers.",
                        name
                    )
                }
            }
            None => Ok(ProviderSelection::Enabled),
        }
    }

    pub fn as_list(&self) -> Vec<ProviderId> {
        self.as_list_from(&Settings::load())
    }

    /// Resolve providers against an explicit Settings snapshot.
    pub fn as_list_from(&self, settings: &Settings) -> Vec<ProviderId> {
        match self {
            ProviderSelection::Single(id) => vec![*id],
            ProviderSelection::Both => vec![ProviderId::Codex, ProviderId::Claude],
            ProviderSelection::All => ProviderId::all().to_vec(),
            ProviderSelection::Enabled => settings.get_enabled_provider_ids(),
        }
    }

    /// Like [`Self::as_list_from`], but the default (enabled) selection fails
    /// instead of resolving to an empty list that looks like success.
    pub fn resolved_ids(&self, settings: &Settings) -> anyhow::Result<Vec<ProviderId>> {
        let ids = self.as_list_from(settings);
        if matches!(self, Self::Enabled) && ids.is_empty() {
            anyhow::bail!(NO_ENABLED_PROVIDERS_ERROR);
        }
        Ok(ids)
    }
}

pub const NO_ENABLED_PROVIDERS_ERROR: &str =
    "No providers are enabled. Enable a provider in Preferences, or pass --provider.";

/// JSON output payload
#[derive(Debug, Serialize)]
pub struct ProviderPayload {
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub source: String,
    #[serde(flatten)]
    pub result: ProviderFetchResult,
}

/// Error payload for JSON output
#[derive(Debug, Serialize)]
struct ErrorPayload {
    provider: String,
    error: String,
}

/// Run the usage command
pub async fn run(args: UsageArgs) -> anyhow::Result<()> {
    let command = UsageCommand::from_args(args)?;
    command.log();
    let output = collect_usage_output(&command).await;
    print_usage_output(output)
}

struct UsageCommand {
    format: OutputFormat,
    providers: Vec<ProviderId>,
    use_color: bool,
    brief: bool,
    fetch_status: bool,
    pretty: bool,
    ctx: FetchContext,
    accounts: ConfiguredAccounts,
    all_accounts: bool,
}

impl UsageCommand {
    fn from_args(args: UsageArgs) -> anyhow::Result<Self> {
        let format = effective_format(&args);
        let source_mode = SourceMode::parse(&args.source).unwrap_or(SourceMode::Auto);
        let settings = Settings::load();
        let providers =
            ProviderSelection::from_arg(args.provider.as_deref())?.resolved_ids(&settings)?;

        Ok(Self {
            format,
            providers,
            use_color: !args.no_color && is_terminal(),
            brief: args.brief,
            fetch_status: args.status,
            pretty: args.pretty,
            ctx: build_usage_fetch_context(&args, source_mode),
            accounts: ConfiguredAccounts::load(),
            all_accounts: args.all_accounts,
        })
    }

    fn log(&self) {
        tracing::debug!(
            "Running usage command: providers={:?}, format={:?}, source={:?}, status={}",
            self.providers,
            self.format,
            self.ctx.source_mode,
            self.fetch_status
        );
    }
}

fn effective_format(args: &UsageArgs) -> OutputFormat {
    if args.json {
        OutputFormat::Json
    } else {
        args.format
    }
}

fn build_usage_fetch_context(args: &UsageArgs, source_mode: SourceMode) -> FetchContext {
    FetchContext {
        source_mode,
        include_credits: !args.no_credits,
        web_timeout: args.web_timeout,
        verbose: false,
        manual_cookie_header: None,
        api_key: None,
        workspace_id: None,
        api_region: None,
        gateway_url: None,
        account_config_dir: None,
    }
}

enum UsageOutput {
    Text(Vec<String>),
    Json {
        results: Vec<serde_json::Value>,
        pretty: bool,
    },
}

#[derive(Clone)]
struct UsageTarget {
    provider: ProviderId,
    account: Option<AccountTarget>,
}

fn usage_targets(command: &UsageCommand) -> Vec<UsageTarget> {
    command
        .providers
        .iter()
        .flat_map(|provider| {
            let accounts = if command.all_accounts {
                command.accounts.targets_for(*provider)
            } else {
                Vec::new()
            };
            if accounts.is_empty() {
                vec![UsageTarget {
                    provider: *provider,
                    account: None,
                }]
            } else {
                accounts
                    .into_iter()
                    .map(|account| UsageTarget {
                        provider: *provider,
                        account: Some(account),
                    })
                    .collect()
            }
        })
        .collect()
}

async fn collect_usage_output(command: &UsageCommand) -> UsageOutput {
    let targets = usage_targets(command);
    match command.format {
        OutputFormat::Text => {
            let mut sections = stream::iter(targets.into_iter().enumerate())
                .map(|(index, target)| async move {
                    (index, fetch_provider_text_output(&target, command).await)
                })
                .buffer_unordered(MAX_CONCURRENT_ACCOUNT_FETCHES)
                .collect::<Vec<_>>()
                .await;
            sections.sort_by_key(|(index, _)| *index);
            UsageOutput::Text(sections.into_iter().map(|(_, section)| section).collect())
        }
        OutputFormat::Json => {
            let mut results = stream::iter(targets.into_iter().enumerate())
                .map(|(index, target)| async move {
                    (index, fetch_provider_json_output(&target, command).await)
                })
                .buffer_unordered(MAX_CONCURRENT_ACCOUNT_FETCHES)
                .collect::<Vec<_>>()
                .await;
            results.sort_by_key(|(index, _)| *index);
            UsageOutput::Json {
                results: results.into_iter().map(|(_, result)| result).collect(),
                pretty: command.pretty,
            }
        }
    }
}

async fn fetch_provider_text_output(target: &UsageTarget, command: &UsageCommand) -> String {
    let provider_id = target.provider;
    let output = match fetch_provider_result(target, command).await {
        Ok((result, status)) => {
            if command.brief {
                render_brief_text(provider_id, &result)
            } else {
                render_text_with_status(provider_id, &result, status.as_ref(), command.use_color)
            }
        }
        Err(e) => render_text_error(provider_id, &e.to_string(), command.use_color),
    };
    annotate_text_account(output, target.account.as_ref(), command.brief)
}

async fn fetch_provider_json_output(
    target: &UsageTarget,
    command: &UsageCommand,
) -> serde_json::Value {
    let provider_id = target.provider;
    let mut output = match fetch_provider_result(target, command).await {
        Ok((result, status)) => render_json_result(provider_id, result, status.as_ref()),
        Err(e) => serde_json::json!({
            "provider": provider_id.cli_name(),
            "error": e.to_string(),
        }),
    };
    annotate_json_account(&mut output, target.account.as_ref());
    output
}

async fn fetch_provider_result(
    target: &UsageTarget,
    command: &UsageCommand,
) -> anyhow::Result<(ProviderFetchResult, Option<StatusInfo>)> {
    let provider_id = target.provider;
    let provider = instantiate_provider(provider_id);
    let status_future = command
        .fetch_status
        .then(|| fetch_provider_status(provider_id.cli_name()));
    let ctx = match &target.account {
        Some(account) => command
            .ctx
            .clone()
            .pinned_to_account_dir(provider_id, account.config_dir.clone()),
        None => command
            .ctx
            .clone()
            .for_account(provider_id, &command.accounts),
    };
    let result = provider.fetch_usage(&ctx).await?;
    let status = if let Some(fut) = status_future {
        fut.await
    } else {
        None
    };
    Ok((result, status))
}

fn annotate_text_account(output: String, account: Option<&AccountTarget>, brief: bool) -> String {
    match account {
        Some(account) if brief => {
            format!("{output}, account {} ({})", account.label, account.id)
        }
        Some(account) => format!(
            "{output}\n  Configured account: {} ({})",
            account.label, account.id
        ),
        None => output,
    }
}

fn annotate_json_account(output: &mut serde_json::Value, account: Option<&AccountTarget>) {
    if let Some(account) = account {
        output["configured_account"] = serde_json::json!({
            "id": account.id,
            "label": account.label,
        });
    }
}

fn render_text_error(provider_id: ProviderId, error_msg: &str, use_color: bool) -> String {
    let header = if use_color {
        format!("\x1b[1m{}\x1b[0m", provider_id.display_name())
    } else {
        provider_id.display_name().to_string()
    };
    format!("{}  Error: {}", header, error_msg)
}

fn render_json_result(
    provider_id: ProviderId,
    result: ProviderFetchResult,
    status: Option<&StatusInfo>,
) -> serde_json::Value {
    let mut json_result = serde_json::json!({
        "provider": provider_id.cli_name(),
        "source": result.source_label,
        "usage": result.usage,
        "cost": result.cost,
    });

    if let Some(s) = status {
        json_result["status"] = serde_json::json!({
            "level": format!("{:?}", s.level).to_lowercase(),
            "description": s.description,
        });
    }

    json_result
}

fn print_usage_output(output: UsageOutput) -> anyhow::Result<()> {
    match output {
        UsageOutput::Text(sections) => {
            println!("{}", sections.join("\n\n"));
        }
        UsageOutput::Json { results, pretty } => {
            let output = if pretty {
                serde_json::to_string_pretty(&results)?
            } else {
                serde_json::to_string(&results)?
            };
            println!("{}", output);
        }
    }

    Ok(())
}

/// Check if stdout is a terminal
fn is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// Render usage as text with optional status
pub fn render_text_with_status(
    provider: ProviderId,
    result: &ProviderFetchResult,
    status: Option<&StatusInfo>,
    use_color: bool,
) -> String {
    let mut lines = Vec::new();
    let metadata = instantiate_provider(provider).metadata().clone();

    lines.push(render_usage_header(provider, result, status, use_color));
    append_status_line(&mut lines, status);
    if provider == ProviderId::DeepSeekHarness {
        lines.extend(result.usage.extra_rate_windows.iter().filter_map(|row| {
            row.window
                .reset_description
                .as_ref()
                .map(|value| format!("  {}: {value}", row.title))
        }));
        return lines.join("\n");
    }
    append_account_lines(&mut lines, &result.usage);
    append_usage_window_lines(&mut lines, &result.usage, &metadata, use_color);
    append_cost_line(&mut lines, result.cost.as_ref());

    lines.join("\n")
}

fn render_usage_header(
    provider: ProviderId,
    result: &ProviderFetchResult,
    status: Option<&StatusInfo>,
    use_color: bool,
) -> String {
    let status_indicator = render_status_indicator(status, use_color);
    if use_color {
        format!(
            "\x1b[1m{}\x1b[0m ({}){}",
            provider.display_name(),
            result.source_label,
            status_indicator
        )
    } else {
        format!(
            "{} ({}){}",
            provider.display_name(),
            result.source_label,
            status_indicator
        )
    }
}

fn render_status_indicator(status: Option<&StatusInfo>, use_color: bool) -> String {
    let Some(status) = status else {
        return String::new();
    };

    let (symbol, color) = match status.level {
        StatusLevel::Operational => ("●", "\x1b[32m"), // Green
        StatusLevel::Degraded => ("◐", "\x1b[33m"),    // Yellow
        StatusLevel::Partial => ("◑", "\x1b[33m"),     // Yellow
        StatusLevel::Major => ("○", "\x1b[31m"),       // Red
        StatusLevel::Unknown => ("?", "\x1b[90m"),     // Gray
    };

    if use_color {
        format!(" {}{}\x1b[0m", color, symbol)
    } else {
        format!(" {}", symbol)
    }
}

fn append_status_line(lines: &mut Vec<String>, status: Option<&StatusInfo>) {
    if let Some(s) = status
        && s.level != StatusLevel::Operational
        && s.level != StatusLevel::Unknown
    {
        lines.push(format!("  Status: {}", s.description));
    }
}

fn append_account_lines(lines: &mut Vec<String>, usage: &UsageSnapshot) {
    if let Some(ref email) = usage.account_email {
        lines.push(format!("  Account: {}", email));
    }
    if let Some(ref method) = usage.login_method {
        lines.push(format!("  Plan:    {}", method));
    }
}

fn append_usage_window_lines(
    lines: &mut Vec<String>,
    usage: &UsageSnapshot,
    metadata: &crate::core::ProviderMetadata,
    use_color: bool,
) {
    append_window_line(lines, metadata.session_label, &usage.primary, use_color);
    append_secondary_window_line(
        lines,
        usage.secondary.as_ref(),
        metadata.weekly_label,
        use_color,
    );
    append_model_specific_line(lines, usage.model_specific.as_ref(), use_color);
}

fn append_window_line(lines: &mut Vec<String>, label: &str, window: &RateWindow, use_color: bool) {
    let bar = render_progress_bar(window.used_percent, 20, use_color);
    let reset = window
        .format_countdown()
        .map(|c| format!(" (resets in {})", c))
        .unwrap_or_default();
    lines.push(format!(
        "  {:<8} {} {} used{}",
        format!("{}:", label),
        bar,
        format_percent(window.used_percent),
        reset
    ));
}

fn append_secondary_window_line(
    lines: &mut Vec<String>,
    secondary: Option<&RateWindow>,
    label: &str,
    use_color: bool,
) {
    if let Some(secondary) = secondary {
        append_window_line(lines, label, secondary, use_color);
        let window_minutes = secondary.window_minutes.unwrap_or(10080);
        if let Some(pace) = UsagePace::weekly(secondary, None, window_minutes) {
            lines.push(format!(
                "  Pace:    {} {}",
                pace.stage.emoji(),
                pace.format_status()
            ));
        }
    }
}

fn append_model_specific_line(
    lines: &mut Vec<String>,
    model_specific: Option<&RateWindow>,
    use_color: bool,
) {
    if let Some(opus) = model_specific {
        let opus_bar = render_progress_bar(opus.used_percent, 20, use_color);
        lines.push(format!(
            "  Opus:    {} {} used",
            opus_bar,
            format_percent(opus.used_percent)
        ));
    }
}

pub fn render_brief_text(provider: ProviderId, result: &ProviderFetchResult) -> String {
    if provider == ProviderId::DeepSeekHarness {
        let summary = result
            .usage
            .primary
            .reset_description
            .as_deref()
            .unwrap_or("—");
        let today = result
            .usage
            .extra_rate_windows
            .iter()
            .find(|row| row.id == "dsh-today")
            .and_then(|row| row.window.reset_description.as_deref());
        return match today {
            Some(today) if !summary.ends_with(" tok") => {
                format!("{}: {summary} · {today} tok today", provider.display_name())
            }
            _ => format!("{}: {summary}", provider.display_name()),
        };
    }
    let metadata = instantiate_provider(provider).metadata().clone();
    let usage = &result.usage;
    let reset = usage
        .primary
        .format_countdown()
        .unwrap_or_else(|| "n/a".to_string());
    let mut parts = vec![format!(
        "{} {}",
        metadata.session_label,
        format_percent(usage.primary.used_percent)
    )];
    if let Some(secondary) = &usage.secondary {
        parts.push(format!(
            "{} {}",
            metadata.weekly_label,
            format_percent(secondary.used_percent)
        ));
    }
    parts.push(format!("resets {reset}"));
    if let Some(plan) = &usage.login_method {
        parts.push(plan.clone());
    }
    format!("{}: {}", provider.display_name(), parts.join(", "))
}

fn format_percent(percent: f64) -> String {
    if !percent.is_finite() {
        "0%".to_string()
    } else if percent > 0.0 && percent < 1.0 {
        "<1%".to_string()
    } else {
        format!("{:.0}%", percent.clamp(0.0, 100.0))
    }
}

fn append_cost_line(lines: &mut Vec<String>, cost: Option<&CostSnapshot>) {
    let Some(cost) = cost else {
        return;
    };

    if let Some(limit) = cost.format_limit() {
        lines.push(format!(
            "  Cost:    {} / {} ({})",
            cost.format_used(),
            limit,
            cost.period
        ));
    } else {
        lines.push(format!(
            "  Cost:    {} ({})",
            cost.format_used(),
            cost.period
        ));
    }
}

/// Render usage as text (backwards compatible version)
pub fn render_text(provider: ProviderId, result: &ProviderFetchResult, use_color: bool) -> String {
    render_text_with_status(provider, result, None, use_color)
}

/// Render a text-based progress bar
fn render_progress_bar(percent: f64, width: usize, use_color: bool) -> String {
    let percent = if percent.is_finite() {
        percent.clamp(0.0, 100.0)
    } else {
        0.0
    };
    let filled = ((percent / 100.0) * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);

    let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(empty));

    if use_color {
        let color = if percent >= 90.0 {
            "\x1b[31m" // Red
        } else if percent >= 70.0 {
            "\x1b[33m" // Yellow
        } else {
            "\x1b[32m" // Green
        };
        format!("{}{}\x1b[0m", color, bar)
    } else {
        bar
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ClaudeIdentity, CodexIdentity, DirectoryAccount};

    fn fetch_result(usage: UsageSnapshot) -> ProviderFetchResult {
        ProviderFetchResult::new(usage, "test")
    }

    fn test_command(all_accounts: bool, accounts: ConfiguredAccounts) -> UsageCommand {
        UsageCommand {
            format: OutputFormat::Json,
            providers: vec![ProviderId::Codex, ProviderId::Claude, ProviderId::Gemini],
            use_color: false,
            brief: false,
            fetch_status: false,
            pretty: false,
            ctx: FetchContext::default(),
            accounts,
            all_accounts,
        }
    }

    #[test]
    fn all_accounts_expands_supported_providers_only() {
        let mut accounts = ConfiguredAccounts::default();
        accounts
            .codex
            .add_account(DirectoryAccount::<CodexIdentity>::new(
                Some("personal".into()),
                "/accounts/personal".into(),
            ));
        accounts
            .codex
            .add_account(DirectoryAccount::<CodexIdentity>::new(
                Some("work".into()),
                "/accounts/work".into(),
            ));
        accounts
            .claude
            .add_account(DirectoryAccount::<ClaudeIdentity>::new(
                Some("team".into()),
                "/accounts/claude-team".into(),
            ));

        let targets = usage_targets(&test_command(true, accounts));

        assert_eq!(targets.len(), 4);
        assert_eq!(targets[0].account.as_ref().unwrap().label, "personal");
        assert_eq!(targets[1].account.as_ref().unwrap().label, "work");
        assert_eq!(targets[2].provider, ProviderId::Claude);
        assert_eq!(targets[2].account.as_ref().unwrap().label, "team");
        assert_eq!(targets[3].provider, ProviderId::Gemini);
        assert!(targets[3].account.is_none());
    }

    #[test]
    fn default_usage_keeps_one_active_target_per_provider() {
        let mut accounts = ConfiguredAccounts::default();
        accounts
            .codex
            .add_account(DirectoryAccount::<CodexIdentity>::new(
                Some("personal".into()),
                "/accounts/personal".into(),
            ));
        accounts
            .codex
            .add_account(DirectoryAccount::<CodexIdentity>::new(
                Some("work".into()),
                "/accounts/work".into(),
            ));

        let targets = usage_targets(&test_command(false, accounts));

        assert_eq!(targets.len(), 3);
        assert!(targets.iter().all(|target| target.account.is_none()));
    }

    #[test]
    fn configured_account_is_identified_in_json_and_text_errors() {
        let account = AccountTarget {
            id: "account-id".into(),
            label: "work".into(),
            tint: None,
            config_dir: "/accounts/work".into(),
        };
        let mut json = serde_json::json!({"provider": "codex", "error": "failed"});

        annotate_json_account(&mut json, Some(&account));
        let text = annotate_text_account("Codex  Error: failed".into(), Some(&account), false);
        let brief = annotate_text_account("Codex: Session 10%".into(), Some(&account), true);

        assert_eq!(json["configured_account"]["id"], "account-id");
        assert_eq!(json["configured_account"]["label"], "work");
        assert!(text.contains("Configured account: work (account-id)"));
        assert_eq!(brief, "Codex: Session 10%, account work (account-id)");
    }

    #[test]
    fn text_rendering_shows_sub_one_percent_usage() {
        let result = fetch_result(UsageSnapshot::new(RateWindow::new(0.4)));

        let output = render_text_with_status(ProviderId::Codex, &result, None, false);

        assert!(output.contains("<1% used"));
    }

    #[test]
    fn brief_rendering_keeps_one_line_per_provider() {
        let result = fetch_result(
            UsageSnapshot::new(RateWindow::new(0.4))
                .with_secondary(RateWindow::new(100.0))
                .with_login_method("Pro"),
        );

        let output = render_brief_text(ProviderId::Claude, &result);

        assert_eq!(
            output,
            "Claude: Session (5h) <1%, Weekly 100%, resets n/a, Pro"
        );
    }

    #[test]
    fn deepseek_harness_brief_shows_balance_and_todays_tokens() {
        let today = |usage: UsageSnapshot| {
            usage.with_extra_rate_window(
                "dsh-today",
                "Tokens today",
                RateWindow::with_details(0.0, None, None, Some("1,234".into())),
            )
        };
        let balance = fetch_result(today(UsageSnapshot::new(RateWindow::with_details(
            0.0,
            None,
            None,
            Some("¥42.12".into()),
        ))));
        assert_eq!(
            render_brief_text(ProviderId::DeepSeekHarness, &balance),
            "DeepSeek Harness: ¥42.12 · 1,234 tok today"
        );
        let tokens_only = fetch_result(today(UsageSnapshot::new(RateWindow::with_details(
            0.0,
            None,
            None,
            Some("1,234 tok".into()),
        ))));
        assert_eq!(
            render_brief_text(ProviderId::DeepSeekHarness, &tokens_only),
            "DeepSeek Harness: 1,234 tok"
        );
    }

    #[test]
    fn gemini_plan_preserves_acronym_casing() {
        let result = fetch_result(
            UsageSnapshot::new(RateWindow::new(0.0))
                .with_login_method("Gemini Code Assist in Google One AI Pro"),
        );

        let output = render_text(ProviderId::Gemini, &result, false);

        assert!(output.contains("Plan:    Gemini Code Assist in Google One AI Pro"));
        assert!(!output.contains("Google One Ai Pro"));
    }

    #[test]
    fn progress_bar_renders_exact_empty_half_and_full_bars() {
        assert_eq!(render_progress_bar(0.0, 10, false), "[░░░░░░░░░░]");
        assert_eq!(render_progress_bar(50.0, 10, false), "[█████░░░░░]");
        assert_eq!(render_progress_bar(100.0, 10, false), "[██████████]");
    }

    #[test]
    fn progress_bar_clamps_non_finite_and_out_of_range_input() {
        assert_eq!(render_progress_bar(f64::NAN, 10, false), "[░░░░░░░░░░]");
        assert_eq!(render_progress_bar(-10.0, 10, false), "[░░░░░░░░░░]");
        assert_eq!(render_progress_bar(150.0, 10, false), "[██████████]");
    }

    #[test]
    fn progress_bar_without_color_has_no_ansi_codes() {
        let output = render_progress_bar(80.0, 10, false);
        assert!(!output.contains('\u{1b}'));
        assert_eq!(output, "[████████░░]");
    }

    fn settings_with_enabled(ids: &[&str]) -> Settings {
        Settings {
            enabled_providers: ids.iter().map(|id| (*id).to_string()).collect(),
            ..Settings::default()
        }
    }

    #[test]
    fn default_selection_uses_enabled_providers_from_settings() {
        let settings = settings_with_enabled(&["grok", "cursor"]);
        let selected = ProviderSelection::from_arg(None)
            .unwrap()
            .as_list_from(&settings);

        assert_eq!(selected, vec![ProviderId::Cursor, ProviderId::Grok]);
        assert_ne!(selected, vec![ProviderId::Claude]);
    }

    #[test]
    fn default_selection_on_default_settings_is_not_claude_only() {
        let settings = Settings::default();
        let selected = ProviderSelection::from_arg(None)
            .unwrap()
            .as_list_from(&settings);

        assert_eq!(
            selected,
            vec![
                ProviderId::Codex,
                ProviderId::Claude,
                ProviderId::Cursor,
                ProviderId::Grok
            ]
        );
        assert_ne!(selected, vec![ProviderId::Claude]);
    }

    #[test]
    fn all_selection_includes_every_provider_regardless_of_settings() {
        let settings = settings_with_enabled(&["grok"]);
        let selected = ProviderSelection::from_arg(Some("all"))
            .unwrap()
            .as_list_from(&settings);

        assert_eq!(selected, ProviderId::all().to_vec());
        assert!(selected.len() > settings.get_enabled_provider_ids().len());
    }

    #[test]
    fn single_selection_stays_single_regardless_of_settings() {
        let settings = settings_with_enabled(&["grok", "cursor"]);
        let selected = ProviderSelection::from_arg(Some("claude"))
            .unwrap()
            .as_list_from(&settings);

        assert_eq!(selected, vec![ProviderId::Claude]);
    }

    #[test]
    fn empty_enabled_providers_resolves_to_empty_list() {
        let settings = settings_with_enabled(&[]);
        let selected = ProviderSelection::from_arg(None)
            .unwrap()
            .as_list_from(&settings);

        assert!(selected.is_empty());
    }

    #[test]
    fn empty_enabled_providers_is_an_error_for_the_default_selection() {
        let settings = settings_with_enabled(&[]);
        let err = ProviderSelection::from_arg(None)
            .unwrap()
            .resolved_ids(&settings)
            .expect_err("empty enabled set must not look like success");
        assert!(err.to_string().contains("No providers are enabled"));

        let still_ok = ProviderSelection::from_arg(Some("claude"))
            .unwrap()
            .resolved_ids(&settings)
            .unwrap();
        assert_eq!(still_ok, vec![ProviderId::Claude]);
    }
}
