//! Cross-snapshot enforcement tracking.
//!
//! A window that vanishes from an otherwise-successful provider response is not
//! the same as a window the provider explicitly lifted. The provider adapters
//! already emit the explicit "not currently enforced" case as an inactive
//! window; this tracker adds the missing third state, `unavailable`, by
//! remembering which windows a provider was reporting and flagging any that
//! quietly drop out. Surfaces can then show honest uncertainty instead of
//! silently losing a limit or fabricating a percentage for it.
//!
//! Scope is `provider | data source | account identity` including directory
//! `account_id` (shared with the capacity-event observer), so accounts,
//! sources, and providers never bleed into each other. State is process-local:
//! the first read of each scope after launch is a fresh baseline that never
//! emits `unavailable`, mirroring the observer's re-baseline-on-launch
//! behaviour so changes that happened while Ceiling was closed are not
//! replayed as surprises.

use std::collections::HashMap;

use crate::capacity_events::{
    ignored_capacity_window, observation_scope, semantic_inactive_window_id, semantic_window_id,
};
use crate::commands::{InactiveRateWindowSnapshot, ProviderUsageSnapshot};

/// Providers Ceiling treats as first-class subscription meters. Only these are
/// tracked: minor or experimental providers have noisier payloads where an
/// omitted window is routine, and flagging those would be more noise than
/// signal. Keep this list small and deliberate.
const FIRST_CLASS_PROVIDERS: &[&str] = &[
    "claude", "codex", "cursor", "grok", "copilot", "gemini", "factory",
];

/// Only core subscription windows are tracked. Conditional bonus pools (promos,
/// Codex Spark, Copilot additional budget, on-demand credits) legitimately come
/// and go, so treating their disappearance as `unavailable` would be a false
/// alarm. These are exactly the ids `core_window_id` / the primary-label match
/// in `capacity_events` resolve to.
const CORE_WINDOW_IDS: &[&str] = &[
    "session", "weekly", "monthly", "plan", "auto", "api", "total",
];

const UNAVAILABLE_DESCRIPTION: &str = "Not reported in the latest update";

fn is_core_window(id: &str) -> bool {
    CORE_WINDOW_IDS.contains(&id)
}

#[derive(Debug, Default)]
pub struct EnforcementTracker {
    /// scope -> the set of core windows we expect to see, as (id -> last-seen
    /// title). Once a scope has seen a core window it stays expected until it
    /// reappears, so a window that stays missing keeps being flagged every
    /// refresh (not just the first one after it drops out).
    expected_windows: HashMap<String, HashMap<String, String>>,
}

impl EnforcementTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `unavailable` inactive rows for expected core windows that dropped
    /// out of this successful snapshot. Additive only — it never edits or
    /// removes a real window, so it cannot regress metered display or event
    /// detection. Returns the titles flagged unavailable this refresh (logging).
    pub fn annotate(&mut self, snapshot: &mut ProviderUsageSnapshot) -> Vec<String> {
        // An errored snapshot is not a successful response; do not treat a
        // window's absence as meaningful, and do not disturb the baseline.
        if snapshot.error.is_some() {
            return Vec::new();
        }
        if !FIRST_CLASS_PROVIDERS.contains(&snapshot.provider_id.as_str()) {
            return Vec::new();
        }

        let scope = observation_scope(snapshot);
        let present = present_window_identities(snapshot);

        // The first read of a scope this process is a fresh baseline: record
        // what is present and never flag, so changes that happened while Ceiling
        // was closed are not replayed as surprises.
        let Some(expected) = self.expected_windows.get_mut(&scope) else {
            self.expected_windows.insert(scope, present);
            return Vec::new();
        };

        // Anything we still expect but is absent now is unavailable. Missing
        // windows are intentionally retained in `expected` so they keep being
        // flagged until they reappear.
        let mut missing: Vec<(String, String)> = expected
            .iter()
            .filter(|(id, _)| !present.contains_key(id.as_str()))
            .map(|(id, title)| (id.clone(), title.clone()))
            .collect();
        missing.sort_by(|a, b| a.0.cmp(&b.0));

        let mut newly_unavailable = Vec::new();
        for (id, title) in missing {
            snapshot
                .inactive_rate_windows
                .push(InactiveRateWindowSnapshot {
                    id: id.clone(),
                    title: title.clone(),
                    description: UNAVAILABLE_DESCRIPTION.to_string(),
                    state: "unavailable".to_string(),
                });
            newly_unavailable.push(title);
        }

        // Merge the present set in: refresh titles and add newly-seen windows,
        // while keeping still-missing windows expected.
        for (id, title) in present {
            expected.insert(id, title);
        }
        newly_unavailable
    }
}

/// The semantic id -> title of every window the provider is currently
/// reporting, whether metered (primary/secondary) or explicitly inactive.
/// Mirrors the capacity-event observer's identity scheme so a window keeps the
/// same id as it moves between tracked and not-enforced.
fn present_window_identities(snapshot: &ProviderUsageSnapshot) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut record = |id: String, title: &str| {
        if is_core_window(&id) {
            out.insert(id, title.to_string());
        }
    };

    let primary_label = snapshot.primary_label.as_deref().unwrap_or("Plan");
    record(
        semantic_window_id(primary_label, snapshot.primary.window_minutes),
        primary_label,
    );

    if let Some(secondary) = snapshot.secondary.as_ref() {
        let label = snapshot.secondary_label.as_deref().unwrap_or("Secondary");
        record(semantic_window_id(label, secondary.window_minutes), label);
    }

    for extra in &snapshot.extra_rate_windows {
        if ignored_capacity_window(snapshot, &extra.id, &extra.title) {
            continue;
        }
        record(
            semantic_inactive_window_id(&snapshot.provider_id, &extra.id, &extra.title),
            &extra.title,
        );
    }

    for inactive in &snapshot.inactive_rate_windows {
        if ignored_capacity_window(snapshot, &inactive.id, &inactive.title) {
            continue;
        }
        record(
            semantic_inactive_window_id(&snapshot.provider_id, &inactive.id, &inactive.title),
            &inactive.title,
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{NamedRateWindowSnapshot, RateWindowSnapshot};

    fn window(minutes: Option<u32>) -> RateWindowSnapshot {
        RateWindowSnapshot {
            used_percent: 40.0,
            remaining_percent: 60.0,
            window_minutes: minutes,
            resets_at: None,
            reset_description: None,
            is_exhausted: false,
            reserve_percent: None,
            reserve_description: None,
            reserve_will_last_to_reset: false,
            reserve_eta_seconds: None,
        }
    }

    fn codex_snapshot() -> ProviderUsageSnapshot {
        ProviderUsageSnapshot {
            provider_id: "codex".into(),
            display_name: "Codex".into(),
            primary: window(Some(300)),
            primary_label: Some("5-hour".into()),
            secondary: Some(window(Some(10_080))),
            secondary_label: Some("Weekly".into()),
            model_specific: None,
            tertiary: None,
            tertiary_label: None,
            extra_rate_windows: Vec::new(),
            inactive_rate_windows: Vec::new(),
            promo_signals: Vec::new(),
            reset_credits_available: None,
            cost: None,
            plan_name: None,
            account_email: Some("person@example.com".into()),
            source_label: "oauth".into(),
            updated_at: "2026-07-17T00:00:00Z".into(),
            error: None,
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

    fn unavailable_titles(snapshot: &ProviderUsageSnapshot) -> Vec<String> {
        snapshot
            .inactive_rate_windows
            .iter()
            .filter(|w| w.state == "unavailable")
            .map(|w| w.title.clone())
            .collect()
    }

    #[test]
    fn first_read_is_a_baseline_and_never_flags_absence() {
        let mut tracker = EnforcementTracker::new();
        let mut only_five_hour = codex_snapshot();
        only_five_hour.secondary = None;
        only_five_hour.secondary_label = None;

        assert!(tracker.annotate(&mut only_five_hour).is_empty());
        assert!(only_five_hour.inactive_rate_windows.is_empty());
    }

    #[test]
    fn a_window_that_drops_out_becomes_unavailable() {
        let mut tracker = EnforcementTracker::new();
        // Baseline: both 5-hour and weekly present.
        tracker.annotate(&mut codex_snapshot());

        // Next read omits the weekly window entirely.
        let mut dropped = codex_snapshot();
        dropped.secondary = None;
        dropped.secondary_label = None;

        let flagged = tracker.annotate(&mut dropped);
        assert_eq!(flagged, vec!["Weekly".to_string()]);
        assert_eq!(unavailable_titles(&dropped), vec!["Weekly".to_string()]);
        assert_eq!(dropped.inactive_rate_windows[0].state, "unavailable");
    }

    #[test]
    fn a_persistently_missing_window_is_flagged_every_refresh() {
        // Regression: the unavailable state must be sticky, not one-shot. A
        // window that stays gone must keep being reported on every refresh, or
        // the indicator would flash once and then silently disappear.
        let mut tracker = EnforcementTracker::new();
        tracker.annotate(&mut codex_snapshot());

        for _ in 0..3 {
            let mut dropped = codex_snapshot();
            dropped.secondary = None;
            dropped.secondary_label = None;
            assert_eq!(tracker.annotate(&mut dropped), vec!["Weekly".to_string()]);
            assert_eq!(unavailable_titles(&dropped), vec!["Weekly".to_string()]);
        }
    }

    #[test]
    fn an_explicitly_lifted_window_is_not_unavailable() {
        let mut tracker = EnforcementTracker::new();
        tracker.annotate(&mut codex_snapshot());

        // Weekly is reported as inactive (provider lifted it), not absent.
        let mut lifted = codex_snapshot();
        lifted.secondary = None;
        lifted.secondary_label = None;
        lifted
            .inactive_rate_windows
            .push(InactiveRateWindowSnapshot {
                id: "codex-weekly".into(),
                title: "Weekly".into(),
                description: "Not currently enforced".into(),
                state: "notEnforced".into(),
            });

        assert!(tracker.annotate(&mut lifted).is_empty());
        assert!(unavailable_titles(&lifted).is_empty());
    }

    #[test]
    fn a_returning_window_stops_being_flagged() {
        let mut tracker = EnforcementTracker::new();
        tracker.annotate(&mut codex_snapshot());

        let mut dropped = codex_snapshot();
        dropped.secondary = None;
        dropped.secondary_label = None;
        assert_eq!(tracker.annotate(&mut dropped).len(), 1);

        // Weekly comes back on the following read.
        let mut restored = codex_snapshot();
        assert!(tracker.annotate(&mut restored).is_empty());
        assert!(unavailable_titles(&restored).is_empty());
    }

    #[test]
    fn errored_snapshot_is_ignored_and_does_not_disturb_the_baseline() {
        let mut tracker = EnforcementTracker::new();
        tracker.annotate(&mut codex_snapshot());

        let mut errored = codex_snapshot();
        errored.secondary = None;
        errored.error = Some("network".into());
        assert!(tracker.annotate(&mut errored).is_empty());
        assert!(errored.inactive_rate_windows.is_empty());

        // The failed read must not have consumed the baseline: a later
        // successful read that still omits weekly is flagged.
        let mut dropped = codex_snapshot();
        dropped.secondary = None;
        dropped.secondary_label = None;
        assert_eq!(tracker.annotate(&mut dropped), vec!["Weekly".to_string()]);
    }

    #[test]
    fn non_first_class_providers_are_not_tracked() {
        let mut tracker = EnforcementTracker::new();
        let mut first = codex_snapshot();
        first.provider_id = "venice".into();
        tracker.annotate(&mut first);

        let mut dropped = codex_snapshot();
        dropped.provider_id = "venice".into();
        dropped.secondary = None;
        dropped.secondary_label = None;
        assert!(tracker.annotate(&mut dropped).is_empty());
        assert!(dropped.inactive_rate_windows.is_empty());
    }

    #[test]
    fn different_accounts_do_not_flag_each_other() {
        let mut tracker = EnforcementTracker::new();
        tracker.annotate(&mut codex_snapshot());

        // A different account's first read is its own baseline, even though it
        // omits weekly relative to the first account.
        let mut other = codex_snapshot();
        other.account_email = Some("other@example.com".into());
        other.secondary = None;
        other.secondary_label = None;
        assert!(tracker.annotate(&mut other).is_empty());
        assert!(other.inactive_rate_windows.is_empty());
    }

    #[test]
    fn different_organizations_under_the_same_email_do_not_flag_each_other() {
        // The same email can span a personal and a business workspace with
        // different limits; switching between them must not reuse a baseline.
        let mut tracker = EnforcementTracker::new();
        tracker.annotate(&mut codex_snapshot());

        let mut other_org = codex_snapshot();
        other_org.account_organization = Some("org_business".into());
        other_org.secondary = None;
        other_org.secondary_label = None;
        assert!(tracker.annotate(&mut other_org).is_empty());
        assert!(other_org.inactive_rate_windows.is_empty());
    }

    #[test]
    fn shared_email_directory_seats_do_not_flag_each_other() {
        // Codex personal + Team seats share a login email and can share an
        // organization label; directory `account_id` is what keeps their
        // enforcement baselines apart (SBS-1079). Without it, the work seat's
        // first read would inherit the personal seat's expected windows.
        let mut tracker = EnforcementTracker::new();
        let mut personal = codex_snapshot();
        personal.account_email = Some("Same@Example.com".into());
        personal.account_organization = Some("Team".into());
        personal.account_id = Some("acct-personal".into());
        tracker.annotate(&mut personal);

        let mut work = codex_snapshot();
        work.account_email = Some("Same@Example.com".into());
        work.account_organization = Some("Team".into());
        work.account_id = Some("acct-work".into());
        work.secondary = None;
        work.secondary_label = None;
        assert!(tracker.annotate(&mut work).is_empty());
        assert!(work.inactive_rate_windows.is_empty());
    }

    #[test]
    fn conditional_bonus_windows_are_never_flagged_unavailable() {
        // Bonus pools (Spark, promos, additional budget) legitimately end. Their
        // disappearance must not be reported as an unreadable core limit.
        let mut tracker = EnforcementTracker::new();
        let mut with_spark = codex_snapshot();
        with_spark.extra_rate_windows.push(NamedRateWindowSnapshot {
            id: "codex-spark-weekly".into(),
            title: "Codex Spark Weekly".into(),
            window: window(Some(10_080)),
            amount: None,
        });
        tracker.annotate(&mut with_spark);

        // Spark disappears on the next successful read; core windows are intact.
        let mut dropped = codex_snapshot();
        assert!(tracker.annotate(&mut dropped).is_empty());
        assert!(dropped.inactive_rate_windows.is_empty());
    }

    #[test]
    fn a_core_window_that_drops_out_is_still_flagged() {
        // The valuable signal is preserved: a genuine subscription window that
        // vanishes is reported, even though bonus pools are ignored.
        let mut tracker = EnforcementTracker::new();
        let mut with_spark = codex_snapshot();
        with_spark.extra_rate_windows.push(NamedRateWindowSnapshot {
            id: "codex-spark-weekly".into(),
            title: "Codex Spark Weekly".into(),
            window: window(Some(10_080)),
            amount: None,
        });
        tracker.annotate(&mut with_spark);

        let mut dropped = codex_snapshot();
        dropped.secondary = None;
        dropped.secondary_label = None;
        assert_eq!(tracker.annotate(&mut dropped), vec!["Weekly".to_string()]);
    }

    fn cursor_snapshot() -> ProviderUsageSnapshot {
        let mut snapshot = codex_snapshot();
        snapshot.provider_id = "cursor".into();
        snapshot.display_name = "Cursor".into();
        snapshot.primary = window(Some(43_200));
        snapshot.primary_label = Some("Plan".into());
        snapshot.secondary = None;
        snapshot.secondary_label = None;
        snapshot.source_label = "web".into();
        snapshot
    }

    #[test]
    fn cursor_plan_unavailable_does_not_stick_as_monthly() {
        let mut tracker = EnforcementTracker::new();
        let mut on_demand_only = cursor_snapshot();
        on_demand_only.primary = window(None);
        on_demand_only
            .inactive_rate_windows
            .push(InactiveRateWindowSnapshot {
                id: "cursor-plan".into(),
                title: "Plan".into(),
                description: "No usage reported".into(),
                state: "unavailable".into(),
            });
        assert!(tracker.annotate(&mut on_demand_only).is_empty());

        let mut with_plan = cursor_snapshot();
        assert!(tracker.annotate(&mut with_plan).is_empty());
        assert!(
            !unavailable_titles(&with_plan)
                .iter()
                .any(|title| title == "Monthly")
        );
    }
}
