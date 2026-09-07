//! Settings management for Ceiling
//!
//! Handles persistent configuration including:
//! - Enabled/disabled providers
//! - Refresh interval
//! - Manual cookies
//! - Other user preferences

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::core::ProviderId;

const NOTIFICATION_POLICY_VERSION: u8 = 1;

fn carries_legacy_credentials(config: &ProviderConfig) -> bool {
    config
        .manual_cookie_header
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || config
            .api_token
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

fn legacy_credential_to_migrate<'a>(
    legacy_value: Option<&'a str>,
    stored_value: Option<&str>,
) -> Option<&'a str> {
    legacy_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|_| stored_value.is_none_or(|value| value.trim().is_empty()))
}

fn parse_start_at_login_command(command: &str) -> Option<(String, String)> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    if let Some(rest) = command.strip_prefix('"') {
        let (exe, extras) = rest.split_once('"')?;
        if exe.is_empty() {
            return None;
        }
        return Some((exe.to_string(), extras.to_string()));
    }
    let lowercase = command.to_ascii_lowercase();
    for (index, _) in lowercase.match_indices(".exe") {
        let exe_end = index + 4;
        let (exe, extras) = command.split_at(exe_end);
        if extras.is_empty() || extras.starts_with(char::is_whitespace) {
            return Some((exe.to_string(), extras.to_string()));
        }
    }
    if let Some((exe, extras)) = command.split_once(char::is_whitespace) {
        if exe.is_empty() {
            return None;
        }
        return Some((exe.to_string(), extras.to_string()));
    }
    Some((command.to_string(), String::new()))
}

fn start_at_login_path_key(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

fn is_start_at_login_binary_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("ceiling.exe")
        || name.eq_ignore_ascii_case("codexbar-cli.exe")
        || name.eq_ignore_ascii_case("codexbar-desktop.exe")
}

fn owns_start_at_login_entry(existing_exe: &std::path::Path, intended: &std::path::Path) -> bool {
    if start_at_login_path_key(existing_exe) == start_at_login_path_key(intended) {
        return true;
    }
    let existing_name = existing_exe.file_name().and_then(|name| name.to_str());
    let intended_name = intended.file_name().and_then(|name| name.to_str());
    if !existing_name.is_some_and(is_start_at_login_binary_name)
        || !intended_name.is_some_and(is_start_at_login_binary_name)
    {
        return false;
    }
    match (existing_exe.parent(), intended.parent()) {
        (Some(existing_dir), Some(intended_dir)) => {
            start_at_login_path_key(existing_dir) == start_at_login_path_key(intended_dir)
        }
        _ => false,
    }
}

mod api_keys;
mod manual_cookies;
mod provider_workspace;
mod raw;
mod status;
mod types;

pub use api_keys::*;
pub use manual_cookies::*;
pub use provider_workspace::*;
use raw::RawSettings;
pub use status::*;
pub use types::*;

/// Remove every app-managed credential for a provider under one shared lock.
///
/// Filesystems do not provide an atomic transaction across these three files.
/// We therefore read and validate all stores before the first write, exclude
/// every competing writer until the last write, and make the operation
/// idempotent. An I/O failure between atomic file replacements can leave a
/// partial revocation, but retrying safely completes it; it can never restore
/// a credential or lose another provider's concurrent update.
///
/// After the shared files are written, the provider-owned hook runs so a
/// vendor-specific shadow copy (StepFun's refreshed Oasis keyring token,
/// SBS-920) cannot outlive Sign out. A hook error fails the revoke; retrying
/// is safe because the file removals and the hook are both idempotent.
pub fn revoke_managed_credentials(provider: ProviderId) -> anyhow::Result<()> {
    crate::secure_file::with_state_write_lock(|| {
        let keys_path = ApiKeys::keys_path()
            .ok_or_else(|| std::io::Error::other("Could not determine API keys path"))?;
        let cookies_path = ManualCookies::cookies_path()
            .ok_or_else(|| std::io::Error::other("Could not determine cookies path"))?;
        let token_store = crate::core::TokenAccountStore::new();

        revoke_managed_credentials_in(provider, &keys_path, &cookies_path, &token_store, || {
            crate::providers::clear_persisted_credentials(provider)
        })
    })
    .map_err(Into::into)
}

/// Whether `provider` still has a credential in Preferences.
///
/// An unreadable store answers `true`. This exists for the background refresh
/// paths, which ask "was this revoked while I was working" before writing a
/// renewed token, and [`ApiKeys::load`] cannot tell an empty store from one
/// that failed to decode. Reading a decode failure as a revoke would drop a
/// renewed token and leave the session on a credential the provider may have
/// already rotated away from.
pub(crate) fn provider_credential_present(provider: ProviderId) -> bool {
    match ApiKeys::try_load() {
        Ok(keys) => keys.has_key(provider.cli_name()),
        Err(error) => {
            tracing::warn!("Could not read stored API keys ({error}); treating as still signed in");
            true
        }
    }
}

/// The body of [`revoke_managed_credentials`], with the paths and the
/// provider-specific hook passed in so a test can fail the hook on purpose.
///
/// The caller holds the state write lock.
fn revoke_managed_credentials_in(
    provider: ProviderId,
    keys_path: &std::path::Path,
    cookies_path: &std::path::Path,
    token_store: &crate::core::TokenAccountStore,
    clear_persisted: impl FnOnce() -> anyhow::Result<()>,
) -> std::io::Result<()> {
    let mut keys = ApiKeys::try_load_from(keys_path).map_err(std::io::Error::other)?;
    let mut cookies = ManualCookies::try_load_from(cookies_path).map_err(std::io::Error::other)?;
    let mut token_accounts = token_store.load().map_err(std::io::Error::other)?;

    keys.remove(provider.cli_name());
    cookies.remove(provider.cli_name());
    token_accounts.remove(&provider);

    // The keyring copy goes first, and the file stores only after it is
    // confirmed gone. Both of the orders' failure modes are real:
    //
    // * A hook that errors after the files were already emptied returns
    //   Err with every file store reporting "no credential", which is
    //   exactly the state that hides the Revoke control. The user is left
    //   unable to retry a revoke that did not finish, while the leftover
    //   keyring token still authenticates.
    // * A crash in the gap between the two leaves whichever half ran. With
    //   the keyring first that is a live-looking file store and no token,
    //   which fails closed; the other way round it is a signed-out UI over
    //   a token that still works.
    clear_persisted().map_err(std::io::Error::other)?;

    keys.save_to(keys_path).map_err(std::io::Error::other)?;
    cookies
        .save_to(cookies_path)
        .map_err(std::io::Error::other)?;
    token_store
        .save_unlocked(&token_accounts)
        .map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests;

/// Application settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "RawSettings", default)]
pub struct Settings {
    /// Enabled provider IDs (by CLI name)
    pub enabled_providers: HashSet<String>,

    /// Refresh interval in seconds (0 = manual only)
    pub refresh_interval_secs: u64,

    /// Legacy compatibility field. Opening a tray/menu surface does not
    /// force-refresh; nothing reads this flag.
    #[serde(default)]
    pub refresh_all_providers_on_menu_open: bool,

    /// Whether to start minimized
    pub start_minimized: bool,

    /// Whether to start at login
    pub start_at_login: bool,

    /// Whether to show notifications
    pub show_notifications: bool,

    /// Whether confirmed scheduled and early resets may raise OS alerts.
    #[serde(default = "default_true")]
    pub capacity_event_notifications_enabled: bool,

    /// Whether to play sound effects for threshold alerts
    pub sound_enabled: bool,

    /// Sound volume for alerts (0-100)
    pub sound_volume: u8,

    /// High usage threshold for warnings (percentage)
    pub high_usage_threshold: f64,

    /// Critical usage threshold for visual severity (percentage)
    pub critical_usage_threshold: f64,

    /// Whether to monitor estimated local API value against a user-set budget.
    #[serde(default)]
    pub spend_budget_alerts_enabled: bool,

    /// Budget period: "daily" or calendar-month-to-date "monthly".
    #[serde(default = "default_spend_budget_period")]
    pub spend_budget_period: String,

    /// Soft alert threshold for estimated API value in USD.
    #[serde(default = "default_spend_budget_warning_usd")]
    pub spend_budget_warning_usd: f64,

    /// Near-cap alert threshold for estimated API value in USD.
    #[serde(default = "default_spend_budget_limit_usd")]
    pub spend_budget_limit_usd: f64,

    /// Whether to warn when today's estimated API value runs far above the
    /// recent daily norm. This is a spike detector, not a cap: it catches a
    /// runaway loop on a machine whose owner never set a budget.
    #[serde(default)]
    pub spend_anomaly_alerts_enabled: bool,

    /// How many times the recent daily median today must reach before it counts
    /// as a spike.
    #[serde(default = "default_spend_anomaly_multiplier")]
    pub spend_anomaly_multiplier: f64,

    /// Whether to poll public provider status pages and badge providers that
    /// are having an incident. Off by default: it adds a set of outbound hosts
    /// derived from the user's enabled providers - each provider's public
    /// status page - on top of the usage requests those providers already
    /// receive. Ceiling also contacts models.dev for public model prices and
    /// GitHub for the update check; those are fixed hosts and are not gated by
    /// this switch.
    #[serde(default)]
    pub provider_incident_badges_enabled: bool,

    /// Internal migration marker for notification defaults. This is not a UI
    /// preference; it prevents old default values from surviving policy fixes.
    #[serde(default)]
    pub notification_policy_version: u8,

    pub provider_usage_thresholds: HashMap<String, UsageThresholdOverride>,

    /// Show provider icons in the merged switcher UI
    #[serde(default = "default_true")]
    pub switcher_shows_icons: bool,

    /// Legacy compatibility field. The branded tray always follows the most
    /// constrained healthy provider and ignores this flag.
    #[serde(default)]
    pub menu_bar_shows_highest_usage: bool,

    /// Legacy compatibility field. The branded tray always draws the Ceiling
    /// icon and ignores this flag.
    #[serde(default)]
    pub menu_bar_shows_percent: bool,

    /// Show usage bars as "used" (true) or "remaining" (false)
    pub show_as_used: bool,

    /// Enable UI animations (chart entrances, transitions)
    pub enable_animations: bool,

    /// Show reset times as relative (e.g., "2h 30m" instead of "3:00 PM")
    pub reset_time_relative: bool,

    /// Replace exhausted quota text with its concrete future reset time.
    #[serde(default)]
    pub show_reset_when_exhausted: bool,

    /// Warn when a provider's pace predicts exhaustion before its reset.
    /// Opt-in: this is a prediction, so it stays off until asked for.
    #[serde(default)]
    pub predictive_pace_warning_enabled: bool,

    /// Legacy compatibility field (`"minimal"`, `"compact"`, or `"detailed"`).
    /// The branded tray ignores this setting.
    pub menu_bar_display_mode: String,

    /// Show all token accounts in provider menus instead of collapsing behind switchers
    #[serde(default)]
    pub show_all_token_accounts_in_menu: bool,

    /// Per-provider configuration map (cookie/usage source, region, manual
    /// headers, API tokens, etc). Replaces the legacy flat per-provider
    /// fields; legacy `settings.json` files are migrated via [`RawSettings`].
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub provider_configs: HashMap<ProviderId, ProviderConfig>,

    /// Per-provider configs whose id this build does not resolve, carried
    /// through so saving never deletes a config we only failed to recognize.
    /// A build that knows the id folds these back into
    /// [`provider_configs`](Self::provider_configs) on load (SBS-625).
    ///
    /// Carried through as far as this build's [`ProviderConfig`] models: they
    /// are re-serialized through that struct, so a per-provider *field* a newer
    /// build added is still dropped. This preserves the entry, not every byte.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub unrecognized_provider_configs: HashMap<String, ProviderConfig>,

    /// Disable OS keychain reads and writes (SBS-1023).
    #[serde(default)]
    pub disable_keychain_access: bool,

    /// Hide personal info (emails, account names) for streaming/sharing
    pub hide_personal_info: bool,

    /// Update channel for receiving updates (Stable or Beta)
    pub update_channel: UpdateChannel,

    /// Per-provider metric preference for tray display
    #[serde(default)]
    pub provider_metrics: HashMap<String, MetricPreference>,

    /// Preferred display order of provider IDs (CLI names).
    ///
    /// An empty list means "fall back to the canonical `ProviderId::all()`
    /// order". Unknown or duplicated ids are filtered out on load; new
    /// providers are appended in their canonical order.
    #[serde(default)]
    pub provider_order: Vec<String>,

    /// Global keyboard shortcut to open the menu (e.g., "Ctrl+Shift+U")
    #[serde(default = "default_global_shortcut")]
    pub global_shortcut: String,

    /// Global keyboard shortcut to show or hide the native taskbar capacity strip.
    #[serde(default = "default_taskbar_toggle_shortcut")]
    pub taskbar_toggle_shortcut: String,

    /// Additional Codex home or sessions directories to include in local cost scans.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codex_custom_sessions_dirs: Vec<String>,

    /// Discover local and configured SSH Codex/Claude sessions.
    #[serde(default)]
    pub agent_sessions_enabled: bool,

    /// SSH targets queried for remote agent sessions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_session_ssh_hosts: Vec<String>,

    /// Automatically download updates in the background
    #[serde(default)]
    pub auto_download_updates: bool,

    /// Install pending updates when quitting the application
    #[serde(default)]
    pub install_updates_on_quit: bool,

    /// UI language for the application (English default for backward compatibility)
    #[serde(default)]
    pub ui_language: Language,

    /// UI theme preference (Phase 12). Defaults to Auto (prefers-color-scheme).
    #[serde(default)]
    pub theme: ThemePreference,

    /// Main PopOut window display scale, in the inclusive range 100..=250.
    /// 100 % is normal size; higher values enlarge the window content.
    #[serde(default = "default_window_scale_percent")]
    pub window_scale_percent: u16,

    /// Tray flyout display scale, in the inclusive range 100..=200.
    /// 100 % is normal size; higher values enlarge the flyout content.
    #[serde(default = "default_tray_scale_percent")]
    pub tray_scale_percent: u16,

    /// Enable the local PowerToys Command Palette status pipe.
    #[serde(default)]
    pub powertoys_status_pipe_enabled: bool,

    /// Show the separate always-on-top floating capacity bar.
    #[serde(default)]
    pub float_bar_enabled: bool,

    /// Show the native usage readout embedded in the Windows taskbar.
    #[serde(default = "default_true")]
    pub taskbar_widget_enabled: bool,

    /// Mirror the native taskbar readout onto every verified horizontal taskbar.
    #[serde(default)]
    pub taskbar_widget_all_monitors: bool,

    /// Opacity of the floating bar window, in the inclusive range 30..=100.
    /// Stored as `u8` so the on-disk format remains stable.
    #[serde(default = "default_float_bar_opacity")]
    pub float_bar_opacity: u8,

    /// Floating-bar visual scale, in the inclusive range 75..=200.
    #[serde(default = "default_float_bar_scale")]
    pub float_bar_scale: u8,

    /// Floating-bar orientation: "horizontal" (default) or "vertical".
    #[serde(default = "default_float_bar_orientation")]
    pub float_bar_orientation: String,

    /// Legacy capacity-display style. New settings always store "floating";
    /// taskbar enablement lives in `taskbar_widget_enabled`.
    #[serde(default = "default_float_bar_style")]
    pub float_bar_style: String,

    /// Open the taskbar glance panel after a short pointer dwell.
    #[serde(default = "default_true")]
    pub taskbar_widget_open_on_hover: bool,

    /// Floating-bar information density: "compact", "standard", or
    /// "detailed". Standard preserves the original layout.
    #[serde(default = "default_float_bar_density")]
    pub float_bar_density: String,

    /// Floating-bar information mode: "exact" (provider icon + exact percentage
    /// and label) or "calm" (a trustworthy pace state plus the next reset, with
    /// exact percentages on expand). Separate from density, which is geometry.
    /// Exact is the migration default so existing bars are unchanged.
    #[serde(default = "default_float_bar_information_mode")]
    pub float_bar_information_mode: String,

    /// Which providers the floating bar shows: "pinned" (the configured list),
    /// "active" (the focused supported app), or "activePlusCritical" (active
    /// plus any pinned provider at or above the warning threshold).
    #[serde(default = "default_float_bar_selection_mode")]
    pub float_bar_selection_mode: String,

    /// When false, active / active-plus-critical modes keep the pinned list
    /// and do not read the focused window.
    #[serde(default = "default_true")]
    pub float_bar_foreground_detection: bool,

    /// Floating-bar contrast mode. `None` means a pre-density settings file;
    /// resolve it through the legacy `float_bar_dark_text` preference so
    /// upgrades preserve their appearance. New installs default to auto.
    #[serde(default)]
    pub float_bar_contrast: Option<String>,

    /// When true the floating bar is fully click-through (overlay mode).
    #[serde(default)]
    pub float_bar_click_through: bool,

    /// Provider CLI names to display in the floating bar. Empty = all enabled.
    #[serde(default)]
    pub float_bar_provider_ids: Vec<String>,

    /// Per-provider account id for the compact taskbar / float-bar strip.
    ///
    /// Key is the provider CLI name (`codex`, `claude`, …); value is a
    /// directory-account UUID. Missing or empty means Auto: show the account
    /// closest to its limit. This is a display preference for the strip only —
    /// it does not change which account is "active" for CLI/fetch identity.
    #[serde(default)]
    pub taskbar_account_by_provider: std::collections::HashMap<String, String>,

    /// When true, the floating bar uses a dark-on-light palette so it
    /// stays legible on light desktop backgrounds. Defaults to false
    /// (light-on-dark, the original look).
    #[serde(default)]
    pub float_bar_dark_text: bool,

    /// When true, show the primary window's next reset inline in each pill.
    #[serde(default)]
    pub float_bar_show_reset_inline: bool,

    /// Legacy compatibility field; the current UI no longer renders local cost pills.
    #[serde(default)]
    pub float_bar_show_cost: bool,
}

fn default_window_scale_percent() -> u16 {
    100
}

pub fn clamp_window_scale_percent(value: u16) -> u16 {
    value.clamp(100, 250)
}

fn default_tray_scale_percent() -> u16 {
    100
}

pub fn clamp_tray_scale_percent(value: u16) -> u16 {
    value.clamp(100, 200)
}

fn default_float_bar_opacity() -> u8 {
    80
}

fn default_float_bar_scale() -> u8 {
    100
}

fn default_float_bar_orientation() -> String {
    "horizontal".to_string()
}

fn default_float_bar_style() -> String {
    "floating".to_string()
}

fn default_float_bar_density() -> String {
    "standard".to_string()
}

fn default_float_bar_information_mode() -> String {
    "exact".to_string()
}

fn default_float_bar_selection_mode() -> String {
    "pinned".to_string()
}

/// Normalize a floating-bar provider-selection mode. Unknown values fall
/// back to pinned so an upgrade never silently starts watching the
/// focused window.
pub fn normalize_float_bar_selection_mode(value: &str) -> String {
    match value {
        "active" => "active".to_string(),
        "activePlusCritical" => "activePlusCritical".to_string(),
        _ => "pinned".to_string(),
    }
}

/// Clamp the floating-bar opacity to the supported range.
///
/// Opacity values below 30% would make the bar effectively invisible, so we
/// pin the lower bound; the upper bound is the natural 100%.
pub fn clamp_float_bar_opacity(value: u8) -> u8 {
    value.clamp(30, 100)
}

/// Clamp the floating-bar visual scale to the supported range.
pub fn clamp_float_bar_scale(value: u8) -> u8 {
    value.clamp(75, 200)
}

/// Normalize a floating-bar orientation string. Unknown values fall back to
/// the default ("horizontal") so a corrupt settings file can't put the
/// renderer into an undefined state.
pub fn normalize_float_bar_orientation(value: &str) -> String {
    match value {
        "vertical" => "vertical".to_string(),
        _ => "horizontal".to_string(),
    }
}

/// Normalize a capacity-display style string. Unknown values fall back to the
/// current default so a corrupt setting cannot select an undefined renderer.
pub fn normalize_float_bar_style(value: &str) -> String {
    match value {
        "floating" => "floating".to_string(),
        "taskbar" => "taskbar".to_string(),
        _ => default_float_bar_style(),
    }
}

/// Normalize a floating-bar density string while preserving the established
/// standard layout for unknown or older values.
pub fn normalize_float_bar_density(value: &str) -> String {
    match value {
        "compact" => "compact".to_string(),
        "detailed" => "detailed".to_string(),
        _ => "standard".to_string(),
    }
}

/// Normalize a floating-bar information mode. Unknown or older values fall back
/// to "exact" so an upgrade never silently switches a user into calm mode.
pub fn normalize_float_bar_information_mode(value: &str) -> String {
    match value {
        "calm" => "calm".to_string(),
        _ => "exact".to_string(),
    }
}

/// Normalize the resolved contrast mode used by the desktop bridge.
pub fn normalize_float_bar_contrast(value: &str) -> String {
    match value {
        "light-text" => "light-text".to_string(),
        "dark-text" => "dark-text".to_string(),
        _ => "auto".to_string(),
    }
}

/// Resolve upgraded settings without changing their previous light/dark text
/// choice. Fresh defaults carry an explicit automatic mode.
pub fn resolved_float_bar_contrast(settings: &Settings) -> String {
    settings
        .float_bar_contrast
        .as_deref()
        .map(normalize_float_bar_contrast)
        .unwrap_or_else(|| {
            if settings.float_bar_dark_text {
                "dark-text".to_string()
            } else {
                "light-text".to_string()
            }
        })
}

/// Canonicalize a requested provider display order.
///
/// Keeps requested provider IDs that map to a real [`ProviderId`], drops
/// duplicates, and appends omitted providers in canonical order. An empty
/// request intentionally returns the full canonical order so display callers
/// can use one path for default and customized ordering.
pub fn normalize_provider_order(requested: &[String]) -> Vec<String> {
    let canonical = ProviderId::all()
        .iter()
        .map(|provider| provider.cli_name().to_string())
        .collect::<Vec<_>>();
    let valid = canonical.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(canonical.len());

    for provider_id in requested {
        if valid.contains(provider_id.as_str()) && seen.insert(provider_id.clone()) {
            out.push(provider_id.clone());
        }
    }
    for provider_id in canonical {
        if seen.insert(provider_id.clone()) {
            out.push(provider_id);
        }
    }

    out
}

fn default_global_shortcut() -> String {
    "Ctrl+Shift+U".to_string()
}

fn default_taskbar_toggle_shortcut() -> String {
    "Ctrl+Shift+H".to_string()
}

fn default_true() -> bool {
    true
}

/// Default cookie source value for browser-authenticated providers.
///
/// Browser cookie extraction reads browser profile databases and decrypts
/// Chromium cookies via Windows DPAPI, which can trigger behavior-based AV
/// engines. Keep that path explicit opt-in by default.
const DEFAULT_COOKIE_SOURCE: &str = "manual";

/// Default usage source value for any provider.
const DEFAULT_PROVIDER_SOURCE: &str = "auto";

/// Default API region for providers that expose one.
fn default_api_region(id: ProviderId) -> &'static str {
    match id {
        ProviderId::Alibaba => crate::providers::AlibabaRegion::Singapore.settings_value(),
        ProviderId::Zai | ProviderId::MiniMax => "global",
        _ => "",
    }
}

/// Default for the codex `openai_web_extras` boolean (true = show extras).
const DEFAULT_CODEX_OPENAI_WEB_EXTRAS: bool = true;
const DEFAULT_CODEX_SPARK_USAGE_VISIBLE: bool = true;

pub fn default_spend_budget_period() -> String {
    "daily".to_string()
}

pub const fn default_spend_budget_warning_usd() -> f64 {
    5.0
}

pub const fn default_spend_budget_limit_usd() -> f64 {
    15.0
}

/// 3x the recent median. Day-to-day spend on an active machine swings by well
/// over 2x on its own, so a lower factor would fire on an ordinary busy day and
/// train the user to ignore it.
pub const fn default_spend_anomaly_multiplier() -> f64 {
    3.0
}

/// Clamp the spike factor to a band where the alert still means something: at
/// 1x every day above the median fires, and past 20x a real runaway loop could
/// finish before the threshold is reached.
pub fn normalize_spend_anomaly_multiplier(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(1.5, 20.0)
    } else {
        default_spend_anomaly_multiplier()
    }
}

pub fn normalize_spend_budget_period(value: &str) -> String {
    match value {
        "monthly" => "monthly".to_string(),
        _ => default_spend_budget_period(),
    }
}

pub fn normalize_spend_budget_usd(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1_000_000.0)
    } else {
        0.0
    }
}

impl Default for Settings {
    fn default() -> Self {
        let mut enabled = HashSet::new();
        // Default enabled providers (first-class local CLI companions)
        enabled.insert("claude".to_string());
        enabled.insert("codex".to_string());
        enabled.insert("cursor".to_string());
        enabled.insert("grok".to_string());

        Self {
            enabled_providers: enabled,
            refresh_interval_secs: 300, // 5 minutes
            refresh_all_providers_on_menu_open: false,
            start_minimized: false,
            start_at_login: false,
            show_notifications: true,
            capacity_event_notifications_enabled: true,
            sound_enabled: true,
            sound_volume: 100,
            high_usage_threshold: 85.0,
            critical_usage_threshold: 90.0,
            spend_budget_alerts_enabled: false,
            spend_budget_period: default_spend_budget_period(),
            spend_budget_warning_usd: default_spend_budget_warning_usd(),
            spend_budget_limit_usd: default_spend_budget_limit_usd(),
            spend_anomaly_alerts_enabled: false,
            spend_anomaly_multiplier: default_spend_anomaly_multiplier(),
            provider_incident_badges_enabled: false,
            notification_policy_version: NOTIFICATION_POLICY_VERSION,
            provider_usage_thresholds: HashMap::new(),
            switcher_shows_icons: true,
            menu_bar_shows_highest_usage: false,
            menu_bar_shows_percent: false,
            show_as_used: true,        // Show as "used" by default
            enable_animations: true,   // Animations enabled by default
            reset_time_relative: true, // Show relative times by default
            show_reset_when_exhausted: false,
            predictive_pace_warning_enabled: false,
            menu_bar_display_mode: "detailed".to_string(), // Detailed mode by default
            show_all_token_accounts_in_menu: false,
            provider_configs: HashMap::new(),
            unrecognized_provider_configs: HashMap::new(),
            disable_keychain_access: false,
            hide_personal_info: false, // Show personal info by default
            update_channel: UpdateChannel::default(), // Stable by default
            provider_metrics: HashMap::new(), // Empty = use Automatic for all
            provider_order: Vec::new(), // Empty = canonical ProviderId::all() order
            global_shortcut: default_global_shortcut(), // Ctrl+Shift+U by default
            taskbar_toggle_shortcut: default_taskbar_toggle_shortcut(), // Ctrl+Shift+H by default
            codex_custom_sessions_dirs: Vec::new(),
            agent_sessions_enabled: false,
            agent_session_ssh_hosts: Vec::new(),
            auto_download_updates: false, // Require explicit opt-in for background downloads
            install_updates_on_quit: false, // Don't auto-install on quit by default
            ui_language: Language::default(), // English by default
            theme: ThemePreference::default(), // Auto (follows prefers-color-scheme)
            window_scale_percent: default_window_scale_percent(),
            tray_scale_percent: default_tray_scale_percent(),
            powertoys_status_pipe_enabled: false,
            float_bar_enabled: false,
            taskbar_widget_enabled: true,
            taskbar_widget_all_monitors: false,
            float_bar_opacity: default_float_bar_opacity(),
            float_bar_scale: default_float_bar_scale(),
            float_bar_orientation: default_float_bar_orientation(),
            float_bar_style: "floating".to_string(),
            taskbar_widget_open_on_hover: true,
            float_bar_density: default_float_bar_density(),
            float_bar_information_mode: default_float_bar_information_mode(),
            float_bar_selection_mode: default_float_bar_selection_mode(),
            float_bar_foreground_detection: true,
            float_bar_contrast: Some("auto".to_string()),
            float_bar_click_through: false,
            float_bar_provider_ids: Vec::new(),
            taskbar_account_by_provider: std::collections::HashMap::new(),
            float_bar_dark_text: false,
            float_bar_show_reset_inline: true,
            float_bar_show_cost: false,
        }
    }
}

impl Settings {
    /// Apply a mutation to the latest settings while holding the shared
    /// cross-process lock for the complete read-modify-write cycle.
    pub fn update(operation: impl FnOnce(&mut Self)) -> anyhow::Result<Self> {
        Self::try_update(|settings| {
            operation(settings);
            Ok(())
        })
        .map(|(settings, ())| settings)
    }

    pub fn try_update<T>(
        operation: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> anyhow::Result<(Self, T)> {
        crate::secure_file::with_state_write_lock(|| {
            let mut settings = Self::load_unlocked();
            let result = operation(&mut settings).map_err(std::io::Error::other)?;
            settings.save_unlocked().map_err(std::io::Error::other)?;
            Ok((settings, result))
        })
        .map_err(Into::into)
    }

    /// Preferred strip account for `provider_id`, if the user pinned one.
    pub fn taskbar_account_for(&self, provider_id: &str) -> Option<&str> {
        self.taskbar_account_by_provider
            .get(provider_id)
            .map(String::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
    }

    /// Get the settings file path
    pub fn settings_path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("Ceiling").join("settings.json"))
    }

    pub(crate) fn backup_path(path: &std::path::Path) -> PathBuf {
        path.with_extension("json.bak")
    }

    /// Load settings from disk
    pub fn load() -> Self {
        let (settings, pending_quarantine) = Self::load_from_disk(false);
        if !pending_quarantine && !settings.has_legacy_credentials() {
            return settings;
        }

        // Loading can become a write: an older file may still embed
        // credentials, and SBS-954's quarantine rename is a write too.
        // Re-read after taking the lock so those writes cannot move a
        // concurrent try_update repair to settings.json.bak (SBS-1029).
        match crate::secure_file::with_state_write_lock(|| Ok(Self::load_unlocked())) {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(%error, "Failed to lock settings load that may write");
                settings
            }
        }
    }

    /// Load and migrate settings while the caller holds the state write lock.
    fn load_unlocked() -> Self {
        let (mut settings, _) = Self::load_from_disk(true);

        if let Some(sanitized) = settings.migrate_legacy_credentials() {
            match sanitized.and_then(|sanitized| {
                Self::write_to_disk(&sanitized)?;
                Ok(sanitized)
            }) {
                Ok(sanitized) => settings = sanitized,
                Err(error) => {
                    tracing::warn!(%error, "Failed to migrate legacy settings credentials");
                }
            }
        }

        settings
    }

    /// Read `settings.json`. `allow_quarantine` is the rename from SBS-954;
    /// only the locked path may set it. The second value is `true` when the
    /// file existed, failed to parse, and was left in place so `load` can
    /// retry under the state lock (SBS-1029).
    fn load_from_disk(allow_quarantine: bool) -> (Self, bool) {
        let mut pending_quarantine = false;
        #[allow(unused_mut)]
        let mut settings = match Self::settings_path() {
            Some(path) if path.exists() => {
                let (settings, pending) = Self::read_path(&path, allow_quarantine);
                pending_quarantine = pending;
                settings
            }
            _ => Self::default(),
        };

        // Sync the toggle with whether Run\Ceiling exists. Repair only when
        // this process owns that entry (SBS-1053).
        #[cfg(target_os = "windows")]
        {
            settings.start_at_login = Self::sync_start_at_login_registry();
        }

        (settings, pending_quarantine)
    }

    fn read_path(path: &std::path::Path, allow_quarantine: bool) -> (Self, bool) {
        match crate::secure_file::read_string(path) {
            Ok(content) => match Self::parse_settings_json(&content) {
                Ok(settings) => (settings, false),
                Err(error) if allow_quarantine => {
                    Self::quarantine_unparseable(path, &error);
                    (Self::default(), false)
                }
                Err(_) => (Self::default(), true),
            },
            Err(error) => {
                tracing::warn!(%error, "settings.json could not be read; using defaults");
                (Self::default(), false)
            }
        }
    }

    fn parse_settings_json(content: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(content.trim_start_matches('\u{feff}'))
    }

    /// Parse `settings.json`, or move a corrupt file aside so the next save
    /// cannot overwrite the user's last known contents.
    ///
    /// The caller must hold the state write lock. Renaming without it can
    /// move a concurrent [`Self::try_update`] repair to `.bak` (SBS-1029).
    pub(crate) fn parse_or_quarantine(path: &std::path::Path, content: &str) -> Self {
        match Self::parse_settings_json(content) {
            Ok(settings) => settings,
            Err(error) => {
                Self::quarantine_unparseable(path, &error);
                Self::default()
            }
        }
    }

    fn quarantine_unparseable(path: &std::path::Path, error: &serde_json::Error) {
        let backup = Self::backup_path(path);
        match std::fs::rename(path, &backup) {
            Ok(()) => tracing::warn!(
                %error,
                backup = %backup.display(),
                "settings.json could not be parsed; original moved aside and defaults loaded"
            ),
            Err(rename_error) => tracing::warn!(
                %error,
                %rename_error,
                "settings.json could not be parsed; falling back to defaults without a backup"
            ),
        }
    }

    /// Save settings to disk
    pub fn save(&self) -> anyhow::Result<()> {
        crate::secure_file::with_state_write_lock(|| {
            self.save_unlocked().map_err(std::io::Error::other)
        })
        .map_err(Into::into)
    }

    fn save_unlocked(&self) -> anyhow::Result<()> {
        let sanitized = match self.migrate_legacy_credentials() {
            Some(result) => result?,
            None => self.clone(),
        };
        Self::write_to_disk(&sanitized)
    }

    fn write_to_disk(settings: &Self) -> anyhow::Result<()> {
        let path = Self::settings_path()
            .ok_or_else(|| anyhow::anyhow!("Could not determine settings path"))?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(settings)?;
        crate::secure_file::write_string(&path, &json)?;

        Ok(())
    }

    /// Move credentials embedded by older releases into the dedicated secure
    /// stores. Existing secure-store entries win so a stale settings file can
    /// never overwrite a newer credential.
    fn migrate_legacy_credentials(&self) -> Option<anyhow::Result<Self>> {
        // Unrecognized ids are included: `manual_cookie_header` and `api_token`
        // are `skip_serializing`, so an inline credential under a provider this
        // build cannot resolve would be read in, never migrated, and dropped by
        // the next save — data loss inside the path that parks unknown ids to
        // prevent exactly that.
        if !self.has_legacy_credentials() {
            return None;
        }

        Some((|| {
            // Unlike ordinary runtime reads, migration must fail closed when
            // an existing secure store cannot be decoded. Treating that store
            // as empty could replace a newer credential with a stale one.
            let mut manual_cookies = ManualCookies::try_load()?;
            let mut api_keys = ApiKeys::try_load()?;
            let mut cookies_changed = false;
            let mut keys_changed = false;
            let mut sanitized = self.clone();

            // The secure stores are keyed by string, so an unresolvable id
            // migrates the same way a known one does.
            let legacy_entries = self
                .provider_configs
                .iter()
                .map(|(provider, config)| (provider.cli_name(), config))
                .chain(
                    self.unrecognized_provider_configs
                        .iter()
                        .map(|(id, config)| (id.as_str(), config)),
                );

            for (provider_id, config) in legacy_entries {
                if let Some(cookie_header) = legacy_credential_to_migrate(
                    config.manual_cookie_header.as_deref(),
                    manual_cookies.get(provider_id),
                ) {
                    manual_cookies.set(provider_id, cookie_header);
                    cookies_changed = true;
                }
                if let Some(api_token) = legacy_credential_to_migrate(
                    config.api_token.as_deref(),
                    api_keys.get(provider_id),
                ) {
                    api_keys.set(provider_id, api_token, Some("Migrated from settings"));
                    keys_changed = true;
                }
            }

            for config in sanitized
                .provider_configs
                .values_mut()
                .chain(sanitized.unrecognized_provider_configs.values_mut())
            {
                config.manual_cookie_header = None;
                config.api_token = None;
            }

            // Do not sanitize settings.json until every changed secure store
            // has been written successfully. A partial failure is safe to
            // retry because existing secure-store values remain authoritative.
            if cookies_changed {
                let path = ManualCookies::cookies_path()
                    .ok_or_else(|| anyhow::anyhow!("Could not determine cookies path"))?;
                manual_cookies.save_to(&path)?;
            }
            if keys_changed {
                let path = ApiKeys::keys_path()
                    .ok_or_else(|| anyhow::anyhow!("Could not determine API keys path"))?;
                api_keys.save_to(&path)?;
            }
            Ok(sanitized)
        })())
    }

    fn has_legacy_credentials(&self) -> bool {
        self.provider_configs
            .values()
            .chain(self.unrecognized_provider_configs.values())
            .any(carries_legacy_credentials)
    }

    fn start_at_login_exe_path(current_exe: &std::path::Path) -> std::path::PathBuf {
        let file_name = current_exe.file_name().and_then(|name| name.to_str());
        if file_name.is_some_and(|name| {
            name.eq_ignore_ascii_case("codexbar-cli.exe")
                || name.eq_ignore_ascii_case("codexbar-desktop.exe")
        }) && let Some(desktop_exe) = current_exe
            .parent()
            .map(|dir| dir.join("ceiling.exe"))
            .filter(|path| path.exists())
        {
            return desktop_exe;
        }

        current_exe.to_path_buf()
    }

    fn start_at_login_command(current_exe: &std::path::Path) -> String {
        let exe_path = Self::start_at_login_exe_path(current_exe);
        format!("\"{}\"", exe_path.display())
    }

    /// Rewrite `Run\Ceiling` only when this process owns that entry and the
    /// value is a bare stale path for this tree. Custom arguments and other
    /// install trees are left alone (SBS-1053).
    fn start_at_login_command_needs_repair(existing: &str, current_exe: &std::path::Path) -> bool {
        let intended = Self::start_at_login_exe_path(current_exe);
        let Some((existing_exe, extras)) = parse_start_at_login_command(existing) else {
            return false;
        };
        if !extras.trim().is_empty() {
            return false;
        }
        if !owns_start_at_login_entry(std::path::Path::new(&existing_exe), &intended) {
            return false;
        }
        existing != Self::start_at_login_command(current_exe)
    }

    fn start_at_login_command_is_owned(existing: &str, current_exe: &std::path::Path) -> bool {
        let intended = Self::start_at_login_exe_path(current_exe);
        parse_start_at_login_command(existing).is_some_and(|(existing_exe, _)| {
            owns_start_at_login_entry(std::path::Path::new(&existing_exe), &intended)
        })
    }

    #[cfg(target_os = "windows")]
    pub fn apply_start_at_login_registry(enabled: bool) -> anyhow::Result<()> {
        use winreg::RegKey;
        use winreg::enums::*;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let run_key = hkcu.open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Run",
            KEY_READ | KEY_WRITE,
        )?;

        if enabled {
            let exe_path = std::env::current_exe()?;
            let command = Self::start_at_login_command(&exe_path);
            run_key.set_value("Ceiling", &command)?;
        } else {
            let existing = run_key.get_value::<String, _>("Ceiling").ok();
            let current_exe = std::env::current_exe()?;
            if existing
                .as_deref()
                .is_some_and(|command| Self::start_at_login_command_is_owned(command, &current_exe))
            {
                let _ = run_key.delete_value("Ceiling");
            }
        }

        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn sync_start_at_login_registry() -> bool {
        use winreg::RegKey;
        use winreg::enums::*;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let Ok(run_key) = hkcu.open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Run",
            KEY_READ | KEY_WRITE,
        ) else {
            return false;
        };

        let Ok(existing) = run_key.get_value::<String, _>("Ceiling") else {
            return false;
        };

        let exe_path = match std::env::current_exe() {
            Ok(exe_path) => exe_path,
            Err(error) => {
                tracing::warn!(
                    "Failed to resolve current executable for start-at-login sync: {error}"
                );
                return false;
            }
        };
        if !Self::start_at_login_command_is_owned(&existing, &exe_path) {
            return false;
        }
        if Self::start_at_login_command_needs_repair(&existing, &exe_path) {
            let command = Self::start_at_login_command(&exe_path);
            if let Err(error) = run_key.set_value("Ceiling", &command) {
                tracing::warn!("Failed to repair Ceiling start-at-login command: {error}");
            }
        }

        true
    }

    #[cfg(not(target_os = "windows"))]
    pub fn apply_start_at_login_registry(_enabled: bool) -> anyhow::Result<()> {
        Ok(())
    }

    /// Set start at login (updates Windows registry)
    pub fn set_start_at_login(&mut self, enabled: bool) -> anyhow::Result<()> {
        self.start_at_login = enabled;
        Self::apply_start_at_login_registry(enabled)?;
        Ok(())
    }

    /// Check if start at login is actually enabled in registry
    #[cfg(target_os = "windows")]
    pub fn is_start_at_login_enabled() -> bool {
        use winreg::RegKey;
        use winreg::enums::*;

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(run_key) = hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run") {
            let existing = run_key.get_value::<String, _>("Ceiling").ok();
            let current_exe = std::env::current_exe().ok();
            existing
                .zip(current_exe)
                .is_some_and(|(command, exe)| Self::start_at_login_command_is_owned(&command, &exe))
        } else {
            false
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub fn is_start_at_login_enabled() -> bool {
        false
    }

    /// Check if a provider is enabled
    pub fn is_provider_enabled(&self, id: ProviderId) -> bool {
        self.enabled_providers.contains(id.cli_name())
    }

    /// Enable a provider
    pub fn enable_provider(&mut self, id: ProviderId) {
        self.enabled_providers.insert(id.cli_name().to_string());
    }

    /// Disable a provider
    pub fn disable_provider(&mut self, id: ProviderId) {
        self.enabled_providers.remove(id.cli_name());
    }

    /// Toggle a provider's enabled state
    pub fn toggle_provider(&mut self, id: ProviderId) -> bool {
        let name = id.cli_name().to_string();
        if self.enabled_providers.contains(&name) {
            self.enabled_providers.remove(&name);
            false
        } else {
            self.enabled_providers.insert(name);
            true
        }
    }

    /// Get list of enabled provider IDs
    pub fn get_enabled_provider_ids(&self) -> Vec<ProviderId> {
        self.provider_display_order()
            .into_iter()
            .filter(|id| self.is_provider_enabled(*id))
            .collect()
    }

    /// Get all available providers with their enabled status
    pub fn get_all_providers_status(&self) -> Vec<ProviderStatus> {
        self.provider_display_order()
            .into_iter()
            .map(|id| ProviderStatus {
                id: id.cli_name().to_string(),
                name: id.display_name().to_string(),
                enabled: self.is_provider_enabled(id),
            })
            .collect()
    }

    /// Provider display order as typed IDs, falling back to canonical order
    /// when no custom order has been persisted.
    pub fn provider_display_order(&self) -> Vec<ProviderId> {
        normalize_provider_order(&self.provider_order)
            .into_iter()
            .filter_map(|provider_id| ProviderId::from_cli_name(&provider_id))
            .collect()
    }

    /// Provider display order as CLI-name strings.
    pub fn provider_display_order_names(&self) -> Vec<String> {
        normalize_provider_order(&self.provider_order)
    }

    /// Get the metric preference for a provider
    pub fn get_provider_metric(&self, id: ProviderId) -> MetricPreference {
        self.provider_metrics
            .get(id.cli_name())
            .copied()
            .unwrap_or_default()
    }

    /// Set the metric preference for a provider
    pub fn set_provider_metric(&mut self, id: ProviderId, metric: MetricPreference) {
        self.provider_metrics
            .insert(id.cli_name().to_string(), metric);
    }

    // ── Per-provider configuration accessors ─────────────────────────
    //
    // These thin wrappers around `provider_configs` apply provider-specific
    // defaults (e.g. cookie/usage source defaults to `"auto"`) so callers
    // never have to reach into the raw `Option<String>` fields. The
    // `*_str` / boolean / setter pairs intentionally mirror the names of
    // the legacy flat fields so call-site migration is mechanical.

    /// Read-only access to a provider's stored config, if any.
    pub fn provider_config(&self, id: ProviderId) -> Option<&ProviderConfig> {
        self.provider_configs.get(&id)
    }

    /// Mutable access to a provider's config, lazily creating an empty
    /// entry if none exists.
    pub fn provider_config_mut(&mut self, id: ProviderId) -> &mut ProviderConfig {
        self.provider_configs.entry(id).or_default()
    }

    /// Cookie source for `id`, or a provider-specific default if unset.
    ///
    /// Cursor defaults to Automatic so Ceiling can use the signed-in IDE session
    /// on disk (and browser cookies when available). Other cookie providers still
    /// default to Manual.
    pub fn cookie_source(&self, id: ProviderId) -> &str {
        self.provider_configs
            .get(&id)
            .and_then(|c| c.cookie_source.as_deref())
            .unwrap_or(if id == ProviderId::Cursor {
                "auto"
            } else {
                DEFAULT_COOKIE_SOURCE
            })
    }

    pub fn set_cookie_source(&mut self, id: ProviderId, source: impl Into<String>) {
        self.provider_config_mut(id).cookie_source = Some(source.into());
    }

    /// Usage source for `id`, or the default `"auto"` if unset.
    pub fn usage_source(&self, id: ProviderId) -> &str {
        self.provider_configs
            .get(&id)
            .and_then(|c| c.usage_source.as_deref())
            .unwrap_or(DEFAULT_PROVIDER_SOURCE)
    }

    pub fn set_usage_source(&mut self, id: ProviderId, source: impl Into<String>) {
        self.provider_config_mut(id).usage_source = Some(source.into());
    }

    /// API region for `id`, or the provider-specific default if unset.
    pub fn api_region(&self, id: ProviderId) -> &str {
        self.provider_configs
            .get(&id)
            .and_then(|c| c.api_region.as_deref())
            .unwrap_or_else(|| default_api_region(id))
    }

    pub fn set_api_region(&mut self, id: ProviderId, region: impl Into<String>) {
        self.provider_config_mut(id).api_region = Some(region.into());
    }

    /// Manual cookie header for `id`, or `""` if unset.
    pub fn manual_cookie_header(&self, id: ProviderId) -> &str {
        self.provider_configs
            .get(&id)
            .and_then(|c| c.manual_cookie_header.as_deref())
            .unwrap_or("")
    }

    pub fn set_manual_cookie_header(&mut self, id: ProviderId, header: impl Into<String>) {
        self.provider_config_mut(id).manual_cookie_header = Some(header.into());
    }

    /// API token for `id`, or `""` if unset.
    pub fn api_token(&self, id: ProviderId) -> &str {
        self.provider_configs
            .get(&id)
            .and_then(|c| c.api_token.as_deref())
            .unwrap_or("")
    }

    pub fn set_api_token(&mut self, id: ProviderId, token: impl Into<String>) {
        self.provider_config_mut(id).api_token = Some(token.into());
    }

    /// Workspace ID override for `id`, or `""` if unset.
    pub fn workspace_id(&self, id: ProviderId) -> &str {
        self.provider_configs
            .get(&id)
            .and_then(|c| c.workspace_id.as_deref())
            .unwrap_or("")
    }

    pub fn set_workspace_id(&mut self, id: ProviderId, value: impl Into<String>) {
        self.provider_config_mut(id).workspace_id = Some(value.into());
    }

    /// Wayfinder gateway URL, defaulting to the local loopback gateway.
    pub fn gateway_url(&self, id: ProviderId) -> &str {
        self.provider_configs
            .get(&id)
            .and_then(|c| c.gateway_url.as_deref())
            .unwrap_or_else(|| {
                if id == ProviderId::Wayfinder {
                    crate::providers::wayfinder::DEFAULT_GATEWAY_URL
                } else {
                    ""
                }
            })
    }

    pub fn set_gateway_url(&mut self, id: ProviderId, value: impl Into<String>) {
        self.provider_config_mut(id).gateway_url = Some(value.into());
    }

    /// IDE base path override for `id`, or `""` if unset.
    pub fn ide_base_path(&self, id: ProviderId) -> &str {
        self.provider_configs
            .get(&id)
            .and_then(|c| c.ide_base_path.as_deref())
            .unwrap_or("")
    }

    pub fn set_ide_base_path(&mut self, id: ProviderId, value: impl Into<String>) {
        self.provider_config_mut(id).ide_base_path = Some(value.into());
    }

    /// Codex `openai_web_extras` toggle, default `true`.
    pub fn openai_web_extras(&self, id: ProviderId) -> bool {
        self.provider_configs
            .get(&id)
            .and_then(|c| c.openai_web_extras)
            .unwrap_or(DEFAULT_CODEX_OPENAI_WEB_EXTRAS)
    }

    pub fn set_openai_web_extras(&mut self, id: ProviderId, value: bool) {
        self.provider_config_mut(id).openai_web_extras = Some(value);
    }

    /// Codex Spark rows are visible by default.
    pub fn spark_usage_visible(&self, id: ProviderId) -> bool {
        self.provider_configs
            .get(&id)
            .and_then(|c| c.spark_usage_visible)
            .unwrap_or(DEFAULT_CODEX_SPARK_USAGE_VISIBLE)
    }

    pub fn set_spark_usage_visible(&mut self, id: ProviderId, value: bool) {
        self.provider_config_mut(id).spark_usage_visible = Some(value);
    }

    /// Per-provider historical-tracking toggle (currently codex-only).
    pub fn historical_tracking(&self, id: ProviderId) -> bool {
        self.provider_configs
            .get(&id)
            .map(|c| c.historical_tracking)
            .unwrap_or(false)
    }

    pub fn set_historical_tracking(&mut self, id: ProviderId, value: bool) {
        self.provider_config_mut(id).historical_tracking = value;
    }

    /// SBS-1023: master switch. When false, no keyring read or write runs.
    pub fn keychain_access_allowed(&self) -> bool {
        !self.disable_keychain_access
    }

    /// SBS-1023: Claude keyring reads and token writes.
    pub fn claude_keychain_access_allowed(&self) -> bool {
        self.keychain_access_allowed() && !self.claude_avoid_keychain_prompts()
    }

    /// Per-provider "avoid keychain prompts" toggle (currently claude-only).
    pub fn avoid_keychain_prompts(&self, id: ProviderId) -> bool {
        self.provider_configs
            .get(&id)
            .map(|c| c.avoid_keychain_prompts)
            .unwrap_or(false)
    }

    pub fn set_avoid_keychain_prompts(&mut self, id: ProviderId, value: bool) {
        self.provider_config_mut(id).avoid_keychain_prompts = value;
    }

    // ── Legacy field-name aliases ────────────────────────────────────
    //
    // Keep the names of the old flat per-provider fields available as
    // accessor methods so existing call sites only need a `()` (read) or
    // `set_` prefix (write). New code should prefer the typed accessors
    // above.

    pub fn codex_cookie_source(&self) -> &str {
        self.cookie_source(ProviderId::Codex)
    }
    pub fn set_codex_cookie_source(&mut self, v: impl Into<String>) {
        self.set_cookie_source(ProviderId::Codex, v)
    }
    pub fn claude_cookie_source(&self) -> &str {
        self.cookie_source(ProviderId::Claude)
    }
    pub fn set_claude_cookie_source(&mut self, v: impl Into<String>) {
        self.set_cookie_source(ProviderId::Claude, v)
    }
    pub fn cursor_cookie_source(&self) -> &str {
        self.cookie_source(ProviderId::Cursor)
    }
    pub fn set_cursor_cookie_source(&mut self, v: impl Into<String>) {
        self.set_cookie_source(ProviderId::Cursor, v)
    }
    pub fn opencode_cookie_source(&self) -> &str {
        self.cookie_source(ProviderId::OpenCode)
    }
    pub fn set_opencode_cookie_source(&mut self, v: impl Into<String>) {
        self.set_cookie_source(ProviderId::OpenCode, v)
    }
    pub fn factory_cookie_source(&self) -> &str {
        self.cookie_source(ProviderId::Factory)
    }
    pub fn set_factory_cookie_source(&mut self, v: impl Into<String>) {
        self.set_cookie_source(ProviderId::Factory, v)
    }
    pub fn alibaba_cookie_source(&self) -> &str {
        self.cookie_source(ProviderId::Alibaba)
    }
    pub fn set_alibaba_cookie_source(&mut self, v: impl Into<String>) {
        self.set_cookie_source(ProviderId::Alibaba, v)
    }
    pub fn kimi_cookie_source(&self) -> &str {
        self.cookie_source(ProviderId::Kimi)
    }
    pub fn set_kimi_cookie_source(&mut self, v: impl Into<String>) {
        self.set_cookie_source(ProviderId::Kimi, v)
    }
    pub fn minimax_cookie_source(&self) -> &str {
        self.cookie_source(ProviderId::MiniMax)
    }
    pub fn set_minimax_cookie_source(&mut self, v: impl Into<String>) {
        self.set_cookie_source(ProviderId::MiniMax, v)
    }
    pub fn augment_cookie_source(&self) -> &str {
        self.cookie_source(ProviderId::Augment)
    }
    pub fn set_augment_cookie_source(&mut self, v: impl Into<String>) {
        self.set_cookie_source(ProviderId::Augment, v)
    }
    pub fn amp_cookie_source(&self) -> &str {
        self.cookie_source(ProviderId::Amp)
    }
    pub fn set_amp_cookie_source(&mut self, v: impl Into<String>) {
        self.set_cookie_source(ProviderId::Amp, v)
    }
    pub fn ollama_cookie_source(&self) -> &str {
        self.cookie_source(ProviderId::Ollama)
    }
    pub fn set_ollama_cookie_source(&mut self, v: impl Into<String>) {
        self.set_cookie_source(ProviderId::Ollama, v)
    }

    pub fn claude_usage_source(&self) -> &str {
        self.usage_source(ProviderId::Claude)
    }
    pub fn set_claude_usage_source(&mut self, v: impl Into<String>) {
        self.set_usage_source(ProviderId::Claude, v)
    }
    pub fn codex_usage_source(&self) -> &str {
        self.usage_source(ProviderId::Codex)
    }
    pub fn set_codex_usage_source(&mut self, v: impl Into<String>) {
        self.set_usage_source(ProviderId::Codex, v)
    }

    pub fn alibaba_api_region(&self) -> &str {
        self.api_region(ProviderId::Alibaba)
    }
    pub fn set_alibaba_api_region(&mut self, v: impl Into<String>) {
        self.set_api_region(ProviderId::Alibaba, v)
    }
    pub fn zai_api_region(&self) -> &str {
        self.api_region(ProviderId::Zai)
    }
    pub fn set_zai_api_region(&mut self, v: impl Into<String>) {
        self.set_api_region(ProviderId::Zai, v)
    }
    pub fn minimax_api_region(&self) -> &str {
        self.api_region(ProviderId::MiniMax)
    }
    pub fn set_minimax_api_region(&mut self, v: impl Into<String>) {
        self.set_api_region(ProviderId::MiniMax, v)
    }

    pub fn alibaba_cookie_header(&self) -> &str {
        self.manual_cookie_header(ProviderId::Alibaba)
    }
    pub fn set_alibaba_cookie_header(&mut self, v: impl Into<String>) {
        self.set_manual_cookie_header(ProviderId::Alibaba, v)
    }
    pub fn kimi_manual_cookie_header(&self) -> &str {
        self.manual_cookie_header(ProviderId::Kimi)
    }
    pub fn set_kimi_manual_cookie_header(&mut self, v: impl Into<String>) {
        self.set_manual_cookie_header(ProviderId::Kimi, v)
    }
    pub fn augment_cookie_header(&self) -> &str {
        self.manual_cookie_header(ProviderId::Augment)
    }
    pub fn set_augment_cookie_header(&mut self, v: impl Into<String>) {
        self.set_manual_cookie_header(ProviderId::Augment, v)
    }
    pub fn amp_cookie_header(&self) -> &str {
        self.manual_cookie_header(ProviderId::Amp)
    }
    pub fn set_amp_cookie_header(&mut self, v: impl Into<String>) {
        self.set_manual_cookie_header(ProviderId::Amp, v)
    }
    pub fn ollama_cookie_header(&self) -> &str {
        self.manual_cookie_header(ProviderId::Ollama)
    }
    pub fn set_ollama_cookie_header(&mut self, v: impl Into<String>) {
        self.set_manual_cookie_header(ProviderId::Ollama, v)
    }
    pub fn minimax_cookie_header(&self) -> &str {
        self.manual_cookie_header(ProviderId::MiniMax)
    }
    pub fn set_minimax_cookie_header(&mut self, v: impl Into<String>) {
        self.set_manual_cookie_header(ProviderId::MiniMax, v)
    }

    pub fn opencode_workspace_id(&self) -> &str {
        self.workspace_id(ProviderId::OpenCode)
    }
    pub fn set_opencode_workspace_id(&mut self, v: impl Into<String>) {
        self.set_workspace_id(ProviderId::OpenCode, v)
    }
    pub fn minimax_api_token(&self) -> &str {
        self.api_token(ProviderId::MiniMax)
    }
    pub fn set_minimax_api_token(&mut self, v: impl Into<String>) {
        self.set_api_token(ProviderId::MiniMax, v)
    }
    pub fn jetbrains_ide_base_path(&self) -> &str {
        self.ide_base_path(ProviderId::JetBrains)
    }
    pub fn set_jetbrains_ide_base_path(&mut self, v: impl Into<String>) {
        self.set_ide_base_path(ProviderId::JetBrains, v)
    }

    pub fn codex_openai_web_extras(&self) -> bool {
        self.openai_web_extras(ProviderId::Codex)
    }
    pub fn set_codex_openai_web_extras(&mut self, v: bool) {
        self.set_openai_web_extras(ProviderId::Codex, v)
    }
    pub fn codex_spark_usage_visible(&self) -> bool {
        self.spark_usage_visible(ProviderId::Codex)
    }
    pub fn set_codex_spark_usage_visible(&mut self, v: bool) {
        self.set_spark_usage_visible(ProviderId::Codex, v)
    }
    pub fn codex_historical_tracking(&self) -> bool {
        self.historical_tracking(ProviderId::Codex)
    }
    pub fn set_codex_historical_tracking(&mut self, v: bool) {
        self.set_historical_tracking(ProviderId::Codex, v)
    }
    pub fn claude_avoid_keychain_prompts(&self) -> bool {
        self.avoid_keychain_prompts(ProviderId::Claude)
    }
    pub fn set_claude_avoid_keychain_prompts(&mut self, v: bool) {
        self.set_avoid_keychain_prompts(ProviderId::Claude, v)
    }
}
