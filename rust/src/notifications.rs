//! System notifications for Ceiling
//!
//! Provides Windows toast notifications for usage alerts

#![allow(dead_code)]

use crate::core::ProviderId;
use crate::core::{RateWindow, UsagePace};
use crate::locale::{self, LocaleKey};
use crate::settings::{Settings, normalize_spend_anomaly_multiplier};
use crate::sound::{AlertSound, play_alert};
use chrono::{DateTime, Utc};

/// Notification types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotificationType {
    /// Usage is approaching limit (high threshold)
    HighUsage,
    /// Usage is critical (critical threshold)
    CriticalUsage,
    /// Usage limit exhausted
    Exhausted,
    /// Provider status issue
    StatusIssue,
    /// Session quota depleted (at 100% usage)
    SessionDepleted,
    /// Session quota restored (back from 100%)
    SessionRestored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PredictiveWarningWindow {
    Session,
    Weekly,
    Monthly,
}

impl PredictiveWarningWindow {
    fn localized_label(self, language: crate::settings::Language) -> String {
        locale::get_text(
            language,
            match self {
                Self::Session => LocaleKey::ProviderSession,
                Self::Weekly => LocaleKey::ProviderWeekly,
                Self::Monthly => LocaleKey::ProviderMonthly,
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PredictiveResetWindow {
    window_minutes: Option<u32>,
    resets_at: DateTime<Utc>,
}

impl PredictiveResetWindow {
    fn belongs_to_same_cycle(&self, other: &Self) -> bool {
        if self.window_minutes != other.window_minutes {
            return false;
        }
        let tolerance_secs = self
            .window_minutes
            .map(|minutes| i64::from(minutes) * 30)
            .unwrap_or(300)
            .max(300);
        (self.resets_at - other.resets_at).num_seconds().abs() < tolerance_secs
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PredictiveWarningKey {
    provider: ProviderId,
    identity: String,
    window: PredictiveWarningWindow,
    reset: PredictiveResetWindow,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PredictiveObservationKey {
    provider: ProviderId,
    identity: String,
    window: PredictiveWarningWindow,
}

impl NotificationType {
    pub fn title(&self) -> &'static str {
        match self {
            NotificationType::HighUsage => "High Usage Warning",
            NotificationType::CriticalUsage => "Critical Usage Alert",
            NotificationType::Exhausted => "Usage Limit Reached",
            NotificationType::StatusIssue => "Provider Status Issue",
            NotificationType::SessionDepleted => "Session Depleted",
            NotificationType::SessionRestored => "Session Restored",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            NotificationType::HighUsage => "⚠️",
            NotificationType::CriticalUsage => "🔴",
            NotificationType::Exhausted => "🚫",
            NotificationType::StatusIssue => "⚡",
            NotificationType::SessionDepleted => "🔴",
            NotificationType::SessionRestored => "✅",
        }
    }
}

/// Drop below this offset under the high threshold before re-arming alerts.
/// Avoids flicker when a meter hovers around the boundary across refreshes.
const REARM_HYSTERESIS: f64 = 3.0;

/// Absolute safety limits for Windows toasts. A provider refresh may discover
/// several state changes at once, but Ceiling must never turn that into a toast
/// storm. Suppressed alerts are intentionally not replayed later.
const MIN_TOAST_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const TOAST_BURST_WINDOW: std::time::Duration = std::time::Duration::from_secs(60 * 60);
const MAX_TOASTS_PER_BURST_WINDOW: usize = 3;

/// Reset toasts may exceed the one-per-refresh rule, but not without bound. A
/// refresh reads a handful of providers, so anything past this is a bug rather
/// than several genuine resets landing together.
const MAX_RESET_TOASTS_PER_REFRESH: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToastPriority {
    Normal,
    /// A confirmed capacity reset. These are what users open Ceiling for, so
    /// they are exempt from the rate limits that exist to tame usage alerts.
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ThresholdAlertKey {
    provider: ProviderId,
    window: String,
    kind: NotificationType,
    /// See [`ThresholdWindowKey::account`]: alerts are per account, so one
    /// account's state cannot clear or suppress another's.
    account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ThresholdWindowKey {
    provider: ProviderId,
    window: String,
    /// Which account this limit belongs to. Two accounts on one provider hit
    /// their limits independently, and sharing a key meant a quiet account
    /// cleared a busy account's pending alert on the very same pass.
    account: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SpendBudgetAlertLevel {
    Warning,
    NearCap,
}

/// Below this, a day is too small to call a spike whatever the ratio. Cents
/// times a large multiplier are still cents, and an alert about them is noise.
const SPEND_ANOMALY_FLOOR_USD: f64 = 1.00;

/// Fewest floor-clearing days that can stand in as "the recent daily median".
/// One or two qualifying days are a sample, not a schedule: they become the
/// median of themselves and a first ordinary day back reads as a spike.
const SPEND_ANOMALY_MIN_USABLE_DAYS: usize = 3;

/// When every recent day is under the floor, there is no median. Going silent
/// hid a $180 loop from a $0.60/day user (SBS-967). This is the largest spike
/// factor the setting will accept, applied to the floor: high enough that an
/// ordinary first day, or the one-usable-day $12 case SBS-965 guards, does not
/// toast, and low enough that the $180 failure still does.
const SPEND_ANOMALY_ABSOLUTE_MULTIPLIER: f64 = 20.0;

fn spend_anomaly_absolute_trigger_usd() -> f64 {
    SPEND_ANOMALY_FLOOR_USD * SPEND_ANOMALY_ABSOLUTE_MULTIPLIER
}

/// How far today runs above the recent daily median.
///
/// `None` when the baseline is zero. A multiple of nothing is not a multiple,
/// so the ratio path stays off. `is_spend_spike` then falls back to an
/// absolute multiple of the floor: the budget alert is a separate opt-in and
/// cannot be this feature's only answer for a light user (SBS-967).
fn spend_spike_ratio(today_usd: f64, baseline_usd: f64) -> Option<f64> {
    (baseline_usd > 0.0).then(|| today_usd / baseline_usd)
}

fn is_spend_spike(today_usd: f64, baseline_usd: f64, multiplier: f64) -> bool {
    if today_usd < SPEND_ANOMALY_FLOOR_USD {
        return false;
    }
    if let Some(ratio) = spend_spike_ratio(today_usd, baseline_usd) {
        return ratio >= multiplier;
    }
    at_least_cents(today_usd, spend_anomaly_absolute_trigger_usd())
}

/// `value >= threshold`, compared at cent precision.
///
/// `today_usd` is a sum of f64 per-model estimates, so a day genuinely worth
/// $20.00 can land at 19.999999998 and miss a bare `>=` — while the toast
/// would have printed it as "$20.00" and the user would be looking at a
/// message that contradicts the threshold it claims to have crossed.
fn at_least_cents(value: f64, threshold: f64) -> bool {
    (value * 100.0).round() >= (threshold * 100.0).round()
}

fn spend_anomaly_toast_body(today_usd: f64, baseline_usd: f64) -> String {
    if let Some(ratio) = spend_spike_ratio(today_usd, baseline_usd) {
        format!(
            "Today's estimated API value is ${today_usd:.2}, about {ratio:.1}x your recent daily median of ${baseline_usd:.2}. This is an estimate from local Codex and Claude logs, not a bill."
        )
    } else {
        let trigger = spend_anomaly_absolute_trigger_usd();
        format!(
            "Today's estimated API value is ${today_usd:.2}, at or above the ${trigger:.2} absolute trigger used when there is no usable recent median. This is an estimate from local Codex and Claude logs, not a bill."
        )
    }
}

/// Median of the baseline days, which is what the spike is measured against.
///
/// The median rather than the mean: one heavy day last week would pull a mean
/// high enough to mask a real spike today, which is precisely the case the
/// alert exists for.
///
/// Days with no spend are left out entirely rather than counted as zero. A
/// normal three-day working week is `[0, 0, 0, 0, 20, 20, 20]`, whose median
/// including the zeros is 0 — and a zero baseline has no ratio to fire on, so
/// the most ordinary schedule there is would have had nothing to compare
/// against. An all-quiet week still has no positive days and still yields 0,
/// which is the genuine "no median" case; the absolute fallback in
/// `is_spend_spike` is what still hears a runaway then (SBS-967).
/// Baseline days must clear the same floor today does. A leftover five-cent day
/// beside one real $20 day would otherwise put the median at $10.02, turning an
/// ordinary $31 day into a 3x "spike" — and that false alarm also marks the day
/// sent, so the real runaway a few hours later says nothing.
///
/// Fewer than three surviving days is also nothing to compare against. One
/// light $1.50 day in an otherwise idle week becomes "the recent daily median",
/// so the first ordinary $12 day back reads as 8x and burns the day's only
/// alert. The existing zero-baseline path already treats that as no comparison
/// (SBS-965).
pub fn spend_baseline_usd(mut daily_usd: Vec<f64>) -> f64 {
    let usable: Vec<f64> = {
        daily_usd.retain(|value| value.is_finite() && *value >= SPEND_ANOMALY_FLOOR_USD);
        daily_usd.sort_by(|left, right| left.total_cmp(right));
        daily_usd
    };
    if usable.len() < SPEND_ANOMALY_MIN_USABLE_DAYS {
        return 0.0;
    }
    let middle = usable.len() / 2;
    if usable.len().is_multiple_of(2) {
        (usable[middle - 1] + usable[middle]) / 2.0
    } else {
        usable[middle]
    }
}

/// Notification manager
pub struct NotificationManager {
    /// Track which threshold notifications have been sent to avoid spam.
    /// Keyed by provider + window so session/weekly cannot clear each other.
    sent_notifications: std::collections::HashSet<ThresholdAlertKey>,
    /// Windows observed during this process. The first trustworthy reading is
    /// a baseline, never a threshold crossing, even when provider startup took
    /// longer than the wall-clock quiet period.
    observed_threshold_windows: std::collections::HashSet<ThresholdWindowKey>,
    /// Candidate threshold crossings must survive a second provider read.
    pending_thresholds: std::collections::HashMap<ThresholdWindowKey, NotificationType>,
    /// Track previous session percent for depleted/restored transitions.
    /// Missing entry means we have not observed this provider yet (baseline).
    previous_session_percent: std::collections::HashMap<ProviderId, f64>,
    /// Target depleted state awaiting a second consistent provider read.
    pending_session_states: std::collections::HashMap<ProviderId, bool>,
    predictive_warning_keys: std::collections::HashSet<PredictiveWarningKey>,
    observed_predictive_windows: std::collections::HashSet<PredictiveObservationKey>,
    /// A budget is global, but its total resets at the boundary represented by
    /// this key (for example, `daily:2026-07-19`).
    spend_budget_cycle: Option<String>,
    spend_budget_observed: bool,
    spend_budget_sent: std::collections::HashSet<SpendBudgetAlertLevel>,
    spend_budget_pending: Option<SpendBudgetAlertLevel>,
    /// Local calendar day the spike detector is currently watching, so it
    /// re-arms once at midnight rather than repeating all day.
    spend_anomaly_day: Option<String>,
    spend_anomaly_observed: bool,
    spend_anomaly_sent: bool,
    spend_anomaly_pending: bool,
    /// Remains false until the initial provider refresh has established a
    /// trustworthy in-process baseline for every enabled provider.
    notifications_armed: bool,
    /// Production provider refreshes opt into a hard per-refresh and rolling
    /// toast budget. Keeping this inactive by default also leaves isolated CLI
    /// callers and focused unit tests deterministic.
    refresh_cycle_active: bool,
    toast_emitted_this_refresh: bool,
    /// Resets are exempt from the one-toast-per-refresh rule, so they need
    /// their own (much looser) ceiling to stay storm-proof.
    reset_toasts_this_refresh: usize,
    last_toast_at: Option<std::time::Instant>,
    recent_toasts: std::collections::VecDeque<std::time::Instant>,
    #[cfg(test)]
    pub(crate) toasts_shown: usize,
}

impl NotificationManager {
    pub fn new() -> Self {
        Self {
            sent_notifications: std::collections::HashSet::new(),
            observed_threshold_windows: std::collections::HashSet::new(),
            pending_thresholds: std::collections::HashMap::new(),
            previous_session_percent: std::collections::HashMap::new(),
            pending_session_states: std::collections::HashMap::new(),
            predictive_warning_keys: std::collections::HashSet::new(),
            observed_predictive_windows: std::collections::HashSet::new(),
            spend_budget_cycle: None,
            spend_budget_observed: false,
            spend_budget_sent: std::collections::HashSet::new(),
            spend_budget_pending: None,
            spend_anomaly_day: None,
            spend_anomaly_observed: false,
            spend_anomaly_sent: false,
            spend_anomaly_pending: false,
            notifications_armed: false,
            refresh_cycle_active: false,
            toast_emitted_this_refresh: false,
            reset_toasts_this_refresh: 0,
            last_toast_at: None,
            recent_toasts: std::collections::VecDeque::new(),
            #[cfg(test)]
            toasts_shown: 0,
        }
    }

    #[cfg(test)]
    fn new_armed() -> Self {
        let mut manager = Self::new();
        manager.arm_after_startup_baseline();
        manager
    }

    pub fn arm_after_startup_baseline(&mut self) {
        self.notifications_armed = true;
    }

    pub fn notifications_are_armed(&self) -> bool {
        self.notifications_armed
    }

    /// Start a real provider refresh. Every refresh gets at most one *usage*
    /// toast, and the process-wide rolling limits still apply across refreshes.
    pub fn begin_refresh_cycle(&mut self) {
        self.refresh_cycle_active = true;
        self.toast_emitted_this_refresh = false;
        self.reset_toasts_this_refresh = 0;
    }

    /// Route confirmed reset events through the same startup gate and circuit
    /// breaker used by threshold notifications. Resets are the one event users
    /// should never miss, so they bypass the rolling cooldown *and* the
    /// one-toast-per-refresh ceiling: providers refresh concurrently, so that
    /// ceiling meant an unrelated alert could silently eat the only weekly
    /// reset notification a user gets all week.
    pub fn notify_capacity_event(&mut self, title: &str, body: &str) -> bool {
        self.emit_toast_with_priority(title, body, ToastPriority::Reset, false)
    }

    /// A reset that happened while Ceiling was closed.
    ///
    /// Bypasses the startup gate, because such an event is detected on exactly
    /// the cycle that gate covers and would otherwise never be reported at all.
    /// Every other guard, including the circuit breaker, still applies.
    pub fn notify_capacity_event_while_away(&mut self, title: &str, body: &str) -> bool {
        self.emit_toast_with_priority(title, body, ToastPriority::Reset, true)
    }

    fn emit_toast(&mut self, title: &str, body: &str) -> bool {
        self.emit_toast_with_priority(title, body, ToastPriority::Normal, false)
    }

    fn emit_toast_with_priority(
        &mut self,
        title: &str,
        body: &str,
        priority: ToastPriority,
        bypass_startup_gate: bool,
    ) -> bool {
        if !self.notifications_armed && !bypass_startup_gate {
            tracing::debug!(
                "Suppressing toast while establishing startup baseline: {} — {}",
                title,
                body
            );
            return false;
        }
        #[cfg(all(debug_assertions, not(test)))]
        if std::env::var("CEILING_ENABLE_DEV_NOTIFICATIONS").as_deref() != Ok("1") {
            tracing::debug!(
                title,
                "suppressing Windows toast from a debug/test-adjacent Ceiling build"
            );
            return false;
        }
        #[cfg(all(target_os = "windows", not(test)))]
        if let Err(reason) = windows_notification_delivery_check() {
            tracing::warn!(title, %reason, "Windows notification delivery is disabled");
            return false;
        }
        if self.refresh_cycle_active {
            let now = std::time::Instant::now();
            while self
                .recent_toasts
                .front()
                .is_some_and(|sent_at| now.duration_since(*sent_at) >= TOAST_BURST_WINDOW)
            {
                self.recent_toasts.pop_front();
            }
            let too_soon = self
                .last_toast_at
                .is_some_and(|sent_at| now.duration_since(sent_at) < MIN_TOAST_INTERVAL);
            let rolling_limit_reached = self.recent_toasts.len() >= MAX_TOASTS_PER_BURST_WINDOW;
            let suppressed = match priority {
                ToastPriority::Normal => {
                    self.toast_emitted_this_refresh || too_soon || rolling_limit_reached
                }
                ToastPriority::Reset => {
                    self.reset_toasts_this_refresh >= MAX_RESET_TOASTS_PER_REFRESH
                }
            };
            if suppressed {
                // Log only the limits that actually applied. A reset is gated
                // solely by its own per-refresh ceiling, and reporting the
                // usage-alert limits alongside it sent anyone debugging a
                // dropped reset chasing the wrong guard.
                match priority {
                    ToastPriority::Normal => tracing::warn!(
                        title,
                        per_refresh_limit = self.toast_emitted_this_refresh,
                        minimum_interval_limit = too_soon,
                        rolling_count = self.recent_toasts.len(),
                        "suppressing usage notification due to toast circuit breaker"
                    ),
                    ToastPriority::Reset => tracing::warn!(
                        title,
                        resets_this_refresh = self.reset_toasts_this_refresh,
                        limit = MAX_RESET_TOASTS_PER_REFRESH,
                        "suppressing reset notification: too many resets in one refresh"
                    ),
                }
                return false;
            }
            // A reset still consumes the usage-alert budget, so an unusual burst
            // of resets cannot be followed by a pile of threshold warnings.
            self.toast_emitted_this_refresh = true;
            if priority == ToastPriority::Reset {
                self.reset_toasts_this_refresh += 1;
            }
            self.last_toast_at = Some(now);
            self.recent_toasts.push_back(now);
        }
        #[cfg(test)]
        {
            self.toasts_shown += 1;
            // Unit tests exercise notification state with synthetic usage
            // values. Never let those fixtures escape into the host OS.
            true
        }
        #[cfg(not(test))]
        {
            self.show_toast(title, body);
            true
        }
    }

    pub fn record_predictive_observation(
        &mut self,
        enabled: bool,
        provider: ProviderId,
        identity: &str,
        window: PredictiveWarningWindow,
        rate_window: &RateWindow,
        pace: &UsagePace,
    ) -> bool {
        if !enabled {
            self.predictive_warning_keys
                .retain(|key| key.provider != provider);
            self.observed_predictive_windows
                .retain(|key| key.provider != provider);
            return false;
        }
        // Any provider that reports a reset can be paced; the caller supplies
        // the identity that separates accounts within one.
        if identity.is_empty() {
            return false;
        }
        let Some(resets_at) = rate_window.resets_at else {
            return false;
        };
        let key = PredictiveWarningKey {
            provider,
            identity: identity.to_string(),
            window,
            reset: PredictiveResetWindow {
                window_minutes: rate_window.window_minutes,
                resets_at,
            },
        };
        let observation_key = PredictiveObservationKey {
            provider,
            identity: identity.to_string(),
            window,
        };
        let first_observation = self.observed_predictive_windows.insert(observation_key);

        let warned_this_cycle = self.predictive_warning_keys.iter().any(|existing| {
            existing.provider == key.provider
                && existing.identity == key.identity
                && existing.window == key.window
                && existing.reset.belongs_to_same_cycle(&key.reset)
        });
        self.predictive_warning_keys.retain(|existing| {
            existing.provider != key.provider
                || existing.identity != key.identity
                || existing.window != key.window
        });

        if pace.will_last_to_reset {
            return false;
        }
        if !pace
            .eta_seconds
            .is_some_and(|eta| eta.is_finite() && eta > 0.0)
        {
            return false;
        }

        self.predictive_warning_keys.insert(key);
        !first_observation && !warned_this_cycle
    }

    pub fn set_predictive_warnings_enabled(&mut self, provider: ProviderId, enabled: bool) {
        if !enabled {
            self.predictive_warning_keys
                .retain(|key| key.provider != provider);
            self.observed_predictive_windows
                .retain(|key| key.provider != provider);
        }
    }

    pub fn check_predictive_pace(
        &mut self,
        provider: ProviderId,
        identity: &str,
        window: PredictiveWarningWindow,
        rate_window: &RateWindow,
        pace: &UsagePace,
        settings: &Settings,
    ) {
        if !self.record_predictive_observation(
            settings.show_notifications && settings.predictive_pace_warning_enabled,
            provider,
            identity,
            window,
            rate_window,
            pace,
        ) {
            return;
        }

        let eta = format_duration(pace.eta_seconds.unwrap_or_default());
        let provider_name = provider.display_name();
        let window_label = window.localized_label(settings.ui_language);
        let title = locale::format_locale(
            settings.ui_language,
            LocaleKey::PredictivePaceWarningTitle,
            &[provider_name, &window_label],
        );
        let body = locale::format_locale(
            settings.ui_language,
            LocaleKey::PredictivePaceWarningBody,
            &[&eta],
        );
        if self.emit_toast(&title, &body) {
            play_alert(AlertSound::Warning, settings);
        }
    }

    /// Check usage and send notifications if thresholds are crossed.
    ///
    /// Alerts are deduped per `(provider, window, severity)`. Dropping below the
    /// high threshold (with hysteresis) re-arms that window only — a quiet weekly
    /// meter must not clear a loud session alert (and vice versa).
    pub fn check_and_notify(
        &mut self,
        provider: ProviderId,
        account: Option<&str>,
        window: &str,
        used_percent: f64,
        settings: &Settings,
    ) {
        // A model pool is a hard ceiling on the work you can do right now — a
        // maxed Opus allowance stops Opus — so it warns like the session and
        // weekly windows. Monthly stays out on purpose: it moves slowly enough
        // that crossing a threshold mid-cycle is normal, not news. See
        // `monthly_and_unknown_windows_never_raise_usage_toasts`.
        if !settings.show_notifications || !matches!(window, "session" | "weekly" | "model") {
            return;
        }

        let thresholds = settings.usage_thresholds(provider, window);
        let rearm_below = (thresholds.high - REARM_HYSTERESIS).max(0.0);
        let window_key = ThresholdWindowKey {
            provider,
            window: window.to_string(),
            account: account.map(str::to_string),
        };

        // A window gets one calm warning per cycle. Crossing critical or
        // exhausted later must not produce a second toast for the same limit.
        let notification_type = if used_percent >= thresholds.high {
            Some(NotificationType::HighUsage)
        } else if used_percent < rearm_below {
            self.clear_window_alerts(provider, account, window);
            self.pending_thresholds.remove(&window_key);
            None
        } else {
            self.pending_thresholds.remove(&window_key);
            None
        };

        // Treat the first live value for every provider/window as its baseline.
        // A slow launch must not reinterpret already-high usage as a crossing.
        if self.observed_threshold_windows.insert(window_key.clone()) {
            for kind in [(used_percent >= thresholds.high).then_some(NotificationType::HighUsage)]
                .into_iter()
                .flatten()
            {
                self.sent_notifications.insert(ThresholdAlertKey {
                    provider,
                    window: window.to_string(),
                    kind,
                    account: account.map(str::to_string),
                });
            }
            self.pending_thresholds.remove(&window_key);
            return;
        }

        if let Some(notif_type) = notification_type {
            let key = ThresholdAlertKey {
                provider,
                window: window.to_string(),
                kind: notif_type,
                account: account.map(str::to_string),
            };
            if !self.sent_notifications.contains(&key) {
                if self.pending_thresholds.get(&window_key) != Some(&notif_type) {
                    self.pending_thresholds.insert(window_key, notif_type);
                    return;
                }
                self.pending_thresholds.remove(&window_key);
                // Mark before emit so startup quiet still arms state.
                self.sent_notifications.insert(key);
                self.send_notification(provider, window, used_percent, notif_type, settings);
            }
        }
    }

    /// Check the global, locally-estimated API-value budget. Like quota alerts,
    /// a crossing must survive two scans and a current-period startup reading is
    /// only a baseline. The budget uses its own cycle key so daily and calendar
    /// month-to-date totals re-arm naturally at their reset boundary.
    pub fn check_spend_budget(
        &mut self,
        cycle_id: &str,
        period_label: &str,
        estimated_usd: f64,
        settings: &Settings,
    ) {
        let enabled = settings.show_notifications
            && settings.spend_budget_alerts_enabled
            && settings.spend_budget_limit_usd.is_finite()
            && settings.spend_budget_limit_usd > 0.0;
        if !enabled || !estimated_usd.is_finite() || estimated_usd < 0.0 {
            self.spend_budget_cycle = None;
            self.spend_budget_observed = false;
            self.spend_budget_sent.clear();
            self.spend_budget_pending = None;
            return;
        }

        if self.spend_budget_cycle.as_deref() != Some(cycle_id) {
            self.spend_budget_cycle = Some(cycle_id.to_string());
            self.spend_budget_observed = false;
            self.spend_budget_sent.clear();
            self.spend_budget_pending = None;
        }

        let level = if estimated_usd >= settings.spend_budget_limit_usd {
            Some(SpendBudgetAlertLevel::NearCap)
        } else if settings.spend_budget_warning_usd.is_finite()
            && settings.spend_budget_warning_usd > 0.0
            && estimated_usd >= settings.spend_budget_warning_usd
        {
            Some(SpendBudgetAlertLevel::Warning)
        } else {
            None
        };

        // Never reinterpret an already-high total at startup (or immediately
        // after enabling the feature) as a fresh threshold crossing.
        if !self.spend_budget_observed {
            self.spend_budget_observed = true;
            if let Some(level) = level {
                self.spend_budget_sent.insert(level);
            }
            return;
        }

        let Some(level) = level else {
            self.spend_budget_pending = None;
            return;
        };
        if self.spend_budget_sent.contains(&level) {
            return;
        }
        if self.spend_budget_pending != Some(level) {
            self.spend_budget_pending = Some(level);
            return;
        }

        self.spend_budget_pending = None;
        self.spend_budget_sent.insert(level);
        let title = match level {
            SpendBudgetAlertLevel::Warning => "Estimated API value budget warning",
            SpendBudgetAlertLevel::NearCap => "Estimated API value near cap",
        };
        let threshold = match level {
            SpendBudgetAlertLevel::Warning => settings.spend_budget_warning_usd,
            SpendBudgetAlertLevel::NearCap => settings.spend_budget_limit_usd,
        };
        let body = format!(
            "{period_label} estimated API value is ${estimated_usd:.2}; your {} is ${threshold:.2}. This is an estimate from local Codex and Claude logs, not a bill.",
            match level {
                SpendBudgetAlertLevel::Warning => "warning budget",
                SpendBudgetAlertLevel::NearCap => "cap",
            }
        );
        if self.emit_toast(title, &body) {
            play_alert(
                match level {
                    SpendBudgetAlertLevel::Warning => AlertSound::Warning,
                    SpendBudgetAlertLevel::NearCap => AlertSound::Critical,
                },
                settings,
            );
        }
    }

    /// Warn when today's estimated API value runs far above the recent daily
    /// norm (SBS-279).
    ///
    /// This is deliberately not a budget: it needs no configured cap and asks
    /// only "is today unlike my recent days", which is what a runaway agent
    /// loop looks like before the bill arrives. The baseline is the **median**
    /// of the preceding days rather than the mean, so a single heavy day last
    /// week cannot raise the bar high enough to hide a real spike today.
    ///
    /// `baseline_usd` must exclude today, or today's own spike would drag the
    /// bar it is measured against upward.
    pub fn check_spend_anomaly(
        &mut self,
        day_id: &str,
        today_usd: f64,
        baseline_usd: f64,
        settings: &Settings,
    ) {
        let enabled = settings.show_notifications && settings.spend_anomaly_alerts_enabled;
        if !enabled
            || !today_usd.is_finite()
            || today_usd < 0.0
            || !baseline_usd.is_finite()
            || baseline_usd < 0.0
        {
            self.spend_anomaly_day = None;
            self.spend_anomaly_observed = false;
            self.spend_anomaly_sent = false;
            self.spend_anomaly_pending = false;
            return;
        }

        if self.spend_anomaly_day.as_deref() != Some(day_id) {
            self.spend_anomaly_day = Some(day_id.to_string());
            self.spend_anomaly_observed = false;
            self.spend_anomaly_sent = false;
            self.spend_anomaly_pending = false;
        }

        let multiplier = normalize_spend_anomaly_multiplier(settings.spend_anomaly_multiplier);
        let spiking = is_spend_spike(today_usd, baseline_usd, multiplier);

        // A machine that was already mid-spike when Ceiling started (or when
        // the setting was switched on) has not crossed anything yet.
        if !self.spend_anomaly_observed {
            self.spend_anomaly_observed = true;
            self.spend_anomaly_sent = spiking;
            return;
        }

        if !spiking {
            self.spend_anomaly_pending = false;
            return;
        }
        if self.spend_anomaly_sent {
            return;
        }
        // Two consecutive scans, like every other alert here: one scan can read
        // a partially written transcript.
        if !self.spend_anomaly_pending {
            self.spend_anomaly_pending = true;
            return;
        }

        let body = spend_anomaly_toast_body(today_usd, baseline_usd);
        // Marked sent only once a toast actually went out. `emit_toast` also
        // returns false for the per-refresh cap, the burst limit, and the
        // startup gate, and enrichment runs inside the same refresh that just
        // raised a usage alert. Marking first would spend the day's one spike
        // alert on a toast nobody saw — on exactly the day a runaway is also
        // tripping the usage threshold, which is the case this exists for.
        if self.emit_toast("Spend running above normal", &body) {
            self.spend_anomaly_pending = false;
            self.spend_anomaly_sent = true;
            play_alert(AlertSound::Warning, settings);
        }
        // `pending` is deliberately left set when the toast was swallowed. It
        // records that the crossing is already confirmed, so the next scan
        // retries the moment a slot frees up instead of spending another two
        // scans re-confirming what it already knows.
    }

    fn clear_window_alerts(&mut self, provider: ProviderId, account: Option<&str>, window: &str) {
        self.sent_notifications.retain(|key| {
            !(key.provider == provider && key.window == window && key.account.as_deref() == account)
        });
    }

    /// Send a notification for a status issue
    pub fn notify_status_issue(
        &mut self,
        provider: ProviderId,
        description: &str,
        settings: &Settings,
    ) {
        if !settings.show_notifications {
            return;
        }
        let key = ThresholdAlertKey {
            provider,
            window: "status".to_string(),
            kind: NotificationType::StatusIssue,
            // A provider outage is not an account-level event.
            account: None,
        };
        if !self.sent_notifications.contains(&key) {
            self.sent_notifications.insert(key);
            self.send_status_notification(provider, description, settings);
        }
    }

    /// Clear status issue notification (when resolved)
    pub fn clear_status_issue(&mut self, provider: ProviderId) {
        self.sent_notifications
            .retain(|key| !(key.provider == provider && key.kind == NotificationType::StatusIssue));
    }

    /// Check session quota transitions (depleted/restored)
    /// Call this with each usage update to detect transitions
    pub fn check_session_transition(
        &mut self,
        provider: ProviderId,
        current_percent: f64,
        settings: &Settings,
    ) {
        if !settings.show_notifications {
            return;
        }

        const DEPLETED_THRESHOLD: f64 = 99.99; // Consider depleted at 99.99%+
        let current_depleted = current_percent >= DEPLETED_THRESHOLD;

        let Some(previous_percent) = self.previous_session_percent.get(&provider).copied() else {
            // First observation is a baseline only. In particular, do not arm
            // a restoration toast from a single already-depleted startup read.
            self.previous_session_percent
                .insert(provider, current_percent);
            return;
        };
        let previous_depleted = previous_percent >= DEPLETED_THRESHOLD;

        if current_depleted == previous_depleted {
            self.pending_session_states.remove(&provider);
            self.previous_session_percent
                .insert(provider, current_percent);
            return;
        }

        if self.pending_session_states.get(&provider) != Some(&current_depleted) {
            self.pending_session_states
                .insert(provider, current_depleted);
            return;
        }
        self.pending_session_states.remove(&provider);

        // Check for depleted transition: was not depleted, now is
        if current_depleted {
            let title = NotificationType::SessionDepleted.title();
            let body = format!(
                "{} session depleted. 0% left. Will notify when available again.",
                provider.display_name()
            );
            if self.emit_toast(title, &body) {
                play_alert(AlertSound::Error, settings);
            }
            self.sent_notifications.insert(ThresholdAlertKey {
                provider,
                window: "session".to_string(),
                kind: NotificationType::SessionDepleted,
                account: None,
            });
        }
        // Check for restored transition: was depleted, now is not
        else {
            let depleted_key = ThresholdAlertKey {
                provider,
                window: "session".to_string(),
                kind: NotificationType::SessionDepleted,
                account: None,
            };
            if self.sent_notifications.contains(&depleted_key) {
                let title = NotificationType::SessionRestored.title();
                let body = format!(
                    "{} session restored. Session quota is available again.",
                    provider.display_name()
                );
                if self.emit_toast(title, &body) {
                    play_alert(AlertSound::Success, settings);
                }
                self.sent_notifications.remove(&depleted_key);
            }
        }

        self.previous_session_percent
            .insert(provider, current_percent);
    }

    /// Send a Windows toast notification with sound
    fn send_notification(
        &mut self,
        provider: ProviderId,
        window: &str,
        used_percent: f64,
        notif_type: NotificationType,
        settings: &Settings,
    ) {
        let title = notif_type.title();
        let body = Self::notification_body(provider, window, used_percent, notif_type);
        if self.emit_toast(title, &body) {
            play_alert(Self::alert_sound_for(notif_type), settings);
        }
    }

    fn notification_body(
        provider: ProviderId,
        window: &str,
        used_percent: f64,
        notif_type: NotificationType,
    ) -> String {
        let provider_name = provider.display_name();
        let window_label = match window {
            "session" => "session",
            "weekly" => "weekly",
            "monthly" => "monthly",
            "primary" => "usage",
            other => other,
        };
        match notif_type {
            NotificationType::HighUsage => {
                format!("{provider_name} {window_label} at {used_percent:.0}% — approaching limit")
            }
            NotificationType::CriticalUsage => {
                format!("{provider_name} {window_label} at {used_percent:.0}% — critically high")
            }
            NotificationType::Exhausted => {
                format!("{provider_name} {window_label} exhausted ({used_percent:.0}%)")
            }
            NotificationType::StatusIssue => format!("{provider_name} is experiencing issues"),
            NotificationType::SessionDepleted => {
                format!("{provider_name} session depleted. 0% left.")
            }
            NotificationType::SessionRestored => {
                format!("{provider_name} session restored. Quota available again.")
            }
        }
    }

    fn alert_sound_for(notif_type: NotificationType) -> AlertSound {
        match notif_type {
            NotificationType::HighUsage => AlertSound::Warning,
            NotificationType::CriticalUsage => AlertSound::Critical,
            NotificationType::Exhausted
            | NotificationType::StatusIssue
            | NotificationType::SessionDepleted => AlertSound::Error,
            NotificationType::SessionRestored => AlertSound::Success,
        }
    }

    fn send_status_notification(
        &mut self,
        provider: ProviderId,
        description: &str,
        settings: &Settings,
    ) {
        let title = NotificationType::StatusIssue.title();
        let body = format!("{}: {}", provider.display_name(), description);
        if self.emit_toast(title, &body) {
            play_alert(AlertSound::Error, settings);
        }
    }

    #[cfg(target_os = "windows")]
    fn show_toast(&self, title: &str, body: &str) {
        use std::os::windows::process::CommandExt;
        use std::process::Command;

        ensure_aumid_registered_once();

        // Fire-and-forget on the normal notification path: a provider refresh must
        // never block on PowerShell. Failures are logged, not surfaced.
        let Some(powershell) = crate::host::windows_powershell_exe() else {
            tracing::warn!(
                "Failed to dispatch toast notification '{}': trusted PowerShell not found",
                title
            );
            return;
        };
        match Command::new(powershell)
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &toast_powershell_script(title, body),
            ])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .spawn()
        {
            Ok(_) => tracing::debug!("Toast notification dispatched: {}", title),
            Err(e) => tracing::warn!("Failed to dispatch toast notification '{}': {}", title, e),
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn show_toast(&self, title: &str, body: &str) {
        use std::process::Command;

        // Try notify-send first (works on most Linux distros including WSL with WSLg)
        if let Ok(output) = Command::new("notify-send")
            .args([
                "--app-name=Ceiling",
                "--icon=dialog-information",
                title,
                body,
            ])
            .output()
            && output.status.success()
        {
            tracing::debug!("Sent notification via notify-send: {}", title);
            return;
        }

        tracing::info!("Notification: {} - {}", title, body);
    }
}

fn format_duration(seconds: f64) -> String {
    let total_minutes = (seconds / 60.0).ceil().max(1.0) as i64;
    let days = total_minutes / 1440;
    let hours = (total_minutes % 1440) / 60;
    let minutes = total_minutes % 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

impl Default for NotificationManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Escape a string for inclusion in the toast XML payload.
#[cfg(target_os = "windows")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The Ceiling mark, shown on every toast and as the app icon Windows renders
/// beside the "Ceiling" attribution. Embedded so it is available to portable
/// builds, which have no install directory to read it from.
#[cfg(target_os = "windows")]
const NOTIFICATION_ICON_PNG: &[u8] = include_bytes!("../icons/icon.png");

/// Backplate Windows draws behind the app icon, matching the mark's own
/// background so the two do not read as two stacked squares. ARGB hex.
#[cfg(target_os = "windows")]
const NOTIFICATION_ICON_BACKGROUND: &str = "FF0D1117";

/// Write the toast icon next to the settings file and return its path.
///
/// Windows reads this off disk when it renders the toast, so it has to be a
/// real file rather than an embedded resource. Rewritten only when missing or
/// the wrong size, so a normal launch does no disk work.
#[cfg(target_os = "windows")]
fn notification_icon_path() -> Option<std::path::PathBuf> {
    static ICON: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let path = crate::settings::Settings::settings_path()?
            .parent()?
            .join("notification-icon.png");
        let current_len = std::fs::metadata(&path).ok().map(|meta| meta.len());
        if current_len != Some(NOTIFICATION_ICON_PNG.len() as u64) {
            std::fs::create_dir_all(path.parent()?).ok()?;
            if let Err(error) = std::fs::write(&path, NOTIFICATION_ICON_PNG) {
                tracing::warn!(%error, "could not write the toast icon; toasts will be text-only");
                return None;
            }
        }
        Some(path)
    })
    .clone()
}

/// Build the PowerShell snippet that shows a single Ceiling toast. Uses
/// ToastGeneric (Win 10+) and wraps the body in try/catch so PowerShell exits
/// with code 1 on failure instead of swallowing the error. A single-quoted
/// here-string (@'...'@) prevents PowerShell from expanding the XML content.
#[cfg(target_os = "windows")]
fn toast_powershell_script(title: &str, body: &str) -> String {
    format!(
        r#"try {{
    [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
    [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null
    $template = @'
{}
'@
    $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
    $xml.LoadXml($template)
    $toast = [Windows.UI.Notifications.ToastNotification]::new($xml)
    $notifier = [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier("{}")
    if ($null -eq $notifier) {{ throw "CreateToastNotifier returned null" }}
    $notifier.Show($toast)
}} catch {{
    [System.Console]::Error.WriteLine("Ceiling toast failed: $_")
    exit 1
}}"#,
        toast_xml(title, body, notification_icon_path().as_deref()),
        CEILING_AUMID
    )
}

/// The toast payload itself.
///
/// `appLogoOverride` puts the Ceiling mark in the toast body; the app name and
/// the icon beside it come from the AUMID registration. Falls back to plain
/// text when the icon could not be written, since a toast with no logo still
/// beats no toast.
#[cfg(target_os = "windows")]
fn toast_xml(title: &str, body: &str, icon: Option<&std::path::Path>) -> String {
    let logo = icon
        .map(|path| {
            format!(
                r#"<image placement="appLogoOverride" src="{}"/>"#,
                xml_escape(&path.display().to_string())
            )
        })
        .unwrap_or_default();
    format!(
        r#"<toast><visual><binding template="ToastGeneric">{logo}<text>{}</text><text>{}</text></binding></visual></toast>"#,
        xml_escape(title),
        xml_escape(body)
    )
}

/// Establish Ceiling's Windows notification identity at most once per process,
/// before the first toast. Both halves matter: see [`ensure_aumid_registered`]
/// and [`ensure_start_menu_shortcut`].
#[cfg(target_os = "windows")]
fn ensure_aumid_registered_once() {
    use std::sync::Once;
    static AUMID_INIT: Once = Once::new();
    AUMID_INIT.call_once(|| {
        ensure_aumid_registered();
        report_notification_history_support();
        repair_action_center_visibility();
    });
}

/// Undo the notification-center opt-out Windows applies to unregistered apps.
///
/// Windows creates our channel the first time we toast. Before Ceiling carried
/// an AUMID on a Start Menu shortcut it had no real app identity, so Windows
/// stamped `ShowInActionCenter=0` on that channel: banners appear for a few
/// seconds and are then discarded instead of collecting in the notification
/// center. Apps with a proper identity get no such value at all, and the
/// absent-value default is to show.
///
/// Only ever flips an existing `0` back to `1`. A missing value already means
/// "show", and nothing here creates or relaxes any other notification setting.
#[cfg(target_os = "windows")]
fn repair_action_center_visibility() {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};

    let path = format!(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Notifications\Settings\{CEILING_AUMID}"
    );
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(key) = hkcu.open_subkey_with_flags(&path, KEY_READ | KEY_SET_VALUE) else {
        // No channel yet: Windows will create one once we toast, and by then
        // the shortcut above gives us an identity that defaults to showing.
        return;
    };
    if key.get_value::<u32, _>("ShowInActionCenter").ok() != Some(0) {
        return;
    }
    match key.set_value("ShowInActionCenter", &1u32) {
        Ok(()) => tracing::info!(
            "re-enabled notification-center history for Ceiling; alerts were banner-only"
        ),
        Err(error) => tracing::warn!(
            %error,
            "could not re-enable notification-center history for Ceiling"
        ),
    }
}

/// Show a Ceiling toast synchronously and report whether the OS actually
/// accepted it. Powers the Settings "Send test notification" button, so it
/// deliberately bypasses the startup-baseline gate, the per-refresh limit, and
/// the rolling cooldown: the user asked for it right now and needs to see the
/// real Windows toast pipeline (AUMID registration + PowerShell + WinRT)
/// succeed or fail end-to-end. Blocking is fine — this runs off a user click,
/// never the refresh loop.
#[cfg(target_os = "windows")]
pub fn send_test_notification() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    ensure_aumid_registered_once();
    windows_notification_delivery_check()?;

    let title = "Ceiling notifications are on";
    let body = "This is a test. Unexpected resets and usage alerts will look like this.";

    let powershell = crate::host::windows_powershell_exe()
        .ok_or_else(|| "Could not find the trusted Windows PowerShell".to_string())?;
    let output = Command::new(powershell)
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &toast_powershell_script(title, body),
        ])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .map_err(|e| format!("Could not launch the Windows notifier: {e}"))?;

    if output.status.success() {
        tracing::debug!("Test toast notification shown");
        return Ok(());
    }

    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    tracing::warn!("Test toast notification failed: {detail}");
    if detail.is_empty() {
        Err("Windows rejected the notification. Check notification settings.".to_string())
    } else {
        Err(detail.to_string())
    }
}

#[cfg(target_os = "windows")]
fn windows_notification_delivery_check() -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) =
        hkcu.open_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion\PushNotifications")
        && key.get_value::<u32, _>("ToastEnabled").ok() == Some(0)
    {
        return Err(
            "Windows notifications are turned off. Turn them on in Settings > System > Notifications."
                .to_string(),
        );
    }

    // The current channel is authoritative once it says anything, and the
    // retired one is consulted only when it does not. Someone who switched
    // Ceiling off before the AUMID changed recorded that against the old name,
    // and moving identities must not toast straight over their decision; but
    // once they re-enable Ceiling in Windows Settings that lands on the current
    // channel, and a stale 0 on the old one must not veto it forever.
    let channel_enabled = |aumid: &str| {
        hkcu.open_subkey(format!(
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Notifications\Settings\{aumid}"
        ))
        .ok()
        .and_then(|key| key.get_value::<u32, _>("Enabled").ok())
    };
    let effective = effective_channel_enabled(
        channel_enabled(CEILING_AUMID),
        channel_enabled(LEGACY_CEILING_AUMID),
    );
    if notification_channel_is_disabled(effective) {
        return Err(
            "Windows notifications are turned off for Ceiling. Enable Ceiling in Settings > System > Notifications."
                .to_string(),
        );
    }

    Ok(())
}

/// Which channel's `Enabled` value decides delivery.
///
/// The current AUMID wins whenever it has an explicit value; the retired one is
/// a fallback for users who set their preference before the identity changed.
/// Split out from the registry read so the precedence itself can be tested.
#[cfg(target_os = "windows")]
fn effective_channel_enabled(current: Option<u32>, legacy: Option<u32>) -> Option<u32> {
    current.or(legacy)
}

#[cfg(target_os = "windows")]
fn notification_channel_is_disabled(enabled: Option<u32>) -> bool {
    // Only the explicit app toggle blocks delivery outright. `ShowInActionCenter`
    // is a separate value that suppresses notification-center history while
    // banners still arrive; [`repair_action_center_visibility`] handles that one
    // rather than treating it as an undeliverable channel.
    enabled == Some(0)
}

/// Non-Windows fallback for the test-notification button.
#[cfg(not(target_os = "windows"))]
pub fn send_test_notification() -> Result<(), String> {
    use std::process::Command;

    let title = "Ceiling notifications are on";
    let body = "This is a test. Unexpected resets and usage alerts will look like this.";

    match Command::new("notify-send")
        .args([
            "--app-name=Ceiling",
            "--icon=dialog-information",
            title,
            body,
        ])
        .output()
    {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stderr);
            let detail = detail.trim();
            if detail.is_empty() {
                Err("The system notifier rejected the notification.".to_string())
            } else {
                Err(detail.to_string())
            }
        }
        Err(e) => Err(format!("No system notifier available: {e}")),
    }
}

/// Register [`CEILING_AUMID`] in the Windows registry so that
/// `CreateToastNotifier(CEILING_AUMID)` resolves to a valid notifier instead of
/// returning null.  Must be called at least once before the first toast.  Safe
/// to call multiple times (idempotent registry write).
///
/// This covers activation only. Notification-center history additionally needs
/// the Start Menu shortcut written by [`ensure_start_menu_shortcut`].
#[cfg(target_os = "windows")]
fn ensure_aumid_registered() {
    use winreg::RegKey;
    use winreg::enums::*;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    // HKCU\SOFTWARE\Classes\AppUserModelId\<AUMID> is the documented path for
    // registering Win32 desktop app AUMIDs without a COM server or Start Menu shortcut.
    let result = hkcu
        .create_subkey(format!(r"SOFTWARE\Classes\AppUserModelId\{CEILING_AUMID}"))
        .and_then(|(key, _)| {
            // DisplayName and IconUri are what Windows renders as the sender of
            // the toast, and on the Ceiling row in Settings > Notifications.
            // Without them a toast is attributed to a nameless, iconless app.
            key.set_value("DisplayName", &"Ceiling")?;
            if let Some(icon) = notification_icon_path() {
                key.set_value("IconUri", &icon.as_os_str())?;
                key.set_value("IconBackgroundColor", &NOTIFICATION_ICON_BACKGROUND)?;
            }
            Ok(())
        });

    match result {
        Ok(()) => tracing::debug!("Ceiling AUMID registered for Windows toast notifications"),
        Err(e) => tracing::warn!("Failed to register Ceiling AUMID: {}", e),
    }
}

/// The AUMID every Ceiling toast is published under.
///
/// This is the Tauri bundle `identifier` from `tauri.conf.json`, which the NSIS
/// installer already stamps onto the Start Menu shortcut. Publishing under it
/// means Windows resolves toasts to an app it actually knows about.
///
/// Ceiling used to toast under the bare string `"Ceiling"`, which no shortcut
/// claimed. Windows accepted those toasts but treated the identity as
/// unregistered: it stamped `ShowInActionCenter=0` on that channel, so banners
/// flashed for a few seconds and were then discarded instead of collecting in
/// the notification center. Keep this in step with `tauri.conf.json`.
#[cfg(target_os = "windows")]
const CEILING_AUMID: &str = "io.github.tsouth89.ceiling";

/// The AUMID Ceiling toasted under before the fix above.
///
/// Only consulted so a user who switched Ceiling's notifications off in Windows
/// Settings keeps that choice: their opt-out lives on this channel, and moving
/// to a registered identity must not quietly undo it.
#[cfg(target_os = "windows")]
const LEGACY_CEILING_AUMID: &str = "Ceiling";

/// `System.AppUserModel.ID`, the shortcut property that ties a desktop app to a
/// notification identity. Declared here rather than pulled from
/// `Win32_Storage_EnhancedStorage` to avoid enabling that feature for one
/// constant; the value is fixed by the shell and documented by Microsoft.
#[cfg(target_os = "windows")]
const PKEY_APP_USER_MODEL_ID: windows::Win32::Foundation::PROPERTYKEY =
    windows::Win32::Foundation::PROPERTYKEY {
        fmtid: windows::core::GUID::from_u128(0x9F4C2855_9F79_4B39_A8D0_E1D42DE1D5F3),
        pid: 5,
    };

/// The Start Menu entry that claims our AUMID. The NSIS installer writes this
/// exact file, so an installed build finds it already correct.
#[cfg(target_os = "windows")]
fn start_menu_shortcut_path() -> Result<std::path::PathBuf, String> {
    let appdata = std::env::var_os("APPDATA").ok_or_else(|| "APPDATA is not set".to_string())?;
    Ok(std::path::PathBuf::from(appdata)
        .join(r"Microsoft\Windows\Start Menu\Programs")
        .join("Ceiling.lnk"))
}

/// Whether a Start Menu shortcut claims [`CEILING_AUMID`], which decides if
/// Windows will keep our toasts in the notification center.
///
/// Windows resolves a desktop app's notification identity from a Start Menu
/// shortcut carrying `System.AppUserModel.ID`. Without one it still shows the
/// banner, but treats the app as unregistered and discards the toast rather
/// than keeping it. The registry AUMID key is not a substitute: an identity
/// registered that way and with `ShowInActionCenter=1` still did not retain.
///
/// Installed builds get this free, because the NSIS installer stamps the bundle
/// identifier onto `Ceiling.lnk`. Portable builds have no installer and Ceiling
/// deliberately does **not** create the shortcut for them: someone who chose a
/// portable build did so to avoid writing to the machine, and silently adding a
/// Start Menu entry to buy notification history is not a trade Ceiling should
/// make on their behalf. Portable users get banners without history, and this
/// says so in the log once rather than leaving it a mystery.
#[cfg(target_os = "windows")]
fn report_notification_history_support() {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

    let Ok(shortcut) = start_menu_shortcut_path() else {
        return;
    };
    unsafe {
        // A failure here means this thread already has COM up under another
        // model, which is fine to borrow. Only balance our own initialization.
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        if read_shortcut_aumid(&shortcut).as_deref() == Some(CEILING_AUMID) {
            tracing::debug!("Start Menu shortcut claims the Ceiling AUMID; toasts will persist");
        } else {
            tracing::info!(
                expected = CEILING_AUMID,
                path = %shortcut.display(),
                "no Start Menu shortcut claims Ceiling's notification identity (normal for a portable build): \
                 alerts will appear as banners but Windows will not keep them in the notification center"
            );
        }
        if initialized {
            CoUninitialize();
        }
    }
}

/// The AUMID an existing `.lnk` publishes under, if any.
///
/// # Safety
/// COM must be initialized on the calling thread.
#[cfg(target_os = "windows")]
unsafe fn read_shortcut_aumid(shortcut: &std::path::Path) -> Option<String> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface};

    unsafe {
        if !shortcut.exists() {
            return None;
        }
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(&HSTRING::from(shortcut), STGM_READ).ok()?;
        let store: IPropertyStore = link.cast().ok()?;
        Some(store.GetValue(&PKEY_APP_USER_MODEL_ID).ok()?.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{PaceStage, RateWindow, UsagePace};
    use chrono::{DateTime, Duration, Utc};

    fn pace(will_last_to_reset: bool, eta_seconds: Option<f64>) -> UsagePace {
        UsagePace {
            stage: PaceStage::Ahead,
            delta_percent: 20.0,
            expected_used_percent: 40.0,
            actual_used_percent: 60.0,
            eta_seconds,
            will_last_to_reset,
        }
    }

    /// The payload is assembled by string formatting and handed to an XML
    /// parser in another process, where a malformed document just means a
    /// silently missing notification.
    #[cfg(target_os = "windows")]
    #[test]
    fn the_toast_payload_carries_the_logo_and_escapes_its_content() {
        let icon = std::path::Path::new(r"C:\Users\a b\AppData\Roaming\Ceiling\icon.png");
        let xml = toast_xml("Codex weekly reset", "90% \"free\" & <ready>", Some(icon));

        assert!(
            xml.contains(r#"<image placement="appLogoOverride""#),
            "logo missing: {xml}"
        );
        assert!(
            xml.contains(r"Ceiling\icon.png"),
            "icon path missing: {xml}"
        );
        // Unescaped quotes or angle brackets from a provider-supplied label
        // would break the document, so the body must arrive escaped.
        assert!(
            xml.contains("&quot;free&quot; &amp; &lt;ready&gt;"),
            "{xml}"
        );
        assert!(!xml.contains("<ready>"), "raw markup leaked: {xml}");

        // No icon on disk still has to produce a valid, showable toast.
        let bare = toast_xml("Title", "Body", None);
        assert!(!bare.contains("<image"), "{bare}");
        assert!(
            bare.contains("<text>Title</text><text>Body</text>"),
            "{bare}"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn notification_channel_only_blocks_explicit_disable_flags() {
        assert!(!notification_channel_is_disabled(None));
        assert!(!notification_channel_is_disabled(Some(1)));
        assert!(notification_channel_is_disabled(Some(0)));
    }

    /// Switching AUMIDs must not resurrect notifications for someone who turned
    /// them off, since their opt-out is recorded against the old channel. It
    /// must also not trap them: re-enabling Ceiling writes to the *current*
    /// channel, and a stale 0 on the retired one cannot be allowed to veto that
    /// forever, because nothing in the Windows UI would clear it.
    #[cfg(target_os = "windows")]
    #[test]
    fn the_current_channel_outranks_the_retired_one() {
        assert_ne!(
            CEILING_AUMID, LEGACY_CEILING_AUMID,
            "the legacy channel check is pointless if both names match"
        );

        // Nothing set anywhere: deliverable.
        assert!(!notification_channel_is_disabled(
            effective_channel_enabled(None, None)
        ));
        // Opted out before the AUMID changed, never revisited: still honored.
        assert!(notification_channel_is_disabled(effective_channel_enabled(
            None,
            Some(0)
        )));
        // Re-enabled in Windows Settings after the switch: the stale 0 loses.
        assert!(!notification_channel_is_disabled(
            effective_channel_enabled(Some(1), Some(0))
        ));
        // Opted out after the switch: honored regardless of the old value.
        assert!(notification_channel_is_disabled(effective_channel_enabled(
            Some(0),
            Some(1)
        )));
    }

    /// Whether toasts persist hinges on reading this property correctly, and
    /// the COM plumbing fails silently, so read the real installed shortcut
    /// rather than trusting the call not to error.
    ///
    /// Ceiling never writes this file, so the test only reads: on a portable
    /// or CI machine there is no shortcut and the absent case is what matters.
    #[cfg(target_os = "windows")]
    #[test]
    fn the_shortcut_aumid_is_readable_and_absence_is_not_an_error() {
        use windows::Win32::System::Com::{
            COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize,
        };

        unsafe {
            let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();

            // A path that cannot exist must read as "claims nothing", never as
            // a panic or a false match that would mislead the log.
            let missing = std::env::temp_dir().join("ceiling-no-such-shortcut.lnk");
            let _ = std::fs::remove_file(&missing);
            assert_eq!(read_shortcut_aumid(&missing), None);

            if initialized {
                CoUninitialize();
            }
        }
    }

    /// The AUMID must stay in step with the Tauri bundle identifier, since the
    /// installer stamps that value onto the Start Menu shortcut and a mismatch
    /// silently costs notification-center history.
    #[cfg(target_os = "windows")]
    #[test]
    fn the_toast_aumid_matches_the_bundle_identifier() {
        let config = include_str!("../../apps/desktop-tauri/src-tauri/tauri.conf.json");
        assert!(
            config.contains(&format!("\"identifier\": \"{CEILING_AUMID}\"")),
            "tauri.conf.json no longer declares {CEILING_AUMID} as its identifier"
        );
    }

    /// The installer is the only thing that gives Ceiling a notification
    /// identity, and it failing to do so is invisible until someone notices
    /// alerts are not being kept. 1.5.17 shipped exactly that way: the
    /// `[Icons]` entries had no `AppUserModelID`, so the shortcut claimed
    /// nothing and Windows discarded every toast it rendered.
    #[cfg(target_os = "windows")]
    #[test]
    fn the_installer_stamps_our_aumid_onto_the_start_menu_shortcut() {
        let script = include_str!("../installer/codexbar.iss");

        assert!(
            script.contains(&format!(r#"#define AppUserModelId "{CEILING_AUMID}""#)),
            "installer no longer defines {CEILING_AUMID} as its AppUserModelId"
        );
        // AppId is Inno's upgrade identity and is written literally rather than
        // through the preprocessor, so nothing but this stops the two drifting
        // apart and publishing toasts under an id the shortcut does not claim.
        assert!(
            script.contains(&format!("{}AppId={CEILING_AUMID}", '\n')),
            "installer AppId no longer matches {CEILING_AUMID}"
        );

        let start_menu_icon = script
            .lines()
            .find(|line| {
                line.trim_start()
                    .starts_with(r#"Name: "{autoprograms}\Ceiling""#)
            })
            .expect("installer no longer creates a Start Menu shortcut");
        assert!(
            start_menu_icon.contains(r#"AppUserModelID: "{#AppUserModelId}""#),
            "Start Menu shortcut does not claim our AUMID, so toasts will not be kept: {start_menu_icon}"
        );
    }

    fn window(now: DateTime<Utc>, offset: Duration, minutes: u32) -> RateWindow {
        RateWindow::with_details(60.0, Some(minutes), Some(now + offset), None)
    }

    #[test]
    fn predictive_warning_notifies_once_until_recovery_then_rearms() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let window = window(now, Duration::hours(3), 300);
        let risk = pace(false, Some(3600.0));
        let recovery = pace(true, None);
        let mut manager = NotificationManager::new_armed();

        assert!(!manager.record_predictive_observation(
            true,
            ProviderId::Claude,
            "oauth:person@example.com",
            PredictiveWarningWindow::Session,
            &window,
            &recovery,
        ));
        assert!(manager.record_predictive_observation(
            true,
            ProviderId::Claude,
            "oauth:person@example.com",
            PredictiveWarningWindow::Session,
            &window,
            &risk,
        ));
        assert!(!manager.record_predictive_observation(
            true,
            ProviderId::Claude,
            "oauth:person@example.com",
            PredictiveWarningWindow::Session,
            &window,
            &risk,
        ));
        assert!(!manager.record_predictive_observation(
            true,
            ProviderId::Claude,
            "oauth:person@example.com",
            PredictiveWarningWindow::Session,
            &window,
            &recovery,
        ));
        assert!(manager.record_predictive_observation(
            true,
            ProviderId::Claude,
            "oauth:person@example.com",
            PredictiveWarningWindow::Session,
            &window,
            &risk,
        ));
    }

    #[test]
    fn any_provider_can_raise_a_predictive_warning() {
        // This was gated to Claude and Codex, so enabling the setting could
        // never warn a Cursor or Copilot user no matter what their pace said.
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let window = window(now, Duration::days(3), 10080);
        let risk = pace(false, Some(3600.0));
        let recovery = pace(true, None);
        let mut manager = NotificationManager::new_armed();

        // First reading is the silent baseline, as for every provider.
        assert!(!manager.record_predictive_observation(
            true,
            ProviderId::Cursor,
            "web",
            PredictiveWarningWindow::Monthly,
            &window,
            &recovery,
        ));
        // The second confirms it and must be allowed through.
        assert!(manager.record_predictive_observation(
            true,
            ProviderId::Cursor,
            "web",
            PredictiveWarningWindow::Monthly,
            &window,
            &risk,
        ));
    }

    #[test]
    fn predictive_warnings_still_need_an_identity() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let window = window(now, Duration::days(3), 10080);
        let risk = pace(false, Some(3600.0));
        let mut manager = NotificationManager::new_armed();

        assert!(!manager.record_predictive_observation(
            true,
            ProviderId::Cursor,
            "",
            PredictiveWarningWindow::Monthly,
            &window,
            &risk,
        ));
    }

    #[test]
    fn first_predictive_risk_is_a_silent_baseline() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let window = window(now, Duration::hours(3), 300);
        let risk = pace(false, Some(3600.0));
        let mut manager = NotificationManager::new_armed();

        assert!(!manager.record_predictive_observation(
            true,
            ProviderId::Codex,
            "oauth:person@example.com",
            PredictiveWarningWindow::Session,
            &window,
            &risk,
        ));
        assert!(!manager.record_predictive_observation(
            true,
            ProviderId::Codex,
            "oauth:person@example.com",
            PredictiveWarningWindow::Session,
            &window,
            &risk,
        ));
    }

    #[test]
    fn predictive_warning_reset_jitter_does_not_retrigger() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let mut manager = NotificationManager::new_armed();
        let risk = pace(false, Some(3600.0));

        assert!(!manager.record_predictive_observation(
            true,
            ProviderId::Codex,
            "oauth:account-a",
            PredictiveWarningWindow::Weekly,
            &window(now, Duration::days(3), 10080),
            &pace(true, None),
        ));

        assert!(manager.record_predictive_observation(
            true,
            ProviderId::Codex,
            "oauth:account-a",
            PredictiveWarningWindow::Weekly,
            &window(now, Duration::days(3), 10080),
            &risk,
        ));
        assert!(!manager.record_predictive_observation(
            true,
            ProviderId::Codex,
            "oauth:account-a",
            PredictiveWarningWindow::Weekly,
            &window(now, Duration::days(3) + Duration::minutes(5), 10080),
            &risk,
        ));
    }

    #[test]
    fn predictive_warning_isolates_provider_identity_source_and_window() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let reset = window(now, Duration::hours(3), 300);
        let risk = pace(false, Some(3600.0));
        let mut manager = NotificationManager::new_armed();

        for (provider, identity, warning_window) in [
            (
                ProviderId::Claude,
                "cli:person@example.com",
                PredictiveWarningWindow::Session,
            ),
            (
                ProviderId::Claude,
                "oauth:person@example.com",
                PredictiveWarningWindow::Session,
            ),
            (
                ProviderId::Claude,
                "token-account:1",
                PredictiveWarningWindow::Session,
            ),
            (
                ProviderId::Claude,
                "oauth:person@example.com",
                PredictiveWarningWindow::Weekly,
            ),
            (
                ProviderId::Codex,
                "oauth:person@example.com",
                PredictiveWarningWindow::Session,
            ),
        ] {
            assert!(!manager.record_predictive_observation(
                true,
                provider,
                identity,
                warning_window,
                &reset,
                &pace(true, None),
            ));
            assert!(manager.record_predictive_observation(
                true,
                provider,
                identity,
                warning_window,
                &reset,
                &risk,
            ));
        }
    }

    #[test]
    fn predictive_warning_requires_enabled_confident_positive_risk() {
        let now = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let reset = window(now, Duration::hours(3), 300);
        let mut manager = NotificationManager::new();

        for (enabled, observation) in [
            (false, pace(false, Some(3600.0))),
            (true, pace(true, None)),
            (true, pace(false, Some(0.0))),
        ] {
            assert!(!manager.record_predictive_observation(
                enabled,
                ProviderId::Claude,
                "oauth:person@example.com",
                PredictiveWarningWindow::Session,
                &reset,
                &observation,
            ));
        }

        assert!(manager.record_predictive_observation(
            true,
            ProviderId::Claude,
            "oauth:person@example.com",
            PredictiveWarningWindow::Session,
            &reset,
            &pace(false, Some(3600.0)),
        ));
    }

    #[test]
    fn threshold_alerts_do_not_retrigger_across_refreshes() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();

        manager.check_and_notify(ProviderId::Cursor, None, "session", 40.0, &settings);
        manager.check_and_notify(ProviderId::Cursor, None, "session", 86.0, &settings);
        manager.check_and_notify(ProviderId::Cursor, None, "weekly", 20.0, &settings);
        manager.check_and_notify(ProviderId::Cursor, None, "session", 87.0, &settings);
        assert_eq!(manager.toasts_shown, 1);

        // Simulate many refreshes with the same split (high session, quiet weekly).
        for _ in 0..10 {
            manager.check_and_notify(ProviderId::Cursor, None, "session", 87.0, &settings);
            manager.check_and_notify(ProviderId::Cursor, None, "weekly", 25.0, &settings);
        }
        assert_eq!(
            manager.toasts_shown, 1,
            "quiet weekly must not clear session high-usage alert"
        );
    }

    #[test]
    fn a_model_pool_can_warn_like_the_windows_beside_it() {
        // Claude's Opus allowance was filtered out of `check_and_notify`, so it
        // could reach 99% in silence while the weekly window beside it warned.
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();

        // First reading is the baseline and never warns, by design.
        manager.check_and_notify(ProviderId::Claude, None, "model", 20.0, &settings);
        // A crossing waits for a second reading to confirm it, like every other
        // window: one anomalous refresh must not raise a toast on its own.
        manager.check_and_notify(ProviderId::Claude, None, "model", 92.0, &settings);
        assert_eq!(manager.toasts_shown, 0);

        manager.check_and_notify(ProviderId::Claude, None, "model", 96.0, &settings);
        assert_eq!(manager.toasts_shown, 1);
    }

    #[test]
    fn threshold_alerts_are_isolated_per_account() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();
        let work = Some("acct-work");
        let personal = Some("acct-personal");

        // Baseline both accounts below the threshold, or already-high usage at
        // first sight is treated as no crossing and never warns.
        manager.check_and_notify(ProviderId::Codex, work, "session", 40.0, &settings);
        manager.check_and_notify(ProviderId::Codex, personal, "session", 10.0, &settings);

        // What a refresh does with two accounts: every cached reading is checked
        // in one pass, in whatever order the fetches finished. The busy account
        // must still get its warning.
        for _ in 0..5 {
            manager.check_and_notify(ProviderId::Codex, work, "session", 95.0, &settings);
            manager.check_and_notify(ProviderId::Codex, personal, "session", 10.0, &settings);
        }

        assert_eq!(
            manager.toasts_shown, 1,
            "a quiet account cleared the busy account's high-usage alert"
        );
    }

    #[test]
    fn a_quiet_account_does_not_rearm_a_busy_accounts_alert() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();
        let work = Some("acct-work");
        let personal = Some("acct-personal");

        manager.check_and_notify(ProviderId::Codex, personal, "session", 10.0, &settings);
        manager.check_and_notify(ProviderId::Codex, work, "session", 40.0, &settings);

        // Reversed order: the quiet account is seen first. Sharing a key let it
        // clear the sent-alert record, so the busy account re-toasted on every
        // refresh instead of staying quiet after the first warning.
        for _ in 0..5 {
            manager.check_and_notify(ProviderId::Codex, personal, "session", 10.0, &settings);
            manager.check_and_notify(ProviderId::Codex, work, "session", 95.0, &settings);
        }

        assert_eq!(manager.toasts_shown, 1, "repeat toasts for the same limit");
    }

    #[test]
    fn threshold_alerts_are_isolated_per_window() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();

        manager.check_and_notify(ProviderId::Claude, None, "session", 40.0, &settings);
        manager.check_and_notify(ProviderId::Claude, None, "weekly", 40.0, &settings);
        manager.check_and_notify(ProviderId::Claude, None, "session", 86.0, &settings);
        manager.check_and_notify(ProviderId::Claude, None, "weekly", 92.0, &settings);
        manager.check_and_notify(ProviderId::Claude, None, "session", 87.0, &settings);
        manager.check_and_notify(ProviderId::Claude, None, "weekly", 93.0, &settings);
        assert_eq!(manager.toasts_shown, 2);

        manager.check_and_notify(ProviderId::Claude, None, "session", 87.0, &settings);
        manager.check_and_notify(ProviderId::Claude, None, "weekly", 93.0, &settings);
        assert_eq!(manager.toasts_shown, 2);
    }

    #[test]
    fn threshold_alerts_rearm_only_after_hysteresis_drop() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();

        manager.check_and_notify(ProviderId::Codex, None, "session", 40.0, &settings);
        manager.check_and_notify(ProviderId::Codex, None, "session", 86.0, &settings);
        manager.check_and_notify(ProviderId::Codex, None, "session", 87.0, &settings);
        assert_eq!(manager.toasts_shown, 1);

        // Still near the high threshold — do not re-arm.
        manager.check_and_notify(ProviderId::Codex, None, "session", 83.0, &settings);
        manager.check_and_notify(ProviderId::Codex, None, "session", 86.0, &settings);
        assert_eq!(manager.toasts_shown, 1);

        // Drop clearly below high-hysteresis, then climb again.
        manager.check_and_notify(ProviderId::Codex, None, "session", 80.0, &settings);
        manager.check_and_notify(ProviderId::Codex, None, "session", 86.0, &settings);
        manager.check_and_notify(ProviderId::Codex, None, "session", 87.0, &settings);
        assert_eq!(manager.toasts_shown, 2);
    }

    #[test]
    fn session_transition_does_not_restore_from_a_depleted_startup_baseline() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();

        manager.check_session_transition(ProviderId::Claude, 100.0, &settings);
        assert_eq!(manager.toasts_shown, 0);

        manager.check_session_transition(ProviderId::Claude, 100.0, &settings);
        assert_eq!(manager.toasts_shown, 0);

        manager.check_session_transition(ProviderId::Claude, 40.0, &settings);
        manager.check_session_transition(ProviderId::Claude, 42.0, &settings);
        assert_eq!(manager.toasts_shown, 0);
    }

    #[test]
    fn startup_baseline_suppresses_but_arms_state() {
        let mut manager = NotificationManager::new();
        let settings = Settings::default();

        manager.check_and_notify(ProviderId::Cursor, None, "session", 95.0, &settings);
        manager.check_and_notify(ProviderId::Cursor, None, "weekly", 20.0, &settings);
        manager.check_and_notify(ProviderId::Cursor, None, "session", 96.0, &settings);
        assert_eq!(manager.toasts_shown, 0);

        // Completing startup enables future transitions without replaying the
        // same already-high state observed during launch.
        manager.arm_after_startup_baseline();
        manager.check_and_notify(ProviderId::Cursor, None, "session", 95.0, &settings);
        assert_eq!(manager.toasts_shown, 0);

        manager.check_and_notify(ProviderId::Cursor, None, "session", 60.0, &settings);
        manager.check_and_notify(ProviderId::Cursor, None, "session", 86.0, &settings);
        manager.check_and_notify(ProviderId::Cursor, None, "session", 87.0, &settings);
        assert_eq!(manager.toasts_shown, 1);
    }

    #[test]
    fn first_high_reading_is_a_baseline_even_after_notifications_are_armed() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();

        manager.check_and_notify(ProviderId::Cursor, None, "monthly", 82.0, &settings);
        manager.check_and_notify(ProviderId::Cursor, None, "monthly", 83.0, &settings);

        assert_eq!(
            manager.toasts_shown, 0,
            "usage that was already high at launch is not a new crossing"
        );
    }

    #[test]
    fn baseline_severity_downgrade_does_not_create_a_warning() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();

        manager.check_and_notify(ProviderId::Cursor, None, "monthly", 95.0, &settings);
        manager.check_and_notify(ProviderId::Cursor, None, "monthly", 82.0, &settings);
        manager.check_and_notify(ProviderId::Cursor, None, "monthly", 81.0, &settings);

        assert_eq!(manager.toasts_shown, 0);
    }

    #[test]
    fn session_exhaustion_does_not_double_toast_with_transition() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();

        manager.check_session_transition(ProviderId::Claude, 50.0, &settings);
        manager.check_and_notify(ProviderId::Claude, None, "session", 100.0, &settings);
        manager.check_session_transition(ProviderId::Claude, 100.0, &settings);
        manager.check_session_transition(ProviderId::Claude, 100.0, &settings);
        assert_eq!(
            manager.toasts_shown, 1,
            "only session-depleted should fire, not Exhausted + depleted"
        );
    }

    #[test]
    fn one_read_threshold_and_session_spikes_do_not_notify() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();

        manager.check_and_notify(ProviderId::Codex, None, "session", 80.0, &settings);
        manager.check_and_notify(ProviderId::Codex, None, "session", 5.0, &settings);
        manager.check_session_transition(ProviderId::Claude, 40.0, &settings);
        manager.check_session_transition(ProviderId::Claude, 100.0, &settings);
        manager.check_session_transition(ProviderId::Claude, 42.0, &settings);

        assert_eq!(manager.toasts_shown, 0);
    }

    #[test]
    fn reset_notifications_bypass_the_cooldown_and_the_per_refresh_limit() {
        let mut manager = NotificationManager::new_armed();

        // Providers refresh concurrently, so two of them resetting in the same
        // pass must produce two toasts. Dropping the second is how a weekly
        // reset went unreported.
        manager.begin_refresh_cycle();
        assert!(manager.notify_capacity_event("First", "First trusted event"));
        assert!(manager.notify_capacity_event("Second", "Same refresh"));
        assert_eq!(manager.toasts_shown, 2);

        // A confirmed reset in a later refresh is more important than the
        // general cooldown and must remain dependable.
        manager.begin_refresh_cycle();
        assert!(manager.notify_capacity_event("Third", "Next refresh"));
        assert_eq!(manager.toasts_shown, 3);
    }

    #[test]
    fn a_reset_storm_is_still_bounded_within_one_refresh() {
        let mut manager = NotificationManager::new_armed();

        manager.begin_refresh_cycle();
        for index in 0..MAX_RESET_TOASTS_PER_REFRESH {
            assert!(
                manager.notify_capacity_event("Reset", &format!("event {index}")),
                "reset {index} should be delivered"
            );
        }
        assert!(!manager.notify_capacity_event("Reset", "one too many"));
        assert_eq!(manager.toasts_shown, MAX_RESET_TOASTS_PER_REFRESH);
    }

    #[test]
    fn a_reset_still_spends_the_usage_alert_budget_for_that_refresh() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();

        manager.begin_refresh_cycle();
        manager.check_and_notify(ProviderId::Codex, None, "session", 40.0, &settings);
        manager.check_and_notify(ProviderId::Codex, None, "session", 86.0, &settings);
        assert!(manager.notify_capacity_event("Reset", "Weekly reset"));
        manager.check_and_notify(ProviderId::Codex, None, "session", 87.0, &settings);

        assert_eq!(
            manager.toasts_shown, 1,
            "a threshold warning must not ride along behind a reset"
        );
    }

    #[test]
    fn monthly_and_unknown_windows_never_raise_usage_toasts() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();

        for window in ["monthly", "primary"] {
            manager.check_and_notify(ProviderId::Cursor, None, window, 40.0, &settings);
            manager.check_and_notify(ProviderId::Cursor, None, window, 100.0, &settings);
            manager.check_and_notify(ProviderId::Cursor, None, window, 100.0, &settings);
        }

        assert_eq!(manager.toasts_shown, 0);
    }

    #[test]
    fn critical_and_exhausted_usage_do_not_add_more_toasts() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings::default();

        manager.check_and_notify(ProviderId::Codex, None, "weekly", 40.0, &settings);
        manager.check_and_notify(ProviderId::Codex, None, "weekly", 86.0, &settings);
        manager.check_and_notify(ProviderId::Codex, None, "weekly", 87.0, &settings);
        manager.check_and_notify(ProviderId::Codex, None, "weekly", 95.0, &settings);
        manager.check_and_notify(ProviderId::Codex, None, "weekly", 100.0, &settings);

        assert_eq!(manager.toasts_shown, 1);
    }

    #[test]
    fn spend_budget_requires_a_new_confirmed_crossing_per_cycle() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings {
            spend_budget_alerts_enabled: true,
            spend_budget_warning_usd: 5.0,
            spend_budget_limit_usd: 15.0,
            ..Settings::default()
        };

        manager.check_spend_budget("daily:2026-07-19", "Daily", 4.0, &settings);
        manager.check_spend_budget("daily:2026-07-19", "Daily", 5.2, &settings);
        assert_eq!(manager.toasts_shown, 0);
        manager.check_spend_budget("daily:2026-07-19", "Daily", 5.3, &settings);
        assert_eq!(manager.toasts_shown, 1);

        manager.check_spend_budget("daily:2026-07-19", "Daily", 15.1, &settings);
        manager.check_spend_budget("daily:2026-07-19", "Daily", 15.2, &settings);
        assert_eq!(manager.toasts_shown, 2);

        manager.check_spend_budget("daily:2026-07-20", "Daily", 16.0, &settings);
        manager.check_spend_budget("daily:2026-07-20", "Daily", 16.0, &settings);
        assert_eq!(
            manager.toasts_shown, 2,
            "a high first reading of the next day is a quiet baseline"
        );
    }

    fn anomaly_settings() -> Settings {
        Settings {
            spend_anomaly_alerts_enabled: true,
            spend_anomaly_multiplier: 3.0,
            ..Settings::default()
        }
    }

    /// The median, not the mean: one heavy day must not raise the bar enough to
    /// hide a real spike. Mean of [1,1,1,1,1,1,50] is 8.0; median is 1.0.
    #[test]
    fn spend_baseline_uses_the_median_of_the_recent_days() {
        assert_eq!(
            spend_baseline_usd(vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 50.0]),
            1.0
        );
        // Even counts average the two middle values.
        assert_eq!(spend_baseline_usd(vec![2.0, 4.0, 6.0, 8.0]), 5.0);
        assert_eq!(spend_baseline_usd(Vec::new()), 0.0);
        // Junk readings are dropped rather than poisoning the baseline.
        assert_eq!(spend_baseline_usd(vec![f64::NAN, -3.0, 4.0, 4.0, 6.0]), 4.0);
    }

    /// Three leftover five-cent days beside three real $20 days put the median
    /// at $10.02 when the leftovers counted, so an ordinary $31 day read as a
    /// 3x spike — and that false alarm marks the day sent, silencing the real
    /// runaway hours later. Three $20 days satisfy the usable-day minimum so
    /// this still pins the floor filter rather than the new day-count rule.
    #[test]
    fn sub_dollar_days_do_not_count_toward_the_baseline() {
        let days = vec![0.05, 0.05, 0.05, 20.0, 20.0, 20.0];
        assert_eq!(spend_baseline_usd(days.clone()), 20.0);
        assert!(!is_spend_spike(31.0, spend_baseline_usd(days), 3.0));
        // Nothing but noise is still nothing to compare against.
        assert_eq!(spend_baseline_usd(vec![0.05, 0.20, 0.90]), 0.0);
    }

    /// One usable day is not a median. A user away for a week who does one
    /// light $1.50 session would otherwise get a $1.50 baseline, so their first
    /// ordinary $12 day back reads as 8x and burns the day's only alert
    /// (SBS-965).
    #[test]
    fn a_single_usable_day_is_not_a_baseline() {
        // Six silent days and one light session: the ticket's away-week.
        assert_eq!(
            spend_baseline_usd(vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.50]),
            0.0
        );
        assert!(!is_spend_spike(12.0, spend_baseline_usd(vec![1.50]), 3.0));
        // Two qualifying days is still a pair, not a recent median.
        assert_eq!(spend_baseline_usd(vec![1.50, 2.00]), 0.0);
        // Three is the smallest schedule the detector will compare against.
        assert_eq!(spend_baseline_usd(vec![1.50, 1.50, 1.50]), 1.50);
        assert!(is_spend_spike(
            12.0,
            spend_baseline_usd(vec![1.50, 1.50, 1.50]),
            3.0
        ));
    }

    /// A three-day working week is four zeros and three real days. Counting the
    /// zeros puts the median at 0, and a zero baseline has no ratio to fire on
    /// — so the most ordinary schedule there is would have had nothing to
    /// compare against.
    #[test]
    fn quiet_days_do_not_drag_the_baseline_to_zero() {
        let working_week = vec![0.0, 0.0, 0.0, 0.0, 20.0, 20.0, 20.0];
        assert_eq!(spend_baseline_usd(working_week.clone()), 20.0);
        assert!(is_spend_spike(80.0, spend_baseline_usd(working_week), 3.0));

        // A genuinely idle week still has nothing to compare against.
        assert_eq!(spend_baseline_usd(vec![0.0; 7]), 0.0);
    }

    /// `emit_toast` returns false for the startup gate, the per-refresh cap,
    /// and the burst limit alike. Marking the day sent before emitting spent
    /// the one spike alert on a toast nobody saw — and enrichment runs inside
    /// the same refresh that just raised a usage alert, so on a runaway day the
    /// cap is exactly what would eat it.
    #[test]
    fn a_suppressed_spike_toast_is_retried_rather_than_consumed() {
        let mut manager = NotificationManager::new();
        let settings = anomaly_settings();

        // Not armed yet, so every toast is swallowed.
        manager.check_spend_anomaly("2026-08-15", 1.0, 2.0, &settings);
        manager.check_spend_anomaly("2026-08-15", 9.0, 2.0, &settings);
        manager.check_spend_anomaly("2026-08-15", 9.5, 2.0, &settings);
        assert_eq!(manager.toasts_shown, 0);
        assert!(
            !manager.spend_anomaly_sent,
            "a swallowed toast must not count as delivered"
        );

        // The crossing stays confirmed while the toast is blocked, so the next
        // scan retries straight away instead of re-confirming for two more.
        assert!(manager.spend_anomaly_pending);

        // Same day, same spike, now deliverable — and it lands on the very
        // first scan after the slot frees up.
        manager.arm_after_startup_baseline();
        manager.check_spend_anomaly("2026-08-15", 10.0, 2.0, &settings);
        assert_eq!(manager.toasts_shown, 1);
        assert!(manager.spend_anomaly_sent);
        assert!(!manager.spend_anomaly_pending);
    }

    #[test]
    fn a_spike_needs_both_the_ratio_and_a_meaningful_total() {
        // Cents times a big multiplier are still cents.
        // SBS-967: today_usd is a sum of f64 estimates, so a day worth exactly
        // the trigger can arrive a hair under it. The toast prints "$20.00"
        // either way, so the comparison is made at the same cent precision.
        let trigger = spend_anomaly_absolute_trigger_usd();
        let a_hair_under = trigger - 1e-9;
        assert!(
            a_hair_under < trigger,
            "the fixture must actually be below the trigger in raw f64"
        );
        assert!(
            is_spend_spike(a_hair_under, 0.0, 3.0),
            "a day that rounds to the trigger must fire, not miss by a float hair"
        );
        assert!(
            !is_spend_spike(trigger - 0.01, 0.0, 3.0),
            "a cent under the trigger is a real shortfall and must not fire"
        );

        assert!(!is_spend_spike(0.30, 0.01, 3.0));
        assert!(is_spend_spike(30.0, 2.0, 3.0));
        assert!(!is_spend_spike(5.0, 2.0, 3.0));
        // Exactly at the factor counts.
        assert!(is_spend_spike(6.0, 2.0, 3.0));
        // The ratio of a zero baseline is still nothing — a multiple of
        // nothing is not a multiple. The detector then uses the absolute
        // fallback rather than staying silent (SBS-967). The old assertion
        // `!is_spend_spike(500.0, 0.0, 3.0)` was the bug: it pinned the
        // light-user mute that this ticket removes.
        assert_eq!(spend_spike_ratio(10.0, 0.0), None);
        assert!(!is_spend_spike(12.0, 0.0, 3.0));
        assert!(!is_spend_spike(19.99, 0.0, 3.0));
        assert!(is_spend_spike(20.0, 0.0, 3.0));
        assert!(is_spend_spike(180.0, 0.0, 3.0));
        // The user's ratio does not lower the absolute fallback. 3x $1 is
        // $3, which would toast an ordinary first day and the $12
        // one-usable-day case SBS-965 is guarding.
        assert!(!is_spend_spike(19.99, 0.0, 1.5));
        assert!(is_spend_spike(20.0, 0.0, 1.5));
    }

    /// A light user whose every day is under $1 has no median. They still
    /// have to hear about a $180 loop; an ordinary $12 day must not burn
    /// the day's only alert (the SBS-965 neighbor).
    #[test]
    fn a_light_user_still_hears_about_a_runaway_when_the_baseline_is_unusable() {
        let mut manager = NotificationManager::new_armed();
        let settings = anomaly_settings();
        let baseline = spend_baseline_usd(vec![0.60, 0.40, 0.80, 0.55, 0.70, 0.50, 0.90]);
        assert_eq!(baseline, 0.0);

        manager.check_spend_anomaly("2026-08-19", 1.0, baseline, &settings);
        manager.check_spend_anomaly("2026-08-19", 12.0, baseline, &settings);
        manager.check_spend_anomaly("2026-08-19", 12.0, baseline, &settings);
        assert_eq!(
            manager.toasts_shown, 0,
            "an ordinary day with no median must not toast"
        );

        manager.check_spend_anomaly("2026-08-19", 180.0, baseline, &settings);
        assert_eq!(manager.toasts_shown, 0, "one high scan is not enough");
        manager.check_spend_anomaly("2026-08-19", 180.0, baseline, &settings);
        assert_eq!(manager.toasts_shown, 1);
    }

    #[test]
    fn spend_anomaly_toast_explains_the_absolute_fallback() {
        assert_eq!(
            spend_anomaly_toast_body(9.5, 2.0),
            "Today's estimated API value is $9.50, about 4.8x your recent daily median of $2.00. This is an estimate from local Codex and Claude logs, not a bill."
        );
        assert_eq!(
            spend_anomaly_toast_body(20.0, 0.0),
            "Today's estimated API value is $20.00, at or above the $20.00 absolute trigger used when there is no usable recent median. This is an estimate from local Codex and Claude logs, not a bill."
        );
        assert_eq!(
            spend_anomaly_toast_body(180.0, 0.0),
            "Today's estimated API value is $180.00, at or above the $20.00 absolute trigger used when there is no usable recent median. This is an estimate from local Codex and Claude logs, not a bill."
        );
    }

    #[test]
    fn spend_anomaly_needs_a_confirmed_crossing_and_fires_once_a_day() {
        let mut manager = NotificationManager::new_armed();
        let settings = anomaly_settings();

        // First reading of the day is only a baseline.
        manager.check_spend_anomaly("2026-08-15", 1.0, 2.0, &settings);
        assert_eq!(manager.toasts_shown, 0);

        // One high scan is not enough; the second confirms it.
        manager.check_spend_anomaly("2026-08-15", 9.0, 2.0, &settings);
        assert_eq!(manager.toasts_shown, 0);
        manager.check_spend_anomaly("2026-08-15", 9.5, 2.0, &settings);
        assert_eq!(manager.toasts_shown, 1);

        // Spend keeps climbing, but the day has already been reported.
        manager.check_spend_anomaly("2026-08-15", 40.0, 2.0, &settings);
        manager.check_spend_anomaly("2026-08-15", 80.0, 2.0, &settings);
        assert_eq!(manager.toasts_shown, 1);
    }

    /// Rolling into a new day must not replay yesterday's spike, and an
    /// already-high first reading of the new day is a baseline, not a crossing.
    #[test]
    fn spend_anomaly_rearms_quietly_at_the_day_boundary() {
        let mut manager = NotificationManager::new_armed();
        let settings = anomaly_settings();

        manager.check_spend_anomaly("2026-08-15", 1.0, 2.0, &settings);
        manager.check_spend_anomaly("2026-08-15", 9.0, 2.0, &settings);
        manager.check_spend_anomaly("2026-08-15", 9.5, 2.0, &settings);
        assert_eq!(manager.toasts_shown, 1);

        manager.check_spend_anomaly("2026-08-16", 30.0, 2.0, &settings);
        manager.check_spend_anomaly("2026-08-16", 31.0, 2.0, &settings);
        assert_eq!(
            manager.toasts_shown, 1,
            "a high first reading of the next day is a quiet baseline"
        );
    }

    /// Dropping back under the factor must re-arm the alert, or a spike that
    /// resolves and returns would go unreported.
    #[test]
    fn spend_anomaly_clears_a_pending_crossing_that_falls_back() {
        let mut manager = NotificationManager::new_armed();
        let settings = anomaly_settings();

        manager.check_spend_anomaly("2026-08-15", 1.0, 2.0, &settings);
        manager.check_spend_anomaly("2026-08-15", 9.0, 2.0, &settings);
        // Baseline rose (the week caught up), so this is no longer a spike.
        manager.check_spend_anomaly("2026-08-15", 9.0, 8.0, &settings);
        assert_eq!(manager.toasts_shown, 0);
        assert!(!manager.spend_anomaly_pending);

        manager.check_spend_anomaly("2026-08-15", 30.0, 2.0, &settings);
        assert_eq!(manager.toasts_shown, 0, "the streak restarts");
        manager.check_spend_anomaly("2026-08-15", 31.0, 2.0, &settings);
        assert_eq!(manager.toasts_shown, 1);
    }

    #[test]
    fn disabling_spend_anomaly_alerts_clears_its_in_memory_state() {
        let mut manager = NotificationManager::new_armed();

        manager.check_spend_anomaly("2026-08-15", 1.0, 2.0, &anomaly_settings());
        assert!(manager.spend_anomaly_observed);
        assert!(manager.spend_anomaly_day.is_some());

        manager.check_spend_anomaly("", 0.0, 0.0, &Settings::default());

        assert!(!manager.spend_anomaly_observed);
        assert!(manager.spend_anomaly_day.is_none());
        assert!(!manager.spend_anomaly_sent);
        assert!(!manager.spend_anomaly_pending);
    }

    /// An out-of-band multiplier from a hand-edited settings.json must not
    /// disable the alert (1x) or make it unreachable.
    #[test]
    fn spend_anomaly_clamps_an_absurd_multiplier() {
        let mut manager = NotificationManager::new_armed();
        let settings = Settings {
            spend_anomaly_multiplier: 1.0,
            ..anomaly_settings()
        };

        manager.check_spend_anomaly("2026-08-15", 2.0, 2.0, &settings);
        // 2.0 is exactly 1x the baseline: under the 1.5 floor, so no spike.
        manager.check_spend_anomaly("2026-08-15", 2.0, 2.0, &settings);
        manager.check_spend_anomaly("2026-08-15", 2.0, 2.0, &settings);
        assert_eq!(manager.toasts_shown, 0);

        manager.check_spend_anomaly("2026-08-15", 4.0, 2.0, &settings);
        manager.check_spend_anomaly("2026-08-15", 4.0, 2.0, &settings);
        assert_eq!(manager.toasts_shown, 1, "2x clears the clamped 1.5 floor");
    }

    #[test]
    fn disabling_spend_budget_alerts_clears_its_in_memory_state() {
        let mut manager = NotificationManager::new_armed();
        let enabled = Settings {
            spend_budget_alerts_enabled: true,
            spend_budget_warning_usd: 5.0,
            spend_budget_limit_usd: 15.0,
            ..Settings::default()
        };

        manager.check_spend_budget("daily:2026-07-19", "Daily", 4.0, &enabled);
        assert!(manager.spend_budget_observed);
        assert!(manager.spend_budget_cycle.is_some());

        manager.check_spend_budget("", "", 0.0, &Settings::default());

        assert!(!manager.spend_budget_observed);
        assert!(manager.spend_budget_cycle.is_none());
        assert!(manager.spend_budget_sent.is_empty());
        assert!(manager.spend_budget_pending.is_none());
    }
}
