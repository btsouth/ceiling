//! CLI module - command-line interface
//!
//! Matches the original CodexBar CLI structure:
//! - `codexbar` - print usage from enabled providers (default)
//! - `codexbar usage` - print usage from providers
//! - `codexbar cost` - print local token cost usage
//! - `codexbar autostart` - manage Windows auto-start

#![allow(dead_code)]

pub mod account;
pub mod autostart;
pub mod config;
pub mod cost;
pub mod diagnose;
pub mod mcp;
pub mod serve;
pub mod sessions;
pub mod statusline;
pub mod tty_runner;
pub mod usage;

use clap::{Parser, Subcommand};

/// Exit codes matching original CodexBar
pub mod exit_codes {
    pub const SUCCESS: i32 = 0;
    pub const UNEXPECTED_FAILURE: i32 = 1;
    pub const PROVIDER_MISSING: i32 = 2;
    pub const PARSE_ERROR: i32 = 3;
    pub const CLI_TIMEOUT: i32 = 4;
    pub const USAGE_ERROR: i32 = 64;
}

/// Ceiling - Monitor AI provider usage limits
///
/// CLI for inspecting provider usage and managing local config. The desktop
/// menubar shell now lives in `apps/desktop-tauri/`; this binary is CLI-only.
#[derive(Parser, Debug)]
#[command(name = "codexbar")]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    // === Global flags ===
    /// Enable verbose logging
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Emit machine-readable logs (JSON) to stderr
    #[arg(long = "json-output", global = true)]
    pub json_output: bool,

    /// Set log level (trace, debug, info, warn, error)
    #[arg(long = "log-level", global = true, value_parser = ["trace", "verbose", "debug", "info", "warning", "warn", "error", "critical"])]
    pub log_level: Option<String>,

    /// Disable ANSI colors in output
    #[arg(long = "no-color", global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,

    // === Top-level args for the default usage command ===
    #[arg(short, long, help = usage::PROVIDER_ARG_HELP)]
    pub provider: Option<String>,

    /// Output format: text or json
    #[arg(short, long, value_parser = ["text", "json"])]
    pub format: Option<String>,

    /// Shorthand for --format json
    #[arg(long)]
    pub json: bool,

    /// Pretty-print JSON output
    #[arg(long)]
    pub pretty: bool,

    /// Fetch and include provider status pages
    #[arg(long)]
    pub status: bool,

    /// Fetch all token accounts where supported
    #[arg(long = "all-accounts")]
    pub all_accounts: bool,

    /// Skip credits line in output
    #[arg(long = "no-credits")]
    pub no_credits: bool,

    /// Data source: auto, web, cli, oauth
    #[arg(long, default_value = "auto", value_parser = ["auto", "web", "cli", "oauth"])]
    pub source: String,

    /// Web fetch timeout in seconds
    #[arg(long = "web-timeout", default_value = "60")]
    pub web_timeout: u64,

    /// Save HTML snapshots to temp dir when data is missing (debug)
    #[arg(long = "web-debug-dump-html")]
    pub web_debug_dump_html: bool,

    /// Retired Antigravity planInfo debug switch; accepted and ignored so
    /// existing scripts keep working.
    #[arg(long = "antigravity-plan-debug", hide = true)]
    pub antigravity_plan_debug: bool,

    /// Print one compact usage line per provider
    #[arg(long)]
    pub brief: bool,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Print usage from enabled providers as text or JSON (default command)
    Usage(usage::UsageArgs),

    /// Print local token cost usage (Claude, Codex, and Grok) without web/CLI access
    Cost(cost::CostArgs),

    /// Export safe provider diagnostics as JSON
    Diagnose(diagnose::DiagnoseArgs),

    /// List or focus local and configured remote agent sessions
    Sessions(sessions::SessionsArgs),

    /// Serve usage and cost JSON on 127.0.0.1
    Serve(serve::ServeArgs),

    /// Print one compact usage line for an editor status bar (cache-only)
    Statusline(statusline::StatuslineArgs),

    /// Expose usage and spend over MCP stdio (local-first, no network)
    Mcp(mcp::McpArgs),

    /// Manage auto-start on Windows boot
    Autostart(autostart::AutostartArgs),

    /// Manage token accounts for providers
    Account(account::AccountArgs),

    /// Configuration utilities
    Config(config::ConfigArgs),
}

impl Cli {
    /// Convert top-level args to UsageArgs for default command
    pub fn to_usage_args(&self) -> usage::UsageArgs {
        usage::UsageArgs {
            provider: self.provider.clone(),
            format: if self.json {
                usage::OutputFormat::Json
            } else if let Some(ref f) = self.format {
                f.parse().unwrap_or_default()
            } else {
                usage::OutputFormat::Text
            },
            json: self.json,
            no_credits: self.no_credits,
            no_color: self.no_color,
            pretty: self.pretty,
            status: self.status,
            all_accounts: self.all_accounts,
            source: self.source.clone(),
            web_timeout: self.web_timeout,
            web_debug_dump_html: self.web_debug_dump_html,
            antigravity_plan_debug: self.antigravity_plan_debug,
            brief: self.brief,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn bare_invocation_parses_as_default_usage() {
        let cli = Cli::try_parse_from(["codexbar"]).expect("bare invocation should parse");
        assert!(cli.command.is_none());

        let args = cli.to_usage_args();
        assert!(args.provider.is_none());
        assert_eq!(args.format, usage::OutputFormat::Text);
        assert!(!args.brief);
        assert!(!args.json);
    }

    #[test]
    fn top_level_usage_flags_map_onto_the_default_command() {
        let cli = Cli::try_parse_from(["codexbar", "--provider", "all", "--brief", "--json"])
            .expect("top-level usage flags should parse without a subcommand");
        assert!(cli.command.is_none());

        let args = cli.to_usage_args();
        assert_eq!(args.provider.as_deref(), Some("all"));
        assert!(args.brief);
        assert_eq!(args.format, usage::OutputFormat::Json);
        assert!(args.json);
    }

    #[test]
    fn explicit_usage_subcommand_still_parses() {
        let cli = Cli::try_parse_from(["codexbar", "usage", "--provider", "claude"])
            .expect("explicit usage should parse");
        match cli.command {
            Some(Commands::Usage(args)) => {
                assert_eq!(args.provider.as_deref(), Some("claude"));
            }
            other => panic!("expected usage subcommand, got {other:?}"),
        }
    }

    #[test]
    fn top_level_help_names_usage_as_the_default_command() {
        let mut command = Cli::command();
        let mut output = Vec::new();
        command
            .write_long_help(&mut output)
            .expect("top-level help should render");

        let help = String::from_utf8(output).expect("help should be valid utf-8");
        assert!(
            help.contains("default command"),
            "help should name usage as the default: {help}"
        );
        assert!(
            !help.contains("requires an explicit subcommand")
                && !help.contains("codexbar is now CLI-only"),
            "help should not claim a subcommand is required: {help}"
        );
    }

    #[test]
    fn top_level_help_mentions_nanogpt_provider() {
        let mut command = Cli::command();
        let mut output = Vec::new();
        command
            .write_long_help(&mut output)
            .expect("top-level help should render");

        let help = String::from_utf8(output).expect("help should be valid utf-8");
        assert!(help.contains("nanogpt"));
    }

    #[test]
    fn cost_subcommand_help_mentions_grok() {
        let mut command = Cli::command();
        let cost = command
            .find_subcommand_mut("cost")
            .expect("cost subcommand should exist");
        let mut output = Vec::new();
        cost.write_long_help(&mut output)
            .expect("cost help should render");

        let help = String::from_utf8(output).expect("help should be valid utf-8");
        assert!(help.contains("grok"), "cost help should list grok: {help}");
        assert!(
            help.contains("Claude, Codex, and Grok"),
            "cost about should name Grok: {help}"
        );
    }

    #[test]
    fn usage_subcommand_help_mentions_nanogpt_provider() {
        let mut command = Cli::command();
        let usage = command
            .find_subcommand_mut("usage")
            .expect("usage subcommand should exist");
        let mut output = Vec::new();
        usage
            .write_long_help(&mut output)
            .expect("usage help should render");

        let help = String::from_utf8(output).expect("help should be valid utf-8");
        assert!(help.contains("nanogpt"));
    }
}
