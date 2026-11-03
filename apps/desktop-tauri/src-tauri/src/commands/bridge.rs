use super::*;

// ── Bridge snapshot types ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RateWindowSnapshot {
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub window_minutes: Option<u32>,
    pub resets_at: Option<String>,
    pub reset_description: Option<String>,
    pub is_exhausted: bool,
    pub reserve_percent: Option<f64>,
    pub reserve_description: Option<String>,
    pub reserve_will_last_to_reset: bool,
    pub reserve_eta_seconds: Option<f64>,
}

impl RateWindowSnapshot {
    pub(super) fn from_rate_window(rw: &RateWindow) -> Self {
        Self {
            used_percent: rw.used_percent,
            remaining_percent: rw.remaining_percent(),
            window_minutes: rw.window_minutes,
            resets_at: rw.resets_at.map(|dt| dt.to_rfc3339()),
            reset_description: rw.reset_description.clone(),
            is_exhausted: rw.is_exhausted(),
            reserve_percent: None,
            reserve_description: None,
            reserve_will_last_to_reset: false,
            reserve_eta_seconds: None,
        }
    }

    /// Enrich with raw reserve info derived from pace analysis.
    /// delta_percent = actual - expected; negative means ahead (in reserve).
    /// Only meaningful for longer windows (weekly); skip if reserve rounds to 0.
    /// Localization happens at render time so cached snapshots stay language-neutral.
    fn with_pace_reserve(mut self, pace: &codexbar::core::UsagePace) -> Self {
        let reserve = pace.delta_percent.abs().round();
        if pace.delta_percent < 0.0 && reserve > 0.0 {
            self.reserve_percent = Some(reserve);
            self.reserve_will_last_to_reset = pace.will_last_to_reset;
            self.reserve_eta_seconds = pace.eta_seconds;
        }
        self
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostSnapshotBridge {
    pub used: f64,
    pub limit: Option<f64>,
    pub remaining: Option<f64>,
    pub currency_code: String,
    pub period: String,
    pub resets_at: Option<String>,
    pub formatted_used: String,
    pub formatted_limit: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedRateWindowSnapshot {
    pub id: String,
    pub title: String,
    pub window: RateWindowSnapshot,
    /// Money behind this lane, for providers that denominate it that way.
    pub amount: Option<WindowAmountBridge>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowAmountBridge {
    pub used: f64,
    pub limit: Option<f64>,
    pub currency_code: String,
    pub formatted_used: String,
    pub formatted_limit: Option<String>,
}

impl WindowAmountBridge {
    fn from_amount(amount: &codexbar::core::WindowAmount) -> Self {
        Self {
            used: amount.used,
            limit: amount.limit,
            currency_code: amount.currency_code.clone(),
            formatted_used: amount.format_used(),
            formatted_limit: amount.format_limit(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InactiveRateWindowSnapshot {
    pub id: String,
    pub title: String,
    pub description: String,
    /// "notEnforced" (provider reported no active limit) or "unavailable" (the
    /// window dropped out of an otherwise-successful response).
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromoSignalSnapshot {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub window_id: Option<String>,
    pub ends_at: Option<String>,
}

/// Pace prediction snapshot for tray/bridge display.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaceSnapshot {
    pub window_label: String,
    pub stage: &'static str,
    pub delta_percent: f64,
    pub will_last_to_reset: bool,
    pub eta_seconds: Option<f64>,
    pub expected_used_percent: f64,
    pub actual_used_percent: f64,
}

/// A frontend-friendly snapshot of one provider's usage data.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderUsageSnapshot {
    pub provider_id: String,
    pub display_name: String,
    pub primary: RateWindowSnapshot,
    pub primary_label: Option<String>,
    pub secondary: Option<RateWindowSnapshot>,
    pub secondary_label: Option<String>,
    pub model_specific: Option<RateWindowSnapshot>,
    pub tertiary: Option<RateWindowSnapshot>,
    pub tertiary_label: Option<String>,
    pub extra_rate_windows: Vec<NamedRateWindowSnapshot>,
    pub inactive_rate_windows: Vec<InactiveRateWindowSnapshot>,
    pub promo_signals: Vec<PromoSignalSnapshot>,
    pub reset_credits_available: Option<u32>,
    pub cost: Option<CostSnapshotBridge>,
    pub plan_name: Option<String>,
    pub account_email: Option<String>,
    pub source_label: String,
    pub updated_at: String,
    pub error: Option<String>,
    pub pace: Option<PaceSnapshot>,
    pub account_organization: Option<String>,
    pub tray_status_label: Option<String>,
    /// Stable id of the Ceiling-managed account this reading came from.
    /// Together with `provider_id` it identifies a row; `None` while
    /// following whichever account the CLI is signed in as.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Label of the Ceiling-managed account this reading came from. `None` when
    /// the provider is following whichever account its CLI is signed in as.
    #[serde(default)]
    pub account_label: Option<String>,
    /// Accent color for that account, so several seats stay distinguishable.
    #[serde(default)]
    pub account_tint: Option<String>,
    pub fetch_duration_ms: Option<u128>,
    pub wayfinder_usage: Option<codexbar::core::WayfinderUsageSnapshot>,
}

pub(crate) fn filter_hidden_codex_spark_rows(
    snapshot: &mut ProviderUsageSnapshot,
    spark_usage_visible: bool,
) {
    if snapshot.provider_id == "codex" && !spark_usage_visible {
        snapshot
            .extra_rate_windows
            .retain(|extra| !matches!(extra.id.as_str(), "codex-spark" | "codex-spark-weekly"));
    }
}

pub(crate) fn pace_stage_str(stage: codexbar::core::PaceStage) -> &'static str {
    use codexbar::core::PaceStage;
    match stage {
        PaceStage::OnTrack => "on_track",
        PaceStage::SlightlyAhead => "slightly_ahead",
        PaceStage::Ahead => "ahead",
        PaceStage::FarAhead => "far_ahead",
        PaceStage::SlightlyBehind => "slightly_behind",
        PaceStage::Behind => "behind",
        PaceStage::FarBehind => "far_behind",
    }
}

/// Pick the display label for the primary window from its reset cadence.
///
/// `session_label` is the provider's *configured* primary label (Codex
/// "Session", Cursor "Plan"). But Codex/Claude promote the weekly window into
/// the primary slot whenever the API omits the 5-hour meter — labeling that
/// promoted weekly "Session" reads a full week's budget as a 5-hour cap. A
/// weekly-cadence window in the primary slot therefore uses the weekly label;
/// session cadence, monthly plans, and unknown cadence keep the configured one.
fn primary_window_label<'a>(
    provider: ProviderId,
    session_label: &'a str,
    weekly_label: &'a str,
    window_minutes: Option<u32>,
) -> &'a str {
    // Cursor's weekly label names its Auto lane, so a weekly plan is still "Plan".
    if provider == ProviderId::Cursor {
        return session_label;
    }
    match window_minutes {
        Some(m) if m > 720 && m <= 20_160 => weekly_label,
        _ => session_label,
    }
}

/// A window is worth pacing when it is long enough to be spent evenly.
///
/// The same twelve-hour floor the expected-usage markers use — `expectedPace.ts`
/// skips `minutes < 12 * 60`, so exactly twelve hours is paced. A five-hour
/// session is spent in bursts, so a verdict about it would sweep across the bar
/// and mean nothing. There is deliberately no ceiling — a monthly quota is the
/// most pace-worthy window a provider reports, not an outlier to exclude.
fn is_paceable_cadence(window: &RateWindow) -> bool {
    matches!(window.window_minutes, Some(minutes) if minutes >= 720)
}

/// Name a provider-supplied window the way the bars do.
///
/// `capacityPresentation` falls back to "Extra" for a window the provider did
/// not name, so a verdict sitting beside that bar must not invent a different
/// name for it.
fn pace_label(label: Option<&str>) -> &str {
    match label.map(str::trim) {
        Some(text) if !text.is_empty() => text,
        _ => "Extra",
    }
}

/// Pick the window whose pace verdict the user most needs to see.
///
/// Pace exists to warn, so the window running hottest against its own clock
/// wins, whatever its cadence. Preferring weekly by rule meant an idle weekly
/// window could report "far below budget" on a provider whose monthly quota was
/// fifteen points over — and monthly windows were unreachable anyway, since they
/// live in the tertiary slot and the old cadence test capped at fourteen days.
///
/// Every candidate is measured against one `observed_at`, so the comparison
/// cannot turn on the clock advancing between two of them.
///
/// Returns the label of the chosen window alongside its pace.
fn preferred_pace<'a>(
    candidates: impl IntoIterator<Item = (&'a str, &'a RateWindow)>,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> Option<(String, codexbar::core::UsagePace)> {
    candidates
        .into_iter()
        .filter(|(_, window)| is_paceable_cadence(window))
        .filter_map(|(label, window)| {
            // 10080 is only the fallback for a window that states no duration:
            // `UsagePace` reads `window.window_minutes` whenever it is set, so a
            // monthly window is measured against its own month.
            codexbar::core::UsagePace::weekly(window, Some(observed_at), 10080)
                .map(|pace| (label.to_string(), pace))
        })
        // `reduce` with a strict comparison, not `max_by`, which returns the
        // last of several equal maxima. An exact tie should keep the window the
        // UI lists first.
        .reduce(|best, next| {
            if next.1.delta_percent > best.1.delta_percent {
                next
            } else {
                best
            }
        })
}

impl ProviderUsageSnapshot {
    pub(super) fn from_fetch_result(
        id: ProviderId,
        metadata: &ProviderMetadata,
        result: &ProviderFetchResult,
    ) -> Self {
        let usage = &result.usage;
        let primary_label = primary_window_label(
            id,
            metadata.session_label,
            metadata.weekly_label,
            usage.primary.window_minutes,
        )
        .to_string();

        // Every long window the provider reports is a candidate, in the order
        // the UI lists them so an exact tie keeps the more prominent one.
        let tertiary_label = pace_label(usage.tertiary_label.as_deref());
        let pace_candidates = std::iter::once((primary_label.as_str(), &usage.primary))
            .chain(
                usage
                    .secondary
                    .as_ref()
                    .map(|window| (metadata.weekly_label, window)),
            )
            .chain(
                usage
                    .tertiary
                    .as_ref()
                    .map(|window| (tertiary_label, window)),
            )
            .chain(
                usage
                    .extra_rate_windows
                    .iter()
                    .map(|extra| (pace_label(Some(extra.title.as_str())), &extra.window)),
            );

        let pace =
            preferred_pace(pace_candidates, chrono::Utc::now()).map(|(label, p)| PaceSnapshot {
                window_label: label,
                stage: pace_stage_str(p.stage),
                delta_percent: p.delta_percent,
                will_last_to_reset: p.will_last_to_reset,
                eta_seconds: p.eta_seconds,
                expected_used_percent: p.expected_used_percent,
                actual_used_percent: p.actual_used_percent,
            });

        // Compute pace for secondary window (weekly) to derive reserve info
        let secondary_pace = usage
            .secondary
            .as_ref()
            .and_then(|sw| codexbar::core::UsagePace::weekly(sw, None, 10080));

        let primary_snap = RateWindowSnapshot::from_rate_window(&usage.primary);

        let secondary_snap = usage.secondary.as_ref().map(|sw| {
            let mut s = RateWindowSnapshot::from_rate_window(sw);
            if let Some(ref p) = secondary_pace {
                s = s.with_pace_reserve(p);
            }
            s
        });

        Self {
            provider_id: id.cli_name().to_string(),
            display_name: id.display_name().to_string(),
            primary: primary_snap,
            primary_label: Some(primary_label),
            secondary: secondary_snap,
            secondary_label: usage
                .secondary
                .as_ref()
                .map(|_| metadata.weekly_label.to_string()),
            model_specific: usage
                .model_specific
                .as_ref()
                .map(RateWindowSnapshot::from_rate_window),
            tertiary: usage
                .tertiary
                .as_ref()
                .map(RateWindowSnapshot::from_rate_window),
            tertiary_label: usage.tertiary_label.clone(),
            extra_rate_windows: usage
                .extra_rate_windows
                .iter()
                .map(|extra| NamedRateWindowSnapshot {
                    id: extra.id.clone(),
                    title: extra.title.clone(),
                    window: RateWindowSnapshot::from_rate_window(&extra.window),
                    amount: extra.amount.as_ref().map(WindowAmountBridge::from_amount),
                })
                .collect(),
            inactive_rate_windows: usage
                .inactive_rate_windows
                .iter()
                .map(|window| InactiveRateWindowSnapshot {
                    id: window.id.clone(),
                    title: window.title.clone(),
                    description: window.description.clone(),
                    state: window.state.as_str().to_string(),
                })
                .collect(),
            promo_signals: usage
                .promo_signals
                .iter()
                .map(|signal| PromoSignalSnapshot {
                    id: signal.id.clone(),
                    kind: match signal.kind {
                        codexbar::core::PromoKind::Boost => "boost".to_string(),
                        codexbar::core::PromoKind::Inclusion => "inclusion".to_string(),
                    },
                    title: signal.title.clone(),
                    description: signal.description.clone(),
                    window_id: signal.window_id.clone(),
                    ends_at: signal.ends_at.map(|dt| dt.to_rfc3339()),
                })
                .collect(),
            reset_credits_available: usage.reset_credits_available,
            cost: result.cost.as_ref().map(|c| CostSnapshotBridge {
                used: c.used,
                limit: c.limit,
                remaining: c.remaining(),
                currency_code: c.currency_code.clone(),
                period: c.period.clone(),
                resets_at: c.resets_at.map(|dt| dt.to_rfc3339()),
                formatted_used: c.format_used(),
                formatted_limit: c.format_limit(),
            }),
            plan_name: usage.login_method.clone(),
            account_email: usage.account_email.clone(),
            source_label: result.source_label.clone(),
            updated_at: usage.updated_at.to_rfc3339(),
            error: None,
            pace,
            account_organization: usage.account_organization.clone(),
            tray_status_label: None,
            account_id: None,
            account_label: None,
            account_tint: None,
            fetch_duration_ms: None,
            wayfinder_usage: result.wayfinder_usage.clone(),
        }
    }

    pub(super) fn from_error(id: ProviderId, _metadata: &ProviderMetadata, error: String) -> Self {
        let error = friendly_provider_error(id, &error);
        // Deliberately no primary_label: session_label names a usage *window*
        // (Antigravity's is "Claude"), not the provider. On a failed fetch
        // there is no window to label, and leaking "Claude" onto an
        // Antigravity error pill misidentified the seat on the taskbar.
        Self {
            provider_id: id.cli_name().to_string(),
            display_name: id.display_name().to_string(),
            primary: RateWindowSnapshot {
                used_percent: 0.0,
                remaining_percent: 100.0,
                window_minutes: None,
                resets_at: None,
                reset_description: None,
                is_exhausted: false,
                reserve_percent: None,
                reserve_description: None,
                reserve_will_last_to_reset: false,
                reserve_eta_seconds: None,
            },
            primary_label: None,
            secondary: None,
            secondary_label: None,
            model_specific: None,
            tertiary: None,
            tertiary_label: None,
            extra_rate_windows: Vec::new(),
            inactive_rate_windows: Vec::new(),
            promo_signals: Vec::new(),
            reset_credits_available: None,
            cost: None,
            plan_name: None,
            account_email: None,
            source_label: String::new(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            error: Some(error),
            pace: None,
            account_organization: None,
            tray_status_label: None,
            account_id: None,
            account_label: None,
            account_tint: None,
            fetch_duration_ms: None,
            wayfinder_usage: None,
        }
    }
}

/// Build a compact tray status label from a raw snapshot using the current language.
/// Localization is done at render time so cached snapshots stay language-neutral.
pub(crate) fn compact_tray_status_label(
    window: &RateWindowSnapshot,
    lang: codexbar::settings::Language,
) -> String {
    let pct = format!("{:.0}%", window.used_percent);
    if let Some(reset) = compact_reset_description(window, lang) {
        format!("{pct} • {reset}")
    } else {
        pct
    }
}

fn compact_reset_description(
    window: &RateWindowSnapshot,
    lang: codexbar::settings::Language,
) -> Option<String> {
    if let Some(ref resets_at) = window.resets_at {
        let dt = chrono::DateTime::parse_from_rfc3339(resets_at)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc))?;
        return Some(format_compact_reset_countdown(dt, lang));
    }

    window
        .reset_description
        .as_deref()
        .map(|desc| normalize_reset_description(desc, lang))
        .filter(|desc| !desc.is_empty())
}

fn format_compact_reset_countdown(
    resets_at: chrono::DateTime<chrono::Utc>,
    lang: codexbar::settings::Language,
) -> String {
    format_compact_reset_countdown_at(resets_at, lang, chrono::Utc::now())
}

fn format_compact_reset_countdown_at(
    resets_at: chrono::DateTime<chrono::Utc>,
    lang: codexbar::settings::Language,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    if resets_at <= now {
        return locale::get_text(lang, locale::LocaleKey::ResetInProgress);
    }

    // Same floor-and-clamp / 1440-minute day cut as CLI, tooltip, and the
    // TypeScript hooks (SBS-927). The previous `num_minutes().max(0)` left
    // the last 59s as "Resets in 0h 00m".
    let (days, hours, minutes) = codexbar::core::remaining_countdown_parts(resets_at - now);

    if days > 0 {
        locale::format_locale(
            lang,
            locale::LocaleKey::ResetsInDaysHours,
            &[&days.to_string(), &hours.to_string()],
        )
    } else if hours == 0 {
        locale::format_locale(
            lang,
            locale::LocaleKey::ResetsInMinutes,
            &[&minutes.to_string()],
        )
    } else {
        locale::format_locale(
            lang,
            locale::LocaleKey::ResetsInHoursMinutes,
            &[&hours.to_string(), &minutes.to_string()],
        )
    }
}

fn normalize_reset_description(desc: &str, lang: codexbar::settings::Language) -> String {
    let trimmed = desc.trim();
    let lower = trimmed.to_ascii_lowercase();
    let prefix_len = ["resets in ", "reset in ", "in "]
        .iter()
        .find(|&&p| lower.starts_with(p))
        .map(|p| p.len())
        .unwrap_or(0);
    let body = trimmed[prefix_len..].trim_start();
    format!(
        "{} {body}",
        locale::get_text(lang, locale::LocaleKey::ResetsInShort)
    )
}

/// Charts read local `~/.claude` session logs; live capacity needs CLI OAuth.
/// Call this out when capacity auth fails so Accounts/Charts do not look broken.
const CLAUDE_CHARTS_WITHOUT_CAPACITY_NOTE: &str =
    "Charts can still show local session spend without live capacity.";

pub(crate) fn friendly_provider_error(id: ProviderId, error: &str) -> String {
    if id == ProviderId::Grok {
        let trimmed = error.trim();
        let lower = trimmed.to_lowercase();
        if trimmed == "Authentication required"
            || lower.contains("auth required")
            || lower.contains("oauth")
        {
            return "Grok needs sign-in before Ceiling can read usage. Run `grok login`, then refresh Grok in Ceiling.".to_string();
        }
        if lower.contains("auth.json not found") || lower.contains("not installed") {
            return "Grok CLI sign-in was not found. Install Grok Build, run `grok login`, then refresh.".to_string();
        }
        return error.to_string();
    }

    if id != ProviderId::Claude {
        return error.to_string();
    }

    let trimmed = error.trim();
    let lower = trimmed.to_lowercase();

    if lower.contains("swift.cancellationerror")
        || lower.contains("the operation couldn't be completed")
        || lower.contains("the operation could not be completed")
    {
        return "Claude usage fetch was cancelled before usage data was returned. Refresh Claude, or re-authenticate with Claude Code and try again.".to_string();
    }

    // Multi-source Auto failures must be handled before the single-source OAuth
    // rewrite: the composite string also contains "credentials not found" and
    // used to collapse into an OAuth-only line that hid Web/CLI outcomes.
    if lower.starts_with("claude usage failed from all configured sources.") {
        let detail = trimmed
            .strip_prefix("Claude usage failed from all configured sources.")
            .unwrap_or(trimmed)
            .trim()
            .trim_start_matches('.')
            .trim()
            .replace(
                "OAuth: OAuth error: Claude OAuth credentials not found. Run `claude` to authenticate.",
                "OAuth: sign-in not found",
            )
            .replace(
                "OAuth: OAuth error: Claude OAuth credentials not found",
                "OAuth: sign-in not found",
            )
            .replace(
                "Web: No cookies available for web API",
                "Web: no session available",
            )
            .replace("CLI: Provider not installed:", "CLI: not installed:");
        return format!(
            "Claude capacity could not be loaded ({detail}). Run `claude` once in a terminal, then refresh Claude in Ceiling. {CLAUDE_CHARTS_WITHOUT_CAPACITY_NOTE}"
        );
    }

    if let Some(guidance) = trimmed.strip_prefix("OAuth error: Claude Desktop") {
        return format!("Claude Desktop{guidance}");
    }

    if lower.contains("claude oauth credentials not found") {
        return format!(
            "Claude CLI sign-in was not found. Run `claude` once in a terminal, then refresh Claude in Ceiling. {CLAUDE_CHARTS_WITHOUT_CAPACITY_NOTE}"
        );
    }

    if lower.contains("oauth token expired") || lower.contains("token invalid or expired") {
        return format!(
            "Claude sign-in expired. Run `claude` to refresh your Claude Code login, then refresh Claude in Ceiling. {CLAUDE_CHARTS_WITHOUT_CAPACITY_NOTE}"
        );
    }

    if trimmed == "Authentication required" {
        return format!(
            "Claude needs a CLI sign-in before Ceiling can read capacity. Run `claude` once, then refresh Claude in Ceiling. {CLAUDE_CHARTS_WITHOUT_CAPACITY_NOTE}"
        );
    }

    trimmed.to_string()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapState {
    pub(crate) contract_version: &'static str,
    pub(crate) providers: Vec<ProviderCatalogEntry>,
    pub(crate) settings: SettingsSnapshot,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentSurfaceState {
    pub mode: String,
    pub target: SurfaceTarget,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCatalogEntry {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) cookie_domain: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSnapshot {
    enabled_providers: Vec<String>,
    provider_order: Vec<String>,
    refresh_interval_secs: u64,
    refresh_all_providers_on_menu_open: bool,
    start_at_login: bool,
    start_minimized: bool,
    show_notifications: bool,
    capacity_event_notifications_enabled: bool,
    sound_enabled: bool,
    sound_volume: u8,
    high_usage_threshold: f64,
    critical_usage_threshold: f64,
    spend_budget_alerts_enabled: bool,
    spend_budget_period: String,
    spend_budget_warning_usd: f64,
    spend_budget_limit_usd: f64,
    spend_anomaly_alerts_enabled: bool,
    spend_anomaly_multiplier: f64,
    provider_incident_badges_enabled: bool,
    provider_usage_thresholds:
        std::collections::HashMap<String, codexbar::settings::UsageThresholdOverride>,
    predictive_pace_warning_enabled: bool,
    switcher_shows_icons: bool,
    menu_bar_shows_highest_usage: bool,
    menu_bar_shows_percent: bool,
    show_as_used: bool,
    show_all_token_accounts_in_menu: bool,
    enable_animations: bool,
    reset_time_relative: bool,
    show_reset_when_exhausted: bool,
    menu_bar_display_mode: String,
    hide_personal_info: bool,
    update_channel: &'static str,
    auto_download_updates: bool,
    install_updates_on_quit: bool,
    global_shortcut: String,
    taskbar_toggle_shortcut: String,
    codex_custom_sessions_dirs: Vec<String>,
    agent_sessions_enabled: bool,
    agent_session_ssh_hosts: Vec<String>,
    ui_language: &'static str,
    theme: &'static str,
    window_scale_percent: u16,
    tray_scale_percent: u16,
    powertoys_status_pipe_enabled: bool,
    claude_avoid_keychain_prompts: bool,
    codex_spark_usage_visible: bool,
    disable_keychain_access: bool,
    wayfinder_gateway_url: String,
    provider_metrics: std::collections::HashMap<String, &'static str>,
    float_bar_enabled: bool,
    taskbar_widget_enabled: bool,
    taskbar_widget_all_monitors: bool,
    float_bar_opacity: u8,
    float_bar_scale: u8,
    float_bar_orientation: String,
    float_bar_style: String,
    taskbar_widget_open_on_hover: bool,
    float_bar_density: String,
    float_bar_information_mode: String,
    float_bar_selection_mode: String,
    float_bar_foreground_detection: bool,
    float_bar_contrast: String,
    float_bar_click_through: bool,
    float_bar_provider_ids: Vec<String>,
    /// Provider CLI name → directory-account id for the compact strip. Empty
    /// map means Auto (hottest) for every provider.
    taskbar_account_by_provider: std::collections::HashMap<String, String>,
    float_bar_dark_text: bool,
    float_bar_show_reset_inline: bool,
    float_bar_show_cost: bool,
}

#[tauri::command]
pub fn get_bootstrap_state() -> BootstrapState {
    let settings = Settings::load();
    BootstrapState {
        contract_version: "v1",
        providers: provider_catalog_for(&settings),
        settings: SettingsSnapshot::from(settings),
    }
}

#[tauri::command]
pub fn get_provider_catalog() -> Vec<ProviderCatalogEntry> {
    provider_catalog_for(&Settings::load())
}

#[tauri::command]
pub fn get_settings_snapshot() -> SettingsSnapshot {
    SettingsSnapshot::from(Settings::load())
}

impl From<Settings> for SettingsSnapshot {
    fn from(settings: Settings) -> Self {
        let avoid_keychain_prompts = settings.claude_avoid_keychain_prompts();
        let codex_spark_usage_visible = settings.codex_spark_usage_visible();
        let wayfinder_gateway_url = settings.gateway_url(ProviderId::Wayfinder).to_string();

        let provider_order = settings.provider_display_order_names();
        let float_bar_contrast = codexbar::settings::resolved_float_bar_contrast(&settings);
        let enabled_providers = provider_order
            .iter()
            .filter(|provider_id| settings.enabled_providers.contains(*provider_id))
            .cloned()
            .collect();

        let provider_metrics = settings
            .provider_metrics
            .into_iter()
            .map(|(k, v)| (k, metric_preference_label(v)))
            .collect();

        Self {
            enabled_providers,
            provider_order,
            refresh_interval_secs: settings.refresh_interval_secs,
            refresh_all_providers_on_menu_open: settings.refresh_all_providers_on_menu_open,
            start_at_login: settings.start_at_login,
            start_minimized: settings.start_minimized,
            show_notifications: settings.show_notifications,
            capacity_event_notifications_enabled: settings.capacity_event_notifications_enabled,
            sound_enabled: settings.sound_enabled,
            sound_volume: settings.sound_volume,
            high_usage_threshold: settings.high_usage_threshold,
            critical_usage_threshold: settings.critical_usage_threshold,
            spend_budget_alerts_enabled: settings.spend_budget_alerts_enabled,
            spend_budget_period: settings.spend_budget_period,
            spend_budget_warning_usd: settings.spend_budget_warning_usd,
            spend_budget_limit_usd: settings.spend_budget_limit_usd,
            spend_anomaly_alerts_enabled: settings.spend_anomaly_alerts_enabled,
            spend_anomaly_multiplier: settings.spend_anomaly_multiplier,
            provider_incident_badges_enabled: settings.provider_incident_badges_enabled,
            provider_usage_thresholds: settings.provider_usage_thresholds,
            predictive_pace_warning_enabled: settings.predictive_pace_warning_enabled,
            switcher_shows_icons: settings.switcher_shows_icons,
            menu_bar_shows_highest_usage: settings.menu_bar_shows_highest_usage,
            menu_bar_shows_percent: settings.menu_bar_shows_percent,
            show_as_used: settings.show_as_used,
            show_all_token_accounts_in_menu: settings.show_all_token_accounts_in_menu,
            enable_animations: settings.enable_animations,
            reset_time_relative: settings.reset_time_relative,
            show_reset_when_exhausted: settings.show_reset_when_exhausted,
            menu_bar_display_mode: settings.menu_bar_display_mode,
            hide_personal_info: settings.hide_personal_info,
            update_channel: update_channel_label(settings.update_channel),
            auto_download_updates: settings.auto_download_updates,
            install_updates_on_quit: settings.install_updates_on_quit,
            global_shortcut: settings.global_shortcut,
            taskbar_toggle_shortcut: settings.taskbar_toggle_shortcut,
            codex_custom_sessions_dirs: settings.codex_custom_sessions_dirs,
            agent_sessions_enabled: settings.agent_sessions_enabled,
            agent_session_ssh_hosts: settings.agent_session_ssh_hosts,
            ui_language: language_label(settings.ui_language),
            theme: theme_label(settings.theme),
            window_scale_percent: settings.window_scale_percent,
            tray_scale_percent: settings.tray_scale_percent,
            powertoys_status_pipe_enabled: settings.powertoys_status_pipe_enabled,
            claude_avoid_keychain_prompts: avoid_keychain_prompts,
            codex_spark_usage_visible,
            disable_keychain_access: settings.disable_keychain_access,
            wayfinder_gateway_url,
            provider_metrics,
            float_bar_enabled: settings.float_bar_enabled,
            taskbar_widget_enabled: settings.taskbar_widget_enabled,
            taskbar_widget_all_monitors: settings.taskbar_widget_all_monitors,
            float_bar_opacity: settings.float_bar_opacity,
            float_bar_scale: settings.float_bar_scale,
            float_bar_orientation: settings.float_bar_orientation,
            float_bar_style: settings.float_bar_style,
            taskbar_widget_open_on_hover: settings.taskbar_widget_open_on_hover,
            float_bar_density: settings.float_bar_density,
            float_bar_information_mode: settings.float_bar_information_mode,
            float_bar_selection_mode: settings.float_bar_selection_mode,
            float_bar_foreground_detection: settings.float_bar_foreground_detection,
            float_bar_contrast,
            float_bar_click_through: settings.float_bar_click_through,
            float_bar_provider_ids: settings.float_bar_provider_ids,
            taskbar_account_by_provider: settings.taskbar_account_by_provider,
            float_bar_dark_text: settings.float_bar_dark_text,
            float_bar_show_reset_inline: settings.float_bar_show_reset_inline,
            float_bar_show_cost: settings.float_bar_show_cost,
        }
    }
}

pub(crate) fn provider_catalog_for(settings: &Settings) -> Vec<ProviderCatalogEntry> {
    settings
        .provider_display_order()
        .into_iter()
        .map(|provider| ProviderCatalogEntry {
            id: provider.cli_name().to_string(),
            display_name: provider.display_name().to_string(),
            cookie_domain: provider.cookie_domain().map(ToString::to_string),
        })
        .collect()
}

pub(super) fn update_channel_label(channel: UpdateChannel) -> &'static str {
    match channel {
        UpdateChannel::Stable => "stable",
        UpdateChannel::Beta => "beta",
    }
}

pub(super) fn language_label(language: Language) -> &'static str {
    language.label()
}

fn theme_label(theme: ThemePreference) -> &'static str {
    match theme {
        ThemePreference::Auto => "auto",
        ThemePreference::Light => "light",
        ThemePreference::Dark => "dark",
    }
}

pub(super) fn parse_theme(s: &str) -> Option<ThemePreference> {
    match s {
        "auto" => Some(ThemePreference::Auto),
        "light" => Some(ThemePreference::Light),
        "dark" => Some(ThemePreference::Dark),
        _ => None,
    }
}

fn metric_preference_label(pref: MetricPreference) -> &'static str {
    match pref {
        MetricPreference::Automatic => "automatic",
        MetricPreference::Session => "session",
        MetricPreference::Weekly => "weekly",
        MetricPreference::Model => "model",
        MetricPreference::Tertiary => "tertiary",
        MetricPreference::Credits => "credits",
        MetricPreference::ExtraUsage => "extraUsage",
        MetricPreference::Average => "average",
    }
}

pub(super) fn parse_metric_preference(s: &str) -> Option<MetricPreference> {
    match s {
        "automatic" => Some(MetricPreference::Automatic),
        "session" => Some(MetricPreference::Session),
        "weekly" => Some(MetricPreference::Weekly),
        "model" => Some(MetricPreference::Model),
        "tertiary" => Some(MetricPreference::Tertiary),
        "credits" => Some(MetricPreference::Credits),
        "extraUsage" | "extrausage" => Some(MetricPreference::ExtraUsage),
        "average" => Some(MetricPreference::Average),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (SOU-136): a weekly-cadence window promoted into the primary
    /// slot (Codex when the 5-hour is lifted) must read as the weekly, not the
    /// session — otherwise a full week's budget looks like a 5-hour cap.
    #[test]
    fn cursor_plan_keeps_its_name_on_a_weekly_cadence() {
        assert_eq!(
            primary_window_label(ProviderId::Cursor, "Plan", "Auto", Some(10_080)),
            "Plan"
        );
        assert_eq!(
            primary_window_label(ProviderId::Cursor, "Plan", "Auto", Some(44_640)),
            "Plan"
        );
    }

    #[test]
    fn primary_label_follows_window_cadence() {
        // Codex normal: 5-hour present.
        assert_eq!(
            primary_window_label(ProviderId::Codex, "Session", "Weekly", Some(300)),
            "Session"
        );
        // Codex 5-hour lifted: weekly promoted into primary.
        assert_eq!(
            primary_window_label(ProviderId::Codex, "Session", "Weekly", Some(10_080)),
            "Weekly"
        );
        // Cursor's monthly plan keeps its configured label.
        assert_eq!(
            primary_window_label(ProviderId::Codex, "Plan", "Weekly", Some(43_200)),
            "Plan"
        );
        // Unknown cadence keeps the configured label.
        assert_eq!(
            primary_window_label(ProviderId::Codex, "Session", "Weekly", None),
            "Session"
        );
    }

    /// Build a window that is `elapsed_fraction` of the way through its period,
    /// measured from one shared instant so two candidates can tie exactly.
    fn window_at(
        now: chrono::DateTime<chrono::Utc>,
        used_percent: f64,
        window_minutes: u32,
        elapsed_fraction: f64,
    ) -> RateWindow {
        let remaining_minutes = f64::from(window_minutes) * (1.0 - elapsed_fraction);
        let resets_at = now + chrono::Duration::seconds((remaining_minutes * 60.0) as i64);
        RateWindow::with_details(used_percent, Some(window_minutes), Some(resets_at), None)
    }

    /// Regression: the OpenCode Go report. An untouched weekly window read
    /// "far below budget" while the monthly quota it shares a card with was
    /// fifteen points over. The monthly window was doubly unreachable — the old
    /// rule looked only at primary/secondary, and its cadence test capped at
    /// fourteen days.
    #[test]
    fn pace_reports_the_window_running_hottest() {
        let now = chrono::Utc::now();
        let session = window_at(now, 0.0, 300, 0.5);
        let weekly = window_at(now, 0.0, 10_080, 0.15);
        let monthly = window_at(now, 61.0, 43_200, 0.46);

        let (label, pace) = preferred_pace(
            [
                ("Rolling (5h)", &session),
                ("Weekly", &weekly),
                ("Monthly", &monthly),
            ],
            now,
        )
        .expect("a long window should pace");

        assert_eq!(label, "Monthly");
        // Measured against its own thirty days, not the 10080-minute fallback,
        // which would have read this as far *under* budget.
        assert!(
            pace.delta_percent > 10.0,
            "delta was {}",
            pace.delta_percent
        );
    }

    #[test]
    fn pace_still_reads_a_weekly_window_promoted_to_primary() {
        let now = chrono::Utc::now();
        let weekly = window_at(now, 51.0, 10_080, 0.30);

        let (label, pace) = preferred_pace([("Weekly", &weekly)], now).expect("weekly should pace");

        assert_eq!(label, "Weekly");
        assert_eq!(pace.actual_used_percent, 51.0);
    }

    /// A five-hour session is spent in bursts, so it gets no verdict at all
    /// rather than a misleading one — the same floor the bar markers use.
    #[test]
    fn pace_skips_windows_too_short_to_spend_evenly() {
        let now = chrono::Utc::now();
        let session = window_at(now, 90.0, 300, 0.2);

        assert!(preferred_pace([("Rolling (5h)", &session)], now).is_none());
    }

    /// `expectedPace.ts` draws a marker on a window of exactly twelve hours, so
    /// the verdict beside it must not go silent there.
    #[test]
    fn pace_includes_a_window_of_exactly_twelve_hours() {
        let now = chrono::Utc::now();
        let half_day = window_at(now, 80.0, 720, 0.25);

        assert!(preferred_pace([("Half-day", &half_day)], now).is_some());
    }

    /// An exact tie keeps the window the UI lists first, so the verdict does
    /// not hop between equally-hot windows between refreshes.
    #[test]
    fn pace_breaks_an_exact_tie_toward_the_first_window() {
        let now = chrono::Utc::now();
        let weekly = window_at(now, 40.0, 10_080, 0.20);
        let monthly = window_at(now, 40.0, 10_080, 0.20);

        let (label, _) =
            preferred_pace([("Weekly", &weekly), ("Monthly", &monthly)], now).expect("should pace");

        assert_eq!(label, "Weekly");
    }

    /// The bars call an unnamed window "Extra"; the verdict beside them must
    /// not invent a name, and must never render blank.
    #[test]
    fn pace_names_an_unnamed_window_the_way_the_bars_do() {
        assert_eq!(pace_label(None), "Extra");
        assert_eq!(pace_label(Some("   ")), "Extra");
        assert_eq!(pace_label(Some("Monthly")), "Monthly");
    }

    fn snapshot_window_with(
        used_percent: f64,
        window_minutes: Option<u32>,
        resets_at: Option<chrono::DateTime<chrono::Utc>>,
        reset_description: Option<String>,
    ) -> RateWindowSnapshot {
        RateWindowSnapshot {
            used_percent,
            remaining_percent: 100.0 - used_percent,
            window_minutes,
            resets_at: resets_at.map(|dt| dt.to_rfc3339()),
            reset_description,
            is_exhausted: false,
            reserve_percent: None,
            reserve_description: None,
            reserve_will_last_to_reset: false,
            reserve_eta_seconds: None,
        }
    }

    #[test]
    fn tray_status_prefers_relative_reset_countdown() {
        let window = snapshot_window_with(
            13.0,
            Some(300),
            Some(chrono::Utc::now() + chrono::Duration::minutes(125)),
            Some("Jun 10 at 3:00PM".to_string()),
        );

        let label = compact_tray_status_label(&window, Language::English);

        assert!(label.starts_with("13% • Resets in 2h "));
        assert!(label.ends_with('m'));
        assert!(!label.contains("Jun 10"));
    }

    #[test]
    fn tray_status_normalizes_fallback_reset_description() {
        let window = snapshot_window_with(8.0, Some(300), None, Some("2h 05m".to_string()));

        assert_eq!(
            compact_tray_status_label(&window, Language::English),
            "8% • Resets in 2h 05m"
        );
    }

    /// SBS-927: last sub-minute must not render as "Resets in 0h 00m".
    #[test]
    fn compact_reset_countdown_does_not_show_zero_minutes_while_time_remains() {
        let now = "2026-04-02T12:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let resets_at = now + chrono::Duration::seconds(30);
        assert_eq!(
            format_compact_reset_countdown_at(resets_at, Language::English, now),
            "Resets in 1m"
        );
    }

    /// SBS-927: 24h 1s is a day, not "Resets in 24h 00m".
    #[test]
    fn compact_reset_countdown_cuts_a_day_at_twenty_four_hours() {
        let now = "2026-04-02T12:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let at = |seconds: i64| now + chrono::Duration::seconds(seconds);
        assert_eq!(
            format_compact_reset_countdown_at(at(24 * 3600 + 1), Language::English, now),
            "Resets in 1d 0h"
        );
        assert_eq!(
            format_compact_reset_countdown_at(at(24 * 3600 + 30 * 60), Language::English, now),
            "Resets in 1d 0h"
        );
    }
}
