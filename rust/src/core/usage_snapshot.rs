//! Usage snapshot model - represents a point-in-time usage state for a provider

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::RateWindow;

/// Provider-specific operational data reported by a Wayfinder gateway.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WayfinderUsageSnapshot {
    pub gateway_status: String,
    pub offline: bool,
    pub dry_run: bool,
    pub missing_keys: Vec<String>,
    pub model_count: usize,
    pub models: Vec<String>,
    pub requests: u64,
    pub estimated_requests: u64,
    pub tokens: u64,
    pub realized: f64,
    pub baseline: f64,
    pub saved: f64,
    pub saved_percent: f64,
    pub period_days: u32,
    pub unit: String,
    pub priced: bool,
    pub routes: Vec<WayfinderRouteSummary>,
}

/// Per-route savings data reported by Wayfinder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WayfinderRouteSummary {
    pub name: String,
    pub requests: u64,
    pub tokens: u64,
    pub realized: f64,
    pub baseline: f64,
    pub saved: f64,
}

/// A monetary reading attached to a metered window.
///
/// Some providers meter lanes that are natively denominated in money rather
/// than percent (Cursor's on-demand overdraft, bonus credit pools). The
/// percentage still drives the bar, but the amount is the number the user
/// actually cares about, and it has nowhere else to live: `CostSnapshot` is a
/// single slot per provider and cannot describe individual lanes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowAmount {
    /// Amount consumed in this window.
    pub used: f64,
    /// Amount the window is metered against, when the provider states one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    /// Currency code (e.g. "USD").
    pub currency_code: String,
}

impl WindowAmount {
    pub fn new(used: f64, currency_code: impl Into<String>) -> Self {
        Self {
            used: finite_amount(used).unwrap_or(0.0),
            limit: None,
            currency_code: currency_code.into(),
        }
    }

    pub fn with_limit(mut self, limit: f64) -> Self {
        self.limit = finite_amount(limit);
        self
    }

    pub fn format_used(&self) -> String {
        format_currency(self.used, &self.currency_code)
    }

    pub fn format_limit(&self) -> Option<String> {
        self.limit.map(|l| format_currency(l, &self.currency_code))
    }
}

/// A labeled extra usage window surfaced by provider APIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamedRateWindow {
    pub id: String,
    pub title: String,
    pub window: RateWindow,
    /// Money behind this lane, when the provider denominates it that way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<WindowAmount>,
}

/// Explicit enforcement state for a known limit window, so surfaces never have
/// to fabricate a percentage for a window that is not actively metering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EnforcementState {
    /// The provider is actively metering this window; it carries a real
    /// percentage and appears as a normal `RateWindow`, not here.
    Tracked,
    /// The provider responded successfully and explicitly reported no active
    /// limit for this window (e.g. a lifted five-hour cap).
    NotEnforced,
    /// A window we were tracking is absent from an otherwise-successful
    /// response. We do not know whether it was lifted or merely omitted, so we
    /// surface honest uncertainty instead of a made-up value.
    Unavailable,
}

impl EnforcementState {
    /// The wire/UI token for this state (matches the camelCase serde form).
    pub fn as_str(self) -> &'static str {
        match self {
            EnforcementState::Tracked => "tracked",
            EnforcementState::NotEnforced => "notEnforced",
            EnforcementState::Unavailable => "unavailable",
        }
    }
}

/// A known provider limit window that was not reported as an active meter in an
/// otherwise-successful snapshot. This is distinct from a 0% usage window: the
/// provider has not supplied a limit to meter right now (`NotEnforced`), or the
/// window we track dropped out of the response entirely (`Unavailable`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InactiveRateWindow {
    pub id: String,
    pub title: String,
    pub description: String,
    /// Why this window is not actively metering. Defaults to `NotEnforced` so
    /// existing producers and any persisted snapshots keep their meaning.
    #[serde(default = "default_inactive_state")]
    pub state: EnforcementState,
}

fn default_inactive_state() -> EnforcementState {
    EnforcementState::NotEnforced
}

impl InactiveRateWindow {
    /// A window the provider explicitly reports as not currently enforced.
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: description.into(),
            state: EnforcementState::NotEnforced,
        }
    }

    /// A tracked window that dropped out of an otherwise-successful response.
    pub fn unavailable(
        id: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: description.into(),
            state: EnforcementState::Unavailable,
        }
    }
}

impl NamedRateWindow {
    pub fn new(id: impl Into<String>, title: impl Into<String>, window: RateWindow) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            window,
            amount: None,
        }
    }

    /// Attach the money this lane is denominated in.
    pub fn with_amount(mut self, amount: WindowAmount) -> Self {
        self.amount = Some(amount);
        self
    }
}

/// Kind of temporary promotional signal surfaced beside normal meters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PromoKind {
    /// Changes how much capacity is available (e.g. bonus pool / temporary lift).
    Boost,
    /// Quieter note about what is included in a pool (e.g. model membership).
    Inclusion,
}

/// A provider-reported promotional signal. Never invent these from plan names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromoSignal {
    pub id: String,
    pub kind: PromoKind,
    pub title: String,
    pub description: String,
    /// Optional link to a measured window id (primary/secondary/extra id).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    /// Only set when the provider supplies an explicit end/reset for the promo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<DateTime<Utc>>,
}

impl PromoSignal {
    pub fn boost(
        id: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        window_id: Option<String>,
        ends_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: PromoKind::Boost,
            title: title.into(),
            description: description.into(),
            window_id,
            ends_at,
        }
    }

    pub fn inclusion(
        id: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        window_id: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: PromoKind::Inclusion,
            title: title.into(),
            description: description.into(),
            window_id,
            ends_at: None,
        }
    }
}

/// A snapshot of usage data for a provider at a point in time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSnapshot {
    /// Primary rate window (usually session-based, e.g., 5-hour for Claude)
    pub primary: RateWindow,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_label: Option<String>,

    /// Secondary rate window (usually weekly/monthly)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary: Option<RateWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_label: Option<String>,

    /// Model-specific rate window (e.g., Opus quota for Claude)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_specific: Option<RateWindow>,

    /// Tertiary rate window (e.g., 30-day quota for Infini)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tertiary: Option<RateWindow>,

    /// Label for the tertiary window, shown in the UI instead of the "Extra"
    /// fallback. Set this when the tertiary slot has a known purpose (for
    /// example "Monthly" for the OpenCode Go monthly quota).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tertiary_label: Option<String>,

    /// Additional labeled windows that do not fit the primary/secondary/model slots.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_rate_windows: Vec<NamedRateWindow>,

    /// Known windows that a provider is not currently enforcing or reporting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inactive_rate_windows: Vec<InactiveRateWindow>,

    /// Temporary promotional signals derived only from provider-reported fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promo_signals: Vec<PromoSignal>,

    /// Provider-reported rate-limit resets that the user can apply manually.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits_available: Option<u32>,

    /// When this snapshot was captured
    pub updated_at: DateTime<Utc>,

    /// Account email if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_email: Option<String>,

    /// Account organization if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_organization: Option<String>,

    /// Login method/plan info (e.g., "Claude Pro", "Claude Max")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_method: Option<String>,
}

impl UsageSnapshot {
    /// Create a new usage snapshot with just primary window
    pub fn new(primary: RateWindow) -> Self {
        Self {
            primary,
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
            updated_at: Utc::now(),
            account_email: None,
            account_organization: None,
            login_method: None,
        }
    }

    /// Builder pattern: set secondary window
    pub fn with_secondary(mut self, secondary: RateWindow) -> Self {
        self.secondary = Some(secondary);
        self
    }

    /// Builder pattern: set model-specific window
    pub fn with_model_specific(mut self, model_specific: RateWindow) -> Self {
        self.model_specific = Some(model_specific);
        self
    }

    /// Builder pattern: set tertiary window
    pub fn with_tertiary(mut self, tertiary: RateWindow) -> Self {
        self.tertiary = Some(tertiary);
        self
    }

    /// Builder pattern: set a display label for the tertiary window
    pub fn with_tertiary_label(mut self, label: impl Into<String>) -> Self {
        self.tertiary_label = Some(label.into());
        self
    }

    /// Builder pattern: append a labeled extra rate window
    pub fn with_extra_rate_window(
        mut self,
        id: impl Into<String>,
        title: impl Into<String>,
        window: RateWindow,
    ) -> Self {
        self.extra_rate_windows
            .push(NamedRateWindow::new(id, title, window));
        self
    }

    /// Builder pattern: append a named window the provider explicitly reports
    /// as not currently enforced.
    pub fn with_inactive_rate_window(
        mut self,
        id: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.inactive_rate_windows
            .push(InactiveRateWindow::new(id, title, description));
        self
    }

    /// Builder pattern: append a tracked window that dropped out of an
    /// otherwise-successful response (state `Unavailable`).
    pub fn with_unavailable_rate_window(
        mut self,
        id: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.inactive_rate_windows
            .push(InactiveRateWindow::unavailable(id, title, description));
        self
    }

    /// Builder pattern: append a promotional signal.
    pub fn with_promo_signal(mut self, signal: PromoSignal) -> Self {
        self.promo_signals.push(signal);
        self
    }

    /// Builder pattern: set account email
    pub fn with_email(mut self, email: impl Into<String>) -> Self {
        self.account_email = Some(email.into());
        self
    }

    /// Builder pattern: set organization
    pub fn with_organization(mut self, org: impl Into<String>) -> Self {
        self.account_organization = Some(org.into());
        self
    }

    /// Builder pattern: set login method
    pub fn with_login_method(mut self, method: impl Into<String>) -> Self {
        self.login_method = Some(method.into());
        self
    }

    /// Get the most restrictive (highest used) rate window
    pub fn most_restrictive(&self) -> &RateWindow {
        let mut most = &self.primary;

        if let Some(ref secondary) = self.secondary
            && secondary.used_percent > most.used_percent
        {
            most = secondary;
        }

        if let Some(ref model_specific) = self.model_specific
            && model_specific.used_percent > most.used_percent
        {
            most = model_specific;
        }

        if let Some(ref tertiary) = self.tertiary
            && tertiary.used_percent > most.used_percent
        {
            most = tertiary;
        }

        for extra in &self.extra_rate_windows {
            if extra.window.used_percent > most.used_percent {
                most = &extra.window;
            }
        }

        most
    }

    /// Check if any rate window is exhausted
    pub fn any_exhausted(&self) -> bool {
        self.primary.is_exhausted()
            || self.secondary.as_ref().is_some_and(|w| w.is_exhausted())
            || self
                .model_specific
                .as_ref()
                .is_some_and(|w| w.is_exhausted())
            || self.tertiary.as_ref().is_some_and(|w| w.is_exhausted())
            || self
                .extra_rate_windows
                .iter()
                .any(|extra| extra.window.is_exhausted())
    }
}

/// Cost/credits snapshot for providers that support it
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostSnapshot {
    /// Amount used in the current period
    pub used: f64,

    /// Limit for the current period (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,

    /// Currency code (e.g., "USD")
    pub currency_code: String,

    /// Period description (e.g., "Monthly", "Daily")
    pub period: String,

    /// When the period resets
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<DateTime<Utc>>,

    /// When this snapshot was captured
    pub updated_at: DateTime<Utc>,
}

impl CostSnapshot {
    /// Create a new cost snapshot
    pub fn new(used: f64, currency_code: impl Into<String>, period: impl Into<String>) -> Self {
        Self {
            used: finite_amount(used).unwrap_or(0.0),
            limit: None,
            currency_code: currency_code.into(),
            period: period.into(),
            resets_at: None,
            updated_at: Utc::now(),
        }
    }

    /// Builder pattern: set limit
    pub fn with_limit(mut self, limit: f64) -> Self {
        self.limit = finite_amount(limit);
        self
    }

    /// Builder pattern: set reset time
    pub fn with_resets_at(mut self, resets_at: DateTime<Utc>) -> Self {
        self.resets_at = Some(resets_at);
        self
    }

    /// Get remaining amount if limit is set
    pub fn remaining(&self) -> Option<f64> {
        self.limit.map(|l| (l - self.used).max(0.0))
    }

    /// Get usage percentage if limit is set
    pub fn used_percent(&self) -> Option<f64> {
        self.limit.map(|l| {
            if l > 0.0 {
                (self.used / l * 100.0).min(100.0)
            } else {
                100.0
            }
        })
    }

    /// Format the cost as a currency string
    pub fn format_used(&self) -> String {
        format_currency(self.used, &self.currency_code)
    }

    /// Format the limit as a currency string
    pub fn format_limit(&self) -> Option<String> {
        self.limit.map(|l| format_currency(l, &self.currency_code))
    }
}

/// Format a value as currency
fn format_currency(value: f64, currency_code: &str) -> String {
    let value = finite_amount(value).unwrap_or(0.0);
    match currency_code.to_uppercase().as_str() {
        "USD" => format!("${:.2}", value),
        "EUR" => format!("€{:.2}", value),
        "GBP" => format!("£{:.2}", value),
        _ => format!("{:.2} {}", value, currency_code),
    }
}

fn finite_amount(value: f64) -> Option<f64> {
    value.is_finite().then_some(value.max(0.0))
}

/// Combined fetch result containing usage and optional cost data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderFetchResult {
    /// Usage data
    pub usage: UsageSnapshot,

    /// Cost/credits data if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<CostSnapshot>,

    /// Provider-specific operational data that is not quota or identity data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wayfinder_usage: Option<WayfinderUsageSnapshot>,

    /// Label describing the data source (e.g., "oauth", "web", "cli")
    pub source_label: String,
}

impl ProviderFetchResult {
    /// Create a new fetch result
    pub fn new(usage: UsageSnapshot, source_label: impl Into<String>) -> Self {
        Self {
            usage,
            cost: None,
            wayfinder_usage: None,
            source_label: source_label.into(),
        }
    }

    /// Builder pattern: set cost
    pub fn with_cost(mut self, cost: CostSnapshot) -> Self {
        self.cost = Some(cost);
        self
    }

    /// Builder pattern: set Wayfinder operational data.
    pub fn with_wayfinder_usage(mut self, usage: WayfinderUsageSnapshot) -> Self {
        self.wayfinder_usage = Some(usage);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_snapshot_ignores_non_finite_values() {
        let cost = CostSnapshot::new(f64::NAN, "USD", "Monthly").with_limit(f64::INFINITY);

        assert_eq!(cost.used, 0.0);
        assert_eq!(cost.limit, None);
        assert_eq!(cost.used_percent(), None);
        assert_eq!(cost.format_used(), "$0.00");
    }

    #[test]
    fn enforcement_state_serializes_as_camel_case() {
        assert_eq!(
            serde_json::to_string(&EnforcementState::NotEnforced).unwrap(),
            "\"notEnforced\""
        );
        assert_eq!(
            serde_json::to_string(&EnforcementState::Unavailable).unwrap(),
            "\"unavailable\""
        );
        assert_eq!(EnforcementState::Unavailable.as_str(), "unavailable");
    }

    #[test]
    fn inactive_rate_window_state_defaults_to_not_enforced_for_legacy_json() {
        // Snapshots persisted before the state field must still deserialize.
        let legacy = r#"{"id":"codex-weekly","title":"Weekly","description":"x"}"#;
        let window: InactiveRateWindow = serde_json::from_str(legacy).unwrap();
        assert_eq!(window.state, EnforcementState::NotEnforced);

        let unavailable = InactiveRateWindow::unavailable("weekly", "Weekly", "gone");
        let round_tripped: InactiveRateWindow =
            serde_json::from_str(&serde_json::to_string(&unavailable).unwrap()).unwrap();
        assert_eq!(round_tripped.state, EnforcementState::Unavailable);
    }
}
