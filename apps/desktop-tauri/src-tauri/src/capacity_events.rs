use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Duration, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::commands::{ProviderUsageSnapshot, RateWindowSnapshot};

const USED_DROP_THRESHOLD: f64 = 20.0;
const RESET_JITTER_MINUTES: i64 = 10;
const RESET_SHIFT_MINUTES: i64 = 30;
const CONFIRM_USED_TOLERANCE: f64 = 10.0;
const CONFIRMATION_MIN_AGE_SECONDS: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CapacityEventKind {
    ScheduledReset,
    SurpriseReset,
    PartialReset,
    BankedResetGranted,
    ResetTimeShift,
    WindowLifted,
    WindowRestored,
    AllowanceGranted,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityEventPayload {
    pub provider_id: String,
    pub display_name: String,
    pub window_id: String,
    pub window_label: String,
    /// Cadence of the window that changed, when the provider reports one. Lets
    /// consumers tell a 5-hour session boundary from a weekly or monthly one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_minutes: Option<u32>,
    pub kind: CapacityEventKind,
    pub previous_used_percent: f64,
    pub current_used_percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_reset_credits: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_reset_credits: Option<u32>,
    pub previous_reset_at: String,
    pub current_reset_at: String,
    pub occurred_at: String,
    /// True when this was detected on the first reading after launch, i.e. it
    /// happened while Ceiling was not running. The event is real, but
    /// `occurred_at` is when it was *noticed*, not when it happened.
    #[serde(default)]
    pub while_away: bool,
}

/// Windows at or under this cadence reset several times a day.
const SHORT_WINDOW_MAX_MINUTES: u32 = 720;

impl CapacityEventPayload {
    /// True for weekly, monthly and other long windows.
    ///
    /// A 5-hour session boundary comes round several times a day and is exactly
    /// the reset a user already expects, so toasting each one is noise. Weekly
    /// and monthly boundaries are rare and worth interrupting for. Providers
    /// that omit a cadence fall back to the semantic window id.
    pub fn is_long_window(&self) -> bool {
        match self.window_minutes {
            Some(minutes) => minutes > SHORT_WINDOW_MAX_MINUTES,
            None => self.window_id != "session",
        }
    }

    pub fn notification_title(&self) -> String {
        match self.kind {
            CapacityEventKind::ScheduledReset => format!("{} reset", self.display_name),
            CapacityEventKind::SurpriseReset => {
                format!("{} capacity restored early", self.display_name)
            }
            CapacityEventKind::PartialReset => {
                format!("{} capacity partially restored", self.display_name)
            }
            CapacityEventKind::BankedResetGranted => {
                format!("{} banked reset available", self.display_name)
            }
            CapacityEventKind::ResetTimeShift => {
                format!("{} reset time changed", self.display_name)
            }
            CapacityEventKind::WindowLifted => {
                format!("{} limit lifted", self.display_name)
            }
            CapacityEventKind::WindowRestored => {
                format!("{} limit restored", self.display_name)
            }
            CapacityEventKind::AllowanceGranted => {
                format!("{} capacity added", self.display_name)
            }
        }
    }

    /// Local time this happened, when it is known well enough to name.
    fn occurred_local_time(&self) -> Option<String> {
        DateTime::parse_from_rfc3339(&self.occurred_at)
            .ok()
            .map(|at| at.with_timezone(&Local).format("%-I:%M %p").to_string())
    }

    pub fn notification_body(&self) -> String {
        let base = self.notification_body_live();
        if !self.while_away {
            return base;
        }
        // Say plainly that this already happened. Announcing it as if it just
        // occurred is what the old behavior was avoiding by staying silent, and
        // silence is worse than a timestamp.
        match self.occurred_local_time() {
            Some(time) => format!("{base} This happened at {time}, while Ceiling was closed."),
            None => format!("{base} This happened while Ceiling was closed."),
        }
    }

    fn notification_body_live(&self) -> String {
        let remaining = (100.0 - self.current_used_percent).clamp(0.0, 100.0);
        match self.kind {
            CapacityEventKind::ScheduledReset => format!(
                "{} reset on schedule. {:.0}% available now.",
                self.window_label, remaining
            ),
            CapacityEventKind::SurpriseReset => format!(
                "{} reset earlier than expected. {:.0}% available now.",
                self.window_label, remaining
            ),
            CapacityEventKind::PartialReset => format!(
                "{} dropped from {:.0}% to {:.0}% used. {:.0}% available now.",
                self.window_label, self.previous_used_percent, self.current_used_percent, remaining
            ),
            CapacityEventKind::BankedResetGranted => {
                let available = self.current_reset_credits.unwrap_or(1);
                format!(
                    "{} banked reset{} available now.",
                    available,
                    if available == 1 { " is" } else { "s are" }
                )
            }
            CapacityEventKind::ResetTimeShift => {
                format!("{} now has a different reset time.", self.window_label)
            }
            CapacityEventKind::WindowLifted => {
                format!(
                    "{} is no longer reporting an active limit.",
                    self.window_label
                )
            }
            CapacityEventKind::WindowRestored => {
                format!("{} is reporting an active limit again.", self.window_label)
            }
            CapacityEventKind::AllowanceGranted => format!(
                "{} is newly available with {:.0}% remaining.",
                self.window_label, remaining
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObservedWindow {
    id: String,
    label: String,
    used_percent: f64,
    resets_at: DateTime<Utc>,
    /// Absent in baselines written by older builds, and for providers that do
    /// not report a cadence.
    #[serde(default)]
    window_minutes: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderObservation {
    observed_at: DateTime<Utc>,
    windows: HashMap<String, ObservedWindow>,
    #[serde(default)]
    inactive_windows: HashMap<String, String>,
    #[serde(default)]
    extra_window_ids: HashSet<String>,
    #[serde(default)]
    reset_credits_available: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingReset {
    event: PersistedEvent,
    candidate: ObservedWindow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingResetCredits {
    event: PersistedEvent,
    candidate: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum CandidateState {
    Active(ObservedWindow),
    Inactive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingTransition {
    event: PersistedEvent,
    candidate: CandidateState,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EmittedScheduledReset {
    scope: String,
    window_id: String,
    reset_boundary: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedEvent {
    provider_id: String,
    display_name: String,
    window_id: String,
    window_label: String,
    #[serde(default)]
    window_minutes: Option<u32>,
    kind: CapacityEventKind,
    previous_used_percent: f64,
    current_used_percent: f64,
    #[serde(default)]
    previous_reset_credits: Option<u32>,
    #[serde(default)]
    current_reset_credits: Option<u32>,
    previous_reset_at: DateTime<Utc>,
    current_reset_at: DateTime<Utc>,
    occurred_at: DateTime<Utc>,
    #[serde(default)]
    while_away: bool,
}

impl PersistedEvent {
    fn payload(self) -> CapacityEventPayload {
        CapacityEventPayload {
            provider_id: self.provider_id,
            display_name: self.display_name,
            window_id: self.window_id,
            window_label: self.window_label,
            window_minutes: self.window_minutes,
            kind: self.kind,
            previous_used_percent: self.previous_used_percent,
            current_used_percent: self.current_used_percent,
            previous_reset_credits: self.previous_reset_credits,
            current_reset_credits: self.current_reset_credits,
            previous_reset_at: self.previous_reset_at.to_rfc3339(),
            current_reset_at: self.current_reset_at.to_rfc3339(),
            occurred_at: self.occurred_at.to_rfc3339(),
            while_away: self.while_away,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CapacityEventObserver {
    baselines: HashMap<String, ProviderObservation>,
    /// Confirmation candidates are process-local. Persisting them replays an
    /// unfinished event from the previous run as a fresh launch notification.
    #[serde(skip)]
    pending_resets: HashMap<String, PendingReset>,
    #[serde(skip)]
    pending_reset_credits: HashMap<String, PendingResetCredits>,
    #[serde(skip)]
    pending_transitions: HashMap<String, PendingTransition>,
    /// Scheduled resets are trusted on their first post-boundary reading. Keep
    /// a process-local cycle key as a final guard against replaying that same
    /// reset if unrelated provider-window churn temporarily pins a baseline.
    #[serde(skip)]
    emitted_scheduled_resets: HashSet<EmittedScheduledReset>,
    /// Every provider/account scope is re-baselined on its first live reading
    /// after launch so changes that happened while Ceiling was closed are not
    /// emitted as if they just occurred.
    #[serde(skip)]
    seen_scopes: HashSet<String>,
    #[serde(skip)]
    persistence_path: Option<PathBuf>,
}

impl CapacityEventObserver {
    pub fn load_default() -> Self {
        let path = persistence_path();
        let Some(path_ref) = path.as_ref() else {
            return Self::default();
        };
        let mut observer = fs::read_to_string(path_ref)
            .ok()
            .and_then(|contents| serde_json::from_str::<Self>(&contents).ok())
            .unwrap_or_default();
        // Explicitly discard candidates written by older builds.
        observer.pending_resets.clear();
        observer.pending_reset_credits.clear();
        observer.pending_transitions.clear();
        observer.persistence_path = path;
        observer
    }

    pub fn observe(&mut self, snapshot: &ProviderUsageSnapshot) -> Vec<CapacityEventPayload> {
        if snapshot.error.is_some() {
            return Vec::new();
        }
        let now = DateTime::parse_from_rfc3339(&snapshot.updated_at)
            .map(|value| value.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let scope = observation_scope(snapshot);
        let (windows, extra_window_ids) = observed_windows(snapshot);
        let current = ProviderObservation {
            observed_at: now,
            windows,
            inactive_windows: inactive_windows(snapshot),
            extra_window_ids,
            reset_credits_available: snapshot.reset_credits_available,
        };

        if self.seen_scopes.insert(scope.clone()) {
            // First reading of this scope since launch. The persisted baseline is
            // from a previous run, so anything that changed did so while Ceiling
            // was not watching. Report it as such rather than staying silent:
            // saying nothing is what made an overnight reset invisible.
            let away = self
                .baselines
                .get(&scope)
                .map(|previous| detect_away_events(snapshot, previous, &current))
                .unwrap_or_default();
            self.baselines.insert(scope, current);
            self.persist();
            return away;
        }

        let Some(previous) = self.baselines.get(&scope).cloned() else {
            self.baselines.insert(scope, current);
            self.persist();
            return Vec::new();
        };

        let mut emitted = Vec::new();
        let mut held_for_confirmation = false;
        let mut confirmed_windows = HashSet::new();
        let mut scheduled_window_updates = Vec::new();
        let reset_credit_key = format!("{scope}:banked-resets");
        let mut confirmed_reset_credits = false;
        if let Some(pending) = self.pending_reset_credits.get(&reset_credit_key).cloned() {
            if current.reset_credits_available == Some(pending.candidate) {
                if confirmation_is_mature(pending.event.occurred_at, current.observed_at) {
                    self.pending_reset_credits.remove(&reset_credit_key);
                    emitted.push(pending.event.payload());
                    confirmed_reset_credits = true;
                } else {
                    held_for_confirmation = true;
                }
            } else {
                self.pending_reset_credits.remove(&reset_credit_key);
            }
        }
        if !confirmed_reset_credits
            && !self.pending_reset_credits.contains_key(&reset_credit_key)
            && let Some(event) = detect_banked_reset_grant(snapshot, &previous, &current)
        {
            let candidate = current.reset_credits_available.unwrap_or_default();
            self.pending_reset_credits
                .insert(reset_credit_key, PendingResetCredits { event, candidate });
            held_for_confirmation = true;
        }
        for (window_id, current_window) in &current.windows {
            let pending_key = format!("{scope}:{window_id}");
            if let Some(pending) = self.pending_resets.get(&pending_key).cloned() {
                if consistent_confirmation(&pending.candidate, current_window) {
                    if confirmation_is_mature(pending.event.occurred_at, current.observed_at) {
                        self.pending_resets.remove(&pending_key);
                        emitted.push(pending.event.payload());
                        confirmed_windows.insert(window_id.clone());
                    } else {
                        held_for_confirmation = true;
                    }
                    continue;
                }
                self.pending_resets.remove(&pending_key);
            }

            let Some(previous_window) = previous.windows.get(window_id) else {
                continue;
            };
            let Some(event) = detect_reset(
                snapshot,
                &previous,
                &current,
                previous_window,
                current_window,
            ) else {
                continue;
            };
            if event.kind == CapacityEventKind::ScheduledReset {
                let reset_key = EmittedScheduledReset {
                    scope: scope.clone(),
                    window_id: window_id.clone(),
                    reset_boundary: event.previous_reset_at,
                };
                self.emitted_scheduled_resets.retain(|existing| {
                    existing.scope != reset_key.scope
                        || existing.window_id != reset_key.window_id
                        || existing.reset_boundary == reset_key.reset_boundary
                });
                if self.emitted_scheduled_resets.insert(reset_key) {
                    emitted.push(event.payload());
                }
                // A scheduled reset is independently corroborated by its old
                // boundary having passed. Commit this window immediately even
                // if another window in the same provider response needs a
                // confirming read. Holding the whole provider baseline here is
                // what replayed Copilot's monthly reset on every refresh while
                // its quota list was changing at the boundary.
                scheduled_window_updates.push((window_id.clone(), current_window.clone()));
                confirmed_windows.insert(window_id.clone());
            } else {
                self.pending_resets.insert(
                    pending_key,
                    PendingReset {
                        event,
                        candidate: current_window.clone(),
                    },
                );
                held_for_confirmation = true;
            }
        }

        for window_id in transition_window_ids(&previous, &current) {
            if confirmed_windows.contains(&window_id) {
                continue;
            }
            let pending_key = format!("{scope}:transition:{window_id}");
            if let Some(pending) = self.pending_transitions.get(&pending_key).cloned() {
                if transition_is_consistent(&pending.candidate, &current, &window_id) {
                    if confirmation_is_mature(pending.event.occurred_at, current.observed_at) {
                        self.pending_transitions.remove(&pending_key);
                        emitted.push(pending.event.payload());
                        confirmed_windows.insert(window_id.clone());
                    } else {
                        held_for_confirmation = true;
                    }
                    continue;
                }
                self.pending_transitions.remove(&pending_key);
            }

            let Some((event, candidate)) =
                detect_transition(snapshot, &previous, &current, &window_id)
            else {
                continue;
            };
            self.pending_transitions
                .insert(pending_key, PendingTransition { event, candidate });
            held_for_confirmation = true;
        }

        if held_for_confirmation {
            if let Some(baseline) = self.baselines.get_mut(&scope) {
                for (window_id, window) in scheduled_window_updates {
                    baseline.windows.insert(window_id, window);
                }
            }
        } else {
            self.baselines.insert(scope, current);
        }
        self.persist();
        emitted
    }

    fn persist(&self) {
        let Some(path) = self.persistence_path.as_ref() else {
            return;
        };
        if let Some(parent) = path.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            tracing::warn!("failed to create capacity-event directory: {error}");
            return;
        }
        match serde_json::to_vec_pretty(self) {
            Ok(bytes) => {
                if let Err(error) = codexbar::secure_file::atomic_write(path, &bytes) {
                    tracing::warn!("failed to persist capacity-event history: {error}");
                }
            }
            Err(error) => tracing::warn!("failed to serialize capacity-event history: {error}"),
        }
    }
}

/// How stale a baseline may be and still be worth announcing against.
///
/// Reopening Ceiling after a week should not replay a reset nobody is waiting to
/// hear about; reopening the morning after should.
const AWAY_BASELINE_MAX_AGE_HOURS: i64 = 24;

/// Resets found on the first reading after launch.
///
/// Only *scheduled* resets qualify. The live path emits those immediately but
/// holds surprise, partial and shift events for a confirming second reading,
/// because a lone reading can be anomalous. After a restart there is no earlier
/// reading this session to confirm against, so anything needing confirmation
/// cannot be trusted and is silently absorbed into the new baseline as before.
///
/// A scheduled reset needs no second opinion: the window's own reset time having
/// passed is corroboration independent of the usage number.
fn detect_away_events(
    snapshot: &ProviderUsageSnapshot,
    previous: &ProviderObservation,
    current: &ProviderObservation,
) -> Vec<CapacityEventPayload> {
    if current.observed_at - previous.observed_at > Duration::hours(AWAY_BASELINE_MAX_AGE_HOURS) {
        return Vec::new();
    }

    let mut emitted = Vec::new();
    for (window_id, current_window) in &current.windows {
        let Some(previous_window) = previous.windows.get(window_id) else {
            continue;
        };
        let Some(mut event) =
            detect_reset(snapshot, previous, current, previous_window, current_window)
        else {
            continue;
        };
        if event.kind != CapacityEventKind::ScheduledReset {
            continue;
        }
        event.while_away = true;
        // The window was due to reset at its previous reset time, which is a far
        // better answer to "when did this happen" than "when I noticed".
        if previous_window.resets_at > previous.observed_at
            && previous_window.resets_at <= current.observed_at
        {
            event.occurred_at = previous_window.resets_at;
        }
        emitted.push(event.payload());
    }
    emitted
}

fn detect_reset(
    snapshot: &ProviderUsageSnapshot,
    previous_observation: &ProviderObservation,
    current_observation: &ProviderObservation,
    previous: &ObservedWindow,
    current: &ObservedWindow,
) -> Option<PersistedEvent> {
    let used_drop = previous.used_percent - current.used_percent;
    let reset_advanced =
        current.resets_at > previous.resets_at + Duration::minutes(RESET_JITTER_MINUTES);
    let scheduled = previous.resets_at >= previous_observation.observed_at - Duration::minutes(5)
        && previous.resets_at <= current_observation.observed_at + Duration::minutes(5)
        && reset_advanced;
    let reset_shift =
        (current.resets_at - previous.resets_at).num_minutes().abs() >= RESET_SHIFT_MINUTES;
    let reset_unchanged =
        (current.resets_at - previous.resets_at).num_minutes().abs() <= RESET_JITTER_MINUTES;
    let kind = if scheduled {
        CapacityEventKind::ScheduledReset
    } else if used_drop >= USED_DROP_THRESHOLD && reset_advanced {
        CapacityEventKind::SurpriseReset
    } else if used_drop >= USED_DROP_THRESHOLD && reset_unchanged {
        // Some providers restore only part of a pool without moving its normal
        // reset date. Treat a large, confirmed decrease in used capacity as a
        // real event while leaving small corrections and reset-time churn alone.
        CapacityEventKind::PartialReset
    } else if reset_shift {
        CapacityEventKind::ResetTimeShift
    } else {
        return None;
    };
    Some(PersistedEvent {
        provider_id: snapshot.provider_id.clone(),
        display_name: snapshot.display_name.clone(),
        window_id: current.id.clone(),
        window_label: current.label.clone(),
        window_minutes: current.window_minutes,
        kind,
        previous_used_percent: previous.used_percent,
        current_used_percent: current.used_percent,
        previous_reset_credits: None,
        current_reset_credits: None,
        previous_reset_at: previous.resets_at,
        current_reset_at: current.resets_at,
        occurred_at: current_observation.observed_at,
        while_away: false,
    })
}

fn detect_banked_reset_grant(
    snapshot: &ProviderUsageSnapshot,
    previous: &ProviderObservation,
    current: &ProviderObservation,
) -> Option<PersistedEvent> {
    if snapshot.provider_id != "codex" {
        return None;
    }
    let previous_credits = previous.reset_credits_available?;
    let current_credits = current.reset_credits_available?;
    if current_credits <= previous_credits {
        return None;
    }
    Some(PersistedEvent {
        provider_id: snapshot.provider_id.clone(),
        display_name: snapshot.display_name.clone(),
        window_id: "banked-resets".to_string(),
        window_label: "Banked resets".to_string(),
        // Not tied to a rate window; a granted reset credit is always worth
        // reporting, so leave the cadence unset rather than inventing one.
        window_minutes: None,
        kind: CapacityEventKind::BankedResetGranted,
        previous_used_percent: 0.0,
        current_used_percent: 0.0,
        previous_reset_credits: Some(previous_credits),
        current_reset_credits: Some(current_credits),
        previous_reset_at: previous.observed_at,
        current_reset_at: current.observed_at,
        occurred_at: current.observed_at,
        while_away: false,
    })
}

fn consistent_confirmation(candidate: &ObservedWindow, current: &ObservedWindow) -> bool {
    (candidate.used_percent - current.used_percent).abs() <= CONFIRM_USED_TOLERANCE
        && (candidate.resets_at - current.resets_at)
            .num_minutes()
            .abs()
            <= RESET_JITTER_MINUTES
}

fn confirmation_is_mature(candidate_at: DateTime<Utc>, current_at: DateTime<Utc>) -> bool {
    current_at - candidate_at >= Duration::seconds(CONFIRMATION_MIN_AGE_SECONDS)
}

fn transition_window_ids(
    previous: &ProviderObservation,
    current: &ProviderObservation,
) -> HashSet<String> {
    previous
        .windows
        .keys()
        .chain(previous.inactive_windows.keys())
        .chain(current.windows.keys())
        .chain(current.inactive_windows.keys())
        .cloned()
        .collect()
}

fn transition_is_consistent(
    candidate: &CandidateState,
    current: &ProviderObservation,
    window_id: &str,
) -> bool {
    match candidate {
        CandidateState::Active(window) => current
            .windows
            .get(window_id)
            .is_some_and(|current| consistent_confirmation(window, current)),
        CandidateState::Inactive => current.inactive_windows.contains_key(window_id),
    }
}

fn detect_transition(
    snapshot: &ProviderUsageSnapshot,
    previous: &ProviderObservation,
    current: &ProviderObservation,
    window_id: &str,
) -> Option<(PersistedEvent, CandidateState)> {
    let previous_active = previous.windows.get(window_id);
    let current_active = current.windows.get(window_id);
    let previous_inactive = previous.inactive_windows.get(window_id);
    let current_inactive = current.inactive_windows.get(window_id);

    let (kind, label, candidate) =
        if let (Some(_), Some(label)) = (previous_active, current_inactive) {
            (
                CapacityEventKind::WindowLifted,
                label.clone(),
                CandidateState::Inactive,
            )
        } else if let (Some(label), Some(current)) = (previous_inactive, current_active) {
            (
                CapacityEventKind::WindowRestored,
                label.clone(),
                CandidateState::Active(current.clone()),
            )
        } else if previous_active.is_none()
            && previous_inactive.is_none()
            && current.extra_window_ids.contains(window_id)
        {
            let current = current_active?;
            (
                CapacityEventKind::AllowanceGranted,
                current.label.clone(),
                CandidateState::Active(current.clone()),
            )
        } else {
            return None;
        };

    let previous_used_percent = previous_active.map_or(0.0, |window| window.used_percent);
    let current_used_percent = current_active.map_or(0.0, |window| window.used_percent);
    let previous_reset_at = previous_active
        .map(|window| window.resets_at)
        .unwrap_or(previous.observed_at);
    let current_reset_at = current_active
        .map(|window| window.resets_at)
        .unwrap_or(current.observed_at);
    Some((
        PersistedEvent {
            provider_id: snapshot.provider_id.clone(),
            display_name: snapshot.display_name.clone(),
            window_id: window_id.to_string(),
            window_label: label,
            window_minutes: current_active.and_then(|window| window.window_minutes),
            kind,
            previous_used_percent,
            current_used_percent,
            previous_reset_credits: None,
            current_reset_credits: None,
            previous_reset_at,
            current_reset_at,
            occurred_at: current.observed_at,
            while_away: false,
        },
        candidate,
    ))
}

fn observed_windows(
    snapshot: &ProviderUsageSnapshot,
) -> (HashMap<String, ObservedWindow>, HashSet<String>) {
    let mut windows = HashMap::new();
    let mut extra_window_ids = HashSet::new();
    push_window(
        &mut windows,
        snapshot.primary_label.as_deref().unwrap_or("Plan"),
        &snapshot.primary,
    );
    if let Some(window) = snapshot.secondary.as_ref() {
        push_window(
            &mut windows,
            snapshot.secondary_label.as_deref().unwrap_or("Secondary"),
            window,
        );
    }
    // The model lane shares a cadence with the weekly window (Claude's 7-day
    // Opus pool is also 10080 minutes), so a cadence-derived id would land on
    // "weekly" and one window would silently replace the other in this map.
    if let Some(window) = snapshot.model_specific.as_ref()
        && let Some(mut observed) = to_observed_window("Model", window)
    {
        observed.id = "model".to_string();
        windows.insert(observed.id.clone(), observed);
    }
    // Same hazard for the third window: most are monthly, but a provider whose
    // tertiary matches its secondary cadence must not overwrite it. Core slots
    // are already in the map, so an id they hold means this one needs its own.
    if let Some(window) = snapshot.tertiary.as_ref() {
        let label = snapshot.tertiary_label.as_deref().unwrap_or("Extra");
        if let Some(mut observed) = to_observed_window(label, window) {
            if windows.contains_key(&observed.id) {
                observed.id = "tertiary".to_string();
            }
            windows.insert(observed.id.clone(), observed);
        }
    }
    for extra in &snapshot.extra_rate_windows {
        if ignored_capacity_window(snapshot, &extra.id, &extra.title) {
            continue;
        }
        let id = semantic_inactive_window_id(&snapshot.provider_id, &extra.id, &extra.title);
        if let Some(mut observed) = to_observed_window(&extra.title, &extra.window) {
            observed.id.clone_from(&id);
            windows.insert(id.clone(), observed);
            extra_window_ids.insert(id);
        }
    }
    let unavailable = unavailable_window_ids(snapshot);
    windows.retain(|id, _| !unavailable.contains(id));
    extra_window_ids.retain(|id| windows.contains_key(id));
    (windows, extra_window_ids)
}

fn unavailable_window_ids(snapshot: &ProviderUsageSnapshot) -> HashSet<String> {
    snapshot
        .inactive_rate_windows
        .iter()
        .filter(|window| window.state == "unavailable")
        .map(|window| semantic_inactive_window_id(&snapshot.provider_id, &window.id, &window.title))
        .collect()
}

fn inactive_windows(snapshot: &ProviderUsageSnapshot) -> HashMap<String, String> {
    snapshot
        .inactive_rate_windows
        .iter()
        .filter(|window| window.state != "unavailable")
        .filter(|window| !ignored_capacity_window(snapshot, &window.id, &window.title))
        .map(|window| {
            (
                semantic_inactive_window_id(&snapshot.provider_id, &window.id, &window.title),
                window.title.clone(),
            )
        })
        .collect()
}

pub(crate) fn ignored_capacity_window(
    snapshot: &ProviderUsageSnapshot,
    id: &str,
    title: &str,
) -> bool {
    if snapshot.provider_id != "cursor" {
        return false;
    }
    let identity = normalize_window_id(&format!("{id}-{title}"));
    identity.contains("promotional")
        || identity.contains("on-demand")
        || identity.contains("ondemand")
}

pub(crate) fn semantic_inactive_window_id(provider_id: &str, id: &str, title: &str) -> String {
    let title_id = normalize_window_id(title);
    if let Some(core_id) = core_window_id(&title_id) {
        return core_id.to_string();
    }
    // Named extra allowances must keep their own identity even when their
    // cadence is weekly/monthly. Otherwise Codex Spark Weekly overwrites the
    // regular Codex Weekly baseline and hides a real reset.
    if !title_id.is_empty() {
        return title_id;
    }
    let normalized = normalize_window_id(id);
    let without_provider = normalized
        .strip_prefix(&format!("{}-", normalize_window_id(provider_id)))
        .unwrap_or(&normalized);
    core_window_id(without_provider)
        .unwrap_or(without_provider)
        .to_string()
}

fn core_window_id(normalized: &str) -> Option<&'static str> {
    match normalized {
        "auto" => Some("auto"),
        "api" => Some("api"),
        "total" => Some("total"),
        "plan" => Some("plan"),
        "weekly" => Some("weekly"),
        "monthly" => Some("monthly"),
        "session" | "session-5h" | "session-5-hour" | "5-hour" | "five-hour" => Some("session"),
        _ => None,
    }
}

fn push_window(
    windows: &mut HashMap<String, ObservedWindow>,
    label: &str,
    window: &RateWindowSnapshot,
) {
    if let Some(observed) = to_observed_window(label, window) {
        windows.insert(observed.id.clone(), observed);
    }
}

fn to_observed_window(label: &str, window: &RateWindowSnapshot) -> Option<ObservedWindow> {
    let resets_at = window
        .resets_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())?
        .with_timezone(&Utc);
    Some(ObservedWindow {
        id: semantic_window_id(label, window.window_minutes),
        label: label.to_string(),
        used_percent: window.used_percent,
        resets_at,
        window_minutes: window.window_minutes,
    })
}

pub(crate) fn semantic_window_id(label: &str, window_minutes: Option<u32>) -> String {
    let normalized = normalize_window_id(label);
    if matches!(
        normalized.as_str(),
        "auto" | "api" | "total" | "plan" | "weekly" | "monthly"
    ) {
        return normalized;
    }
    match window_minutes {
        // Shares its cutoff with `is_long_window` so the semantic id and the
        // notify/skip decision cannot classify the same window differently.
        Some(minutes) if minutes <= SHORT_WINDOW_MAX_MINUTES => "session".to_string(),
        Some(minutes) if minutes <= 20_160 => "weekly".to_string(),
        Some(_) => "monthly".to_string(),
        None => normalized,
    }
}

fn normalize_window_id(value: &str) -> String {
    value
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

pub(crate) fn observation_scope(snapshot: &ProviderUsageSnapshot) -> String {
    // Scope by every account identifier, not either/or: the same email can
    // belong to different organizations (e.g. a personal vs a business
    // workspace) with distinct limits, and those must never share a baseline.
    // Directory seats that share a login email (Codex personal + Team) also
    // stamp distinct `account_id`s; omitting that id collapsed them (SBS-1079).
    let email = snapshot.account_email.as_deref().unwrap_or("");
    let organization = snapshot.account_organization.as_deref().unwrap_or("");
    let account_id = snapshot
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("");
    let raw = format!(
        "{}|{}|{}|{}|{}",
        snapshot.provider_id, snapshot.source_label, email, organization, account_id
    );
    format!("{}:{:016x}", snapshot.provider_id, fnv1a64(raw.as_bytes()))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn persistence_path() -> Option<PathBuf> {
    codexbar::settings::Settings::settings_path().and_then(|path| {
        path.parent()
            .map(|parent| parent.join("capacity-events.json"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{
        InactiveRateWindowSnapshot, NamedRateWindowSnapshot, ProviderUsageSnapshot,
    };

    fn window(used: f64, resets_at: DateTime<Utc>) -> RateWindowSnapshot {
        RateWindowSnapshot {
            used_percent: used,
            remaining_percent: 100.0 - used,
            window_minutes: Some(300),
            resets_at: Some(resets_at.to_rfc3339()),
            reset_description: None,
            is_exhausted: false,
            reserve_percent: None,
            reserve_description: None,
            reserve_will_last_to_reset: false,
            reserve_eta_seconds: None,
        }
    }

    fn snapshot(at: DateTime<Utc>, used: f64, reset: DateTime<Utc>) -> ProviderUsageSnapshot {
        ProviderUsageSnapshot {
            provider_id: "codex".into(),
            display_name: "Codex".into(),
            primary: window(used, reset),
            primary_label: Some("Session".into()),
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
            account_email: Some("person@example.com".into()),
            source_label: "oauth".into(),
            updated_at: at.to_rfc3339(),
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

    fn with_extra(
        mut snapshot: ProviderUsageSnapshot,
        id: &str,
        title: &str,
        used: f64,
        reset: DateTime<Utc>,
    ) -> ProviderUsageSnapshot {
        snapshot.extra_rate_windows.push(NamedRateWindowSnapshot {
            id: id.into(),
            title: title.into(),
            window: window(used, reset),
            amount: None,
        });
        snapshot
    }

    #[test]
    fn third_and_model_windows_are_observed_without_displacing_core_slots() {
        // Neither slot reached this map before, so their resets and exhaustions
        // were invisible to events. Both share a cadence with a core window in
        // the wild (Claude's model pool is 7 days like its weekly), and the map
        // is keyed by id — a shared key silently drops one of the two.
        let now = Utc::now();
        let reset = now + Duration::hours(3);
        let mut snapshot = snapshot(now, 10.0, reset);
        snapshot.secondary = Some(RateWindowSnapshot {
            window_minutes: Some(10_080),
            ..window(20.0, reset)
        });
        snapshot.secondary_label = Some("Weekly".into());
        snapshot.model_specific = Some(RateWindowSnapshot {
            window_minutes: Some(10_080),
            ..window(96.0, reset)
        });
        snapshot.tertiary = Some(RateWindowSnapshot {
            window_minutes: Some(43_200),
            ..window(57.0, reset)
        });
        snapshot.tertiary_label = Some("Monthly".into());

        let (windows, extra_ids) = observed_windows(&snapshot);

        assert_eq!(windows["weekly"].used_percent, 20.0);
        assert_eq!(windows["model"].used_percent, 96.0);
        assert_eq!(windows["monthly"].used_percent, 57.0);
        assert_eq!(windows["monthly"].label, "Monthly");
        // Not "extras": an extra id appearing for the first time announces a
        // granted allowance, and these windows have been there all along.
        assert!(!extra_ids.contains("model"));
        assert!(!extra_ids.contains("monthly"));
    }

    #[test]
    fn a_third_window_sharing_a_cadence_falls_back_to_its_own_id() {
        let now = Utc::now();
        let reset = now + Duration::hours(3);
        let mut snapshot = snapshot(now, 10.0, reset);
        snapshot.secondary = Some(RateWindowSnapshot {
            window_minutes: Some(10_080),
            ..window(20.0, reset)
        });
        snapshot.secondary_label = Some("Weekly".into());
        snapshot.tertiary = Some(RateWindowSnapshot {
            window_minutes: Some(10_080),
            ..window(80.0, reset)
        });

        let (windows, _) = observed_windows(&snapshot);

        assert_eq!(windows["weekly"].used_percent, 20.0);
        assert_eq!(windows["tertiary"].used_percent, 80.0);
    }

    fn with_inactive(
        mut snapshot: ProviderUsageSnapshot,
        id: &str,
        title: &str,
    ) -> ProviderUsageSnapshot {
        snapshot
            .inactive_rate_windows
            .push(InactiveRateWindowSnapshot {
                id: id.into(),
                title: title.into(),
                description: "Not currently limited".into(),
                state: "notEnforced".into(),
            });
        snapshot
    }

    fn with_reset_credits(
        mut snapshot: ProviderUsageSnapshot,
        available: u32,
    ) -> ProviderUsageSnapshot {
        snapshot.reset_credits_available = Some(available);
        snapshot
    }

    #[test]
    fn surprise_reset_requires_a_consistent_second_read() {
        let start = Utc::now();
        let old_reset = start + Duration::hours(4);
        let new_reset = start + Duration::hours(9);
        let mut observer = CapacityEventObserver::default();

        assert!(
            observer
                .observe(&snapshot(start, 85.0, old_reset))
                .is_empty()
        );
        assert!(
            observer
                .observe(&snapshot(start + Duration::minutes(5), 10.0, new_reset))
                .is_empty()
        );
        let events = observer.observe(&snapshot(
            start + Duration::minutes(10),
            12.0,
            new_reset + Duration::minutes(2),
        ));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CapacityEventKind::SurpriseReset);
    }

    #[test]
    fn banked_reset_grant_requires_a_consistent_second_read() {
        let start = Utc::now();
        let reset = start + Duration::days(7);
        let mut observer = CapacityEventObserver::default();

        assert!(
            observer
                .observe(&with_reset_credits(snapshot(start, 45.0, reset), 0))
                .is_empty()
        );
        assert!(
            observer
                .observe(&with_reset_credits(
                    snapshot(start + Duration::minutes(5), 46.0, reset),
                    1,
                ))
                .is_empty()
        );
        let events = observer.observe(&with_reset_credits(
            snapshot(start + Duration::minutes(10), 47.0, reset),
            1,
        ));

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CapacityEventKind::BankedResetGranted);
        assert_eq!(events[0].previous_reset_credits, Some(0));
        assert_eq!(events[0].current_reset_credits, Some(1));
        assert_eq!(
            events[0].notification_title(),
            "Codex banked reset available"
        );
        assert_eq!(
            events[0].notification_body(),
            "1 banked reset is available now."
        );
    }

    #[test]
    fn startup_and_consumed_banked_resets_do_not_emit() {
        let start = Utc::now();
        let reset = start + Duration::days(7);
        let mut observer = CapacityEventObserver::default();

        observer.observe(&with_reset_credits(snapshot(start, 45.0, reset), 1));
        assert!(
            observer
                .observe(&with_reset_credits(
                    snapshot(start + Duration::minutes(5), 46.0, reset),
                    0,
                ))
                .is_empty()
        );
        assert!(
            observer
                .observe(&with_reset_credits(
                    snapshot(start + Duration::minutes(10), 47.0, reset),
                    0,
                ))
                .is_empty()
        );
    }

    #[test]
    fn rapid_repeat_reads_cannot_confirm_a_capacity_event() {
        let start = Utc::now();
        let old_reset = start + Duration::hours(4);
        let new_reset = start + Duration::hours(9);
        let mut observer = CapacityEventObserver::default();

        observer.observe(&snapshot(start, 85.0, old_reset));
        observer.observe(&snapshot(start + Duration::minutes(5), 10.0, new_reset));
        assert!(
            observer
                .observe(&snapshot(
                    start + Duration::minutes(5) + Duration::seconds(10),
                    11.0,
                    new_reset + Duration::minutes(1),
                ))
                .is_empty(),
            "back-to-back refreshes are not independent confirmation"
        );

        let events = observer.observe(&snapshot(
            start + Duration::minutes(5) + Duration::seconds(31),
            12.0,
            new_reset + Duration::minutes(2),
        ));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CapacityEventKind::SurpriseReset);
    }

    #[test]
    fn reset_time_jitter_and_small_usage_drops_do_not_emit() {
        let start = Utc::now();
        let reset = start + Duration::hours(4);
        let mut observer = CapacityEventObserver::default();

        observer.observe(&snapshot(start, 60.0, reset));
        assert!(
            observer
                .observe(&snapshot(
                    start + Duration::minutes(5),
                    50.0,
                    reset + Duration::minutes(5),
                ))
                .is_empty()
        );
    }

    #[test]
    fn partial_reset_with_unchanged_reset_time_requires_confirmation() {
        let start = Utc::now();
        let reset = start + Duration::days(22);
        let mut observer = CapacityEventObserver::default();

        observer.observe(&snapshot(start, 99.4, reset));
        assert!(
            observer
                .observe(&snapshot(start + Duration::minutes(5), 49.7, reset))
                .is_empty(),
            "a single provider read must not trigger a reset notification"
        );

        let events = observer.observe(&snapshot(
            start + Duration::minutes(10),
            49.7,
            reset + Duration::minutes(2),
        ));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CapacityEventKind::PartialReset);
        assert_eq!(events[0].previous_used_percent, 99.4);
        assert_eq!(events[0].current_used_percent, 49.7);
    }

    #[test]
    fn startup_at_a_partially_restored_value_does_not_replay_an_alert() {
        let start = Utc::now();
        let reset = start + Duration::days(22);
        let mut before_restart = CapacityEventObserver::default();
        before_restart.observe(&snapshot(start, 99.4, reset));

        let persisted = serde_json::to_string(&before_restart).unwrap();
        let mut after_restart: CapacityEventObserver = serde_json::from_str(&persisted).unwrap();
        assert!(
            after_restart
                .observe(&snapshot(start + Duration::minutes(5), 49.7, reset))
                .is_empty()
        );
        assert!(
            after_restart
                .observe(&snapshot(start + Duration::minutes(10), 49.7, reset))
                .is_empty()
        );
    }

    #[test]
    fn scheduled_reset_emits_on_the_first_post_reset_read() {
        let start = Utc::now();
        let old_reset = start + Duration::minutes(5);
        let new_reset = start + Duration::hours(5);
        let mut observer = CapacityEventObserver::default();

        observer.observe(&snapshot(start, 88.0, old_reset));
        let events = observer.observe(&snapshot(start + Duration::minutes(6), 3.0, new_reset));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CapacityEventKind::ScheduledReset);
    }

    #[test]
    fn scheduled_reset_is_not_replayed_while_another_window_awaits_confirmation() {
        let start = Utc::now();
        let old_reset = start + Duration::minutes(5);
        let new_reset = start + Duration::days(31);
        let mut observer = CapacityEventObserver::default();

        let mut before = snapshot(start, 52.4, old_reset);
        before.provider_id = "copilot".into();
        before.display_name = "Copilot".into();
        before.primary_label = Some("Premium".into());
        before.primary.window_minutes = None;
        observer.observe(&before);

        // GitHub's response can briefly add quota rows at the monthly boundary.
        // The new allowance needs confirmation, but the independently confirmed
        // Premium reset must still advance its own baseline immediately.
        let mut first_after = with_extra(
            snapshot(start + Duration::minutes(6), 0.0, new_reset),
            "completions",
            "Completions",
            0.0,
            new_reset,
        );
        first_after.provider_id = "copilot".into();
        first_after.display_name = "Copilot".into();
        first_after.primary_label = Some("Premium".into());
        first_after.primary.window_minutes = None;
        first_after.extra_rate_windows[0].window.window_minutes = None;

        let first = observer.observe(&first_after);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, CapacityEventKind::ScheduledReset);
        assert_eq!(first[0].window_id, "premium");

        let mut rapid_repeat = first_after.clone();
        rapid_repeat.updated_at =
            (start + Duration::minutes(6) + Duration::seconds(2)).to_rfc3339();
        assert!(
            observer.observe(&rapid_repeat).is_empty(),
            "a second boundary refresh must not replay the Premium reset"
        );

        let mut settled = snapshot(start + Duration::minutes(10), 0.0, new_reset);
        settled.provider_id = "copilot".into();
        settled.display_name = "Copilot".into();
        settled.primary_label = Some("Premium".into());
        settled.primary.window_minutes = None;
        assert!(
            observer.observe(&settled).is_empty(),
            "the next fixed refresh must remain quiet after quota churn settles"
        );
    }

    #[test]
    fn scheduled_reset_cycle_key_suppresses_a_replayed_stale_baseline() {
        let start = Utc::now();
        let old_reset = start + Duration::minutes(5);
        let new_reset = start + Duration::hours(5);
        let mut observer = CapacityEventObserver::default();

        observer.observe(&snapshot(start, 88.0, old_reset));
        let stale_baselines = observer.baselines.clone();
        let after = snapshot(start + Duration::minutes(6), 3.0, new_reset);
        assert_eq!(observer.observe(&after).len(), 1);

        // Simulate a future regression or external baseline restore. The cycle
        // key is a second line of defense around user-visible notifications.
        observer.baselines = stale_baselines;
        assert!(observer.observe(&after).is_empty());

        let next_boundary = new_reset;
        let next_after = snapshot(
            next_boundary + Duration::minutes(1),
            2.0,
            next_boundary + Duration::hours(5),
        );
        assert_eq!(observer.observe(&next_after).len(), 1);
        assert_eq!(
            observer.emitted_scheduled_resets.len(),
            1,
            "only the latest reset boundary should remain for this scope and window"
        );
    }

    #[test]
    fn codex_weekly_reset_is_not_hidden_by_spark_weekly() {
        let start = Utc::now();
        let old_reset = start + Duration::minutes(5);
        let new_reset = start + Duration::days(7);
        let spark_reset = start + Duration::days(6);
        let mut observer = CapacityEventObserver::default();

        let mut before = with_extra(
            snapshot(start, 85.0, old_reset),
            "codex-spark-weekly",
            "Codex Spark Weekly",
            0.0,
            spark_reset,
        );
        before.primary_label = Some("Weekly".into());
        before.primary.window_minutes = Some(10_080);
        before.extra_rate_windows[0].window.window_minutes = Some(10_080);
        assert!(observer.observe(&before).is_empty());

        let mut after = with_extra(
            snapshot(start + Duration::minutes(6), 0.0, new_reset),
            "codex-spark-weekly",
            "Codex Spark Weekly",
            0.0,
            spark_reset,
        );
        after.primary_label = Some("Weekly".into());
        after.primary.window_minutes = Some(10_080);
        after.extra_rate_windows[0].window.window_minutes = Some(10_080);

        let events = observer.observe(&after);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CapacityEventKind::ScheduledReset);
        assert_eq!(events[0].window_id, "weekly");
        assert_eq!(events[0].window_label, "Weekly");
    }

    /// Simulates a restart: a fresh observer inherits the persisted baselines but
    /// not `seen_scopes`, exactly as `load_default` leaves it.
    fn restarted_with(baselines: &CapacityEventObserver) -> CapacityEventObserver {
        CapacityEventObserver {
            baselines: baselines.baselines.clone(),
            ..CapacityEventObserver::default()
        }
    }

    #[test]
    fn a_reset_while_ceiling_was_closed_is_reported_rather_than_swallowed() {
        let evening = Utc::now() - Duration::hours(8);
        let reset_at = evening + Duration::hours(3);
        let mut observer = CapacityEventObserver::default();
        observer.observe(&snapshot(evening, 92.0, reset_at));

        // Ceiling is closed over the reset and reopened in the morning.
        let morning = evening + Duration::hours(8);
        let events = restarted_with(&observer).observe(&snapshot(
            morning,
            3.0,
            reset_at + Duration::hours(5),
        ));

        assert_eq!(events.len(), 1, "the overnight reset must be reported");
        assert!(events[0].while_away);
        // Reported for when it happened, not when it was noticed.
        assert_eq!(events[0].occurred_at, reset_at.to_rfc3339());
        assert!(
            events[0]
                .notification_body()
                .contains("while Ceiling was closed"),
            "body should not imply it just happened: {}",
            events[0].notification_body()
        );
    }

    #[test]
    fn an_away_drop_that_would_need_confirming_is_not_announced() {
        // A large drop with an unchanged reset time reads as a partial reset,
        // which the live path only emits after a second corroborating reading.
        // There is no such reading after a restart, so it must stay silent
        // rather than alerting on one possibly-anomalous number.
        let earlier = Utc::now() - Duration::hours(2);
        let reset_at = earlier + Duration::days(20);
        let mut observer = CapacityEventObserver::default();
        observer.observe(&snapshot(earlier, 99.0, reset_at));

        let events = restarted_with(&observer).observe(&snapshot(
            earlier + Duration::hours(2),
            40.0,
            reset_at,
        ));

        assert!(events.is_empty(), "got: {events:?}");
    }

    #[test]
    fn an_away_reset_older_than_a_day_is_not_replayed() {
        let long_ago = Utc::now() - Duration::hours(30);
        let reset_at = long_ago + Duration::hours(3);
        let mut observer = CapacityEventObserver::default();
        observer.observe(&snapshot(long_ago, 92.0, reset_at));

        // Reopening after a long absence should not announce stale history.
        let events = restarted_with(&observer).observe(&snapshot(
            Utc::now(),
            3.0,
            Utc::now() + Duration::hours(4),
        ));

        assert!(events.is_empty(), "got: {events:?}");
    }

    #[test]
    fn a_restart_without_a_reset_stays_quiet() {
        let earlier = Utc::now() - Duration::hours(2);
        let reset_at = earlier + Duration::hours(3);
        let mut observer = CapacityEventObserver::default();
        observer.observe(&snapshot(earlier, 40.0, reset_at));

        // Usage climbed while away; that is not an event.
        let events = restarted_with(&observer).observe(&snapshot(
            earlier + Duration::hours(2),
            55.0,
            reset_at,
        ));

        assert!(events.is_empty(), "got: {events:?}");
    }

    #[test]
    fn a_first_ever_launch_has_nothing_to_compare_and_stays_quiet() {
        let mut observer = CapacityEventObserver::default();

        let events = observer.observe(&snapshot(Utc::now(), 3.0, Utc::now() + Duration::hours(4)));

        assert!(events.is_empty());
    }

    #[test]
    fn away_events_are_only_emitted_once() {
        let evening = Utc::now() - Duration::hours(8);
        let reset_at = evening + Duration::hours(3);
        let mut observer = CapacityEventObserver::default();
        observer.observe(&snapshot(evening, 92.0, reset_at));

        let mut restarted = restarted_with(&observer);
        let morning = evening + Duration::hours(8);
        let first = restarted.observe(&snapshot(morning, 3.0, reset_at + Duration::hours(5)));
        // The next poll in the same session must not repeat it.
        let second = restarted.observe(&snapshot(
            morning + Duration::minutes(1),
            4.0,
            reset_at + Duration::hours(5),
        ));

        assert_eq!(first.len(), 1);
        assert!(second.is_empty(), "got: {second:?}");
    }

    #[test]
    fn observations_are_isolated_by_account_and_source() {
        let start = Utc::now();
        let reset = start + Duration::hours(4);
        let mut observer = CapacityEventObserver::default();
        observer.observe(&snapshot(start, 90.0, reset));

        let mut other = snapshot(
            start + Duration::minutes(5),
            5.0,
            reset + Duration::hours(5),
        );
        other.account_email = Some("other@example.com".into());
        assert!(observer.observe(&other).is_empty());
    }

    /// Two Codex directory seats can share a login email (personal + Team).
    /// After #379 they stamp distinct `account_id`s; usage_history, threshold
    /// toasts, and predictive pace already key on those. Capacity-event
    /// observation used to hash provider|source|email|org only, so both seats
    /// shared a baseline (SBS-1079).
    fn shared_email_directory_seats(
        at: DateTime<Utc>,
        used: f64,
        reset: DateTime<Utc>,
        account_id: &str,
    ) -> ProviderUsageSnapshot {
        let mut seat = snapshot(at, used, reset);
        seat.account_email = Some("Same@Example.com".into());
        seat.account_organization = Some("Team".into());
        seat.account_id = Some(account_id.into());
        seat
    }

    #[test]
    fn observation_scope_separates_directory_seats_that_share_an_email() {
        let now = Utc::now();
        let reset = now + Duration::hours(4);
        let personal = shared_email_directory_seats(now, 90.0, reset, "acct-personal");
        let work = shared_email_directory_seats(now, 10.0, reset, "acct-work");

        assert_ne!(observation_scope(&personal), observation_scope(&work));
        assert_eq!(
            observation_scope(&personal),
            observation_scope(&shared_email_directory_seats(
                now + Duration::minutes(1),
                91.0,
                reset,
                "acct-personal",
            ))
        );
    }

    #[test]
    fn shared_email_directory_seats_do_not_confirm_each_others_resets() {
        // Without account_id in the scope, the work seat's drop from the
        // personal baseline looks like a confirmed surprise reset.
        let start = Utc::now();
        let old_reset = start + Duration::hours(4);
        let new_reset = start + Duration::hours(9);
        let mut observer = CapacityEventObserver::default();

        assert!(
            observer
                .observe(&shared_email_directory_seats(
                    start,
                    85.0,
                    old_reset,
                    "acct-personal",
                ))
                .is_empty()
        );
        assert!(
            observer
                .observe(&shared_email_directory_seats(
                    start + Duration::minutes(5),
                    10.0,
                    new_reset,
                    "acct-work",
                ))
                .is_empty()
        );
        let events = observer.observe(&shared_email_directory_seats(
            start + Duration::minutes(10),
            12.0,
            new_reset + Duration::minutes(2),
            "acct-work",
        ));
        assert!(
            events.is_empty(),
            "shared-email seats must not share a capacity baseline: {events:?}"
        );
    }

    #[test]
    fn reset_time_shift_requires_confirmation() {
        let start = Utc::now();
        let reset = start + Duration::hours(4);
        let shifted = reset + Duration::hours(2);
        let mut observer = CapacityEventObserver::default();

        observer.observe(&snapshot(start, 40.0, reset));
        assert!(
            observer
                .observe(&snapshot(start + Duration::minutes(5), 42.0, shifted))
                .is_empty()
        );
        let events = observer.observe(&snapshot(
            start + Duration::minutes(10),
            43.0,
            shifted + Duration::minutes(2),
        ));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CapacityEventKind::ResetTimeShift);
    }

    #[test]
    fn lifted_window_requires_confirmation() {
        let start = Utc::now();
        let reset = start + Duration::hours(4);
        let mut observer = CapacityEventObserver::default();
        observer.observe(&with_extra(
            snapshot(start, 30.0, reset),
            "codex-weekly",
            "Weekly",
            70.0,
            reset,
        ));

        let lifted = with_inactive(
            snapshot(start + Duration::minutes(5), 31.0, reset),
            "codex-weekly",
            "Weekly",
        );
        assert!(observer.observe(&lifted).is_empty());
        let events = observer.observe(&with_inactive(
            snapshot(start + Duration::minutes(10), 32.0, reset),
            "codex-weekly",
            "Weekly",
        ));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, CapacityEventKind::WindowLifted);
    }

    #[test]
    fn restored_window_and_new_allowance_are_distinguished() {
        let start = Utc::now();
        let reset = start + Duration::hours(4);
        let mut restored_observer = CapacityEventObserver::default();
        restored_observer.observe(&with_inactive(
            snapshot(start, 30.0, reset),
            "codex-weekly",
            "Weekly",
        ));
        restored_observer.observe(&with_extra(
            snapshot(start + Duration::minutes(5), 31.0, reset),
            "codex-weekly",
            "Weekly",
            10.0,
            reset,
        ));
        let restored = restored_observer.observe(&with_extra(
            snapshot(start + Duration::minutes(10), 32.0, reset),
            "codex-weekly",
            "Weekly",
            11.0,
            reset + Duration::minutes(2),
        ));
        assert_eq!(restored[0].kind, CapacityEventKind::WindowRestored);

        let mut allowance_observer = CapacityEventObserver::default();
        allowance_observer.observe(&snapshot(start, 30.0, reset));
        allowance_observer.observe(&with_extra(
            snapshot(start + Duration::minutes(5), 31.0, reset),
            "bonus",
            "Bonus",
            5.0,
            reset,
        ));
        let allowance = allowance_observer.observe(&with_extra(
            snapshot(start + Duration::minutes(10), 32.0, reset),
            "bonus",
            "Bonus",
            6.0,
            reset + Duration::minutes(2),
        ));
        assert_eq!(allowance[0].kind, CapacityEventKind::AllowanceGranted);
    }

    #[test]
    fn cursor_plan_unavailable_is_not_a_lifted_window() {
        let start = Utc::now();
        let reset = start + Duration::hours(4);
        let mut observer = CapacityEventObserver::default();
        let mut baseline = snapshot(start, 30.0, reset);
        baseline.provider_id = "cursor".into();
        baseline.display_name = "Cursor".into();
        baseline.primary_label = Some("Plan".into());
        observer.observe(&baseline);

        let mut missing = snapshot(start + Duration::minutes(5), 0.0, reset);
        missing.provider_id = "cursor".into();
        missing.display_name = "Cursor".into();
        missing.primary_label = Some("Plan".into());
        missing
            .inactive_rate_windows
            .push(InactiveRateWindowSnapshot {
                id: "cursor-plan".into(),
                title: "Plan".into(),
                description: "No usage reported".into(),
                state: "unavailable".into(),
            });
        assert!(observer.observe(&missing).is_empty());

        let mut later = missing.clone();
        later.updated_at = (start + Duration::minutes(10)).to_rfc3339();
        let events = observer.observe(&later);
        assert!(
            events.is_empty(),
            "a missing Plan reading must not confirm a reset or lifted limit: {events:?}"
        );
    }

    #[test]
    fn cursor_promotional_and_on_demand_pools_never_emit() {
        let start = Utc::now();
        let reset = start + Duration::hours(4);
        let mut observer = CapacityEventObserver::default();
        let mut baseline = snapshot(start, 30.0, reset);
        baseline.provider_id = "cursor".into();
        baseline.display_name = "Cursor".into();
        observer.observe(&baseline);

        for (id, title) in [
            ("cursor-promotional", "Promotional"),
            ("cursor-on-demand", "On-demand"),
        ] {
            let mut first = with_extra(
                snapshot(start + Duration::minutes(5), 31.0, reset),
                id,
                title,
                0.0,
                reset,
            );
            first.provider_id = "cursor".into();
            first.display_name = "Cursor".into();
            assert!(observer.observe(&first).is_empty());

            let mut second = with_extra(
                snapshot(start + Duration::minutes(10), 32.0, reset),
                id,
                title,
                0.0,
                reset,
            );
            second.provider_id = "cursor".into();
            second.display_name = "Cursor".into();
            assert!(observer.observe(&second).is_empty());
        }
    }

    #[test]
    fn restart_rebaselines_without_replaying_persisted_history() {
        let start = Utc::now();
        let old_reset = start + Duration::hours(4);
        let new_reset = start + Duration::hours(9);
        let mut before_restart = CapacityEventObserver::default();

        before_restart.observe(&snapshot(start, 85.0, old_reset));
        // Leave a surprise-reset candidate awaiting confirmation.
        before_restart.observe(&snapshot(start + Duration::minutes(5), 10.0, new_reset));

        let persisted = serde_json::to_string(&before_restart).unwrap();
        assert!(!persisted.contains("pending_resets"));
        assert!(!persisted.contains("pending_transitions"));

        let mut after_restart: CapacityEventObserver = serde_json::from_str(&persisted).unwrap();
        assert!(
            after_restart
                .observe(&snapshot(
                    start + Duration::minutes(10),
                    11.0,
                    new_reset + Duration::minutes(1),
                ))
                .is_empty(),
            "the first live reading after restart replaces persisted history"
        );
        assert!(
            after_restart
                .observe(&snapshot(
                    start + Duration::minutes(15),
                    12.0,
                    new_reset + Duration::minutes(2),
                ))
                .is_empty(),
            "an old pre-restart candidate must never be confirmed later"
        );
    }
}
