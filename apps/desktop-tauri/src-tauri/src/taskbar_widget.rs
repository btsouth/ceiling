//! Experimental native Windows taskbar host.
//!
//! Unlike the Tauri FloatBar, this surface is a real child of Explorer's
//! `Shell_TrayWnd`.

use crate::floatbar::taskbar::{TaskbarLandmarks, TaskbarLayout};

const WATCHDOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Max provider tiles on the native taskbar strip. Keep this small enough that
/// a typical primary taskbar still has a verified empty gap; the selection and
/// order come from `float_bar_provider_ids` (or enabled providers in display order).
const MAX_TASKBAR_WIDGET_PROVIDERS: usize = 5;

/// Consecutive transient-landmark misses before the Settings row reports
/// `WaitingLandmarks`. The watchdog runs every `WATCHDOG_INTERVAL`, so this
/// is roughly 30s of persistent misses — long enough to ride out Explorer
/// surfaces (Start, Search, Widgets) opening and closing, short enough to
/// still feel responsive.
const TRANSIENT_LANDMARKS_DEBOUNCE: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChildPlacement {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlacementOutcome {
    Place(ChildPlacement),
    TransientLandmarks,
    VerifiedNoFit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderReadout {
    provider_id: String,
    percent: Option<u8>,
    /// Set when the constraining lane is billed in currency. Takes the tile's
    /// headline slot ahead of `percent` — see [`strip_amount_label`].
    amount_label: Option<String>,
    /// Cents-free fallback for tiles too narrow for `amount_label`.
    amount_label_compact: Option<String>,
    window_label: String,
    reset: Option<String>,
    /// Localized "Unavailable" / "Not currently enforced" for a placeholder
    /// window. Painted ahead of the em dash so the tile reads as a named state
    /// rather than as a fetch error (SBS-876).
    named_label: Option<String>,
    /// Tile-width spelling of `named_label` for strips too narrow for it.
    named_label_compact: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WidgetModel {
    providers: Vec<ProviderReadout>,
    dark_text: bool,
    open_on_hover: bool,
}

fn centered_content_x(item_left: i32, item_width: i32, content_width: i32) -> i32 {
    item_left.saturating_add(item_width.saturating_sub(content_width).max(0) / 2)
}

pub fn native_mode_enabled(settings: &codexbar::settings::Settings) -> bool {
    settings.taskbar_widget_enabled
}

/// Ordered provider ids for the native taskbar strip (and float-bar allowlist).
/// Empty `float_bar_provider_ids` means auto: enabled providers in the Providers
/// tab display order, capped at [`MAX_TASKBAR_WIDGET_PROVIDERS`].
fn taskbar_strip_provider_ids(settings: &codexbar::settings::Settings) -> Vec<String> {
    let preferred_ids = if settings.float_bar_provider_ids.is_empty() {
        settings.provider_display_order_names()
    } else {
        settings.float_bar_provider_ids.clone()
    };
    preferred_ids
        .into_iter()
        .filter(|provider_id| settings.enabled_providers.contains(provider_id))
        .take(MAX_TASKBAR_WIDGET_PROVIDERS)
        .collect()
}

fn native_mode_has_configured_provider(settings: &codexbar::settings::Settings) -> bool {
    !taskbar_strip_provider_ids(settings).is_empty()
}

/// The rate window that actually constrains a provider right now.
///
/// Mirrors `constrainingWindow` in `capacityPresentation.ts` (SOU-288):
/// - Default: exhausted/maxed outranks everything, then highest used %, then
///   soonest reset (Claude session vs weekly).
/// - Cursor: Auto/API are parallel pools. Prefer the hottest lane that still
///   has room so a maxed API bar does not hide Auto capacity on the strip.
#[derive(Debug, Clone, Copy)]
struct ConstrainingReadout<'a> {
    label: Option<&'a str>,
    window: &'a crate::commands::RateWindowSnapshot,
    /// Money behind this lane, when the provider bills it in currency. A
    /// percentage is the wrong readout for a spend lane: "62%" of an $1800 cap
    /// does not tell you that you owe $1112.92 (SBS-191).
    amount: Option<&'a crate::commands::WindowAmountBridge>,
    /// Set when the percent on this window is a placeholder, not a reading
    /// (SBS-876). The tile must not round that placeholder to 0 or 100.
    named_state: Option<&'a str>,
}

impl<'a> ConstrainingReadout<'a> {
    fn new(label: Option<&'a str>, window: &'a crate::commands::RateWindowSnapshot) -> Self {
        Self {
            label,
            window,
            amount: None,
            named_state: None,
        }
    }
}

/// Inactive-row ids that mark `primary` as a placeholder, not a reading.
///
/// Match by id only. Sweep: Cursor writes 0% primary plus an inactive row
/// (`cursor-plan` / `cursor-monthly`); Claude does the same for a missing
/// Session (`claude-session`, SBS-1040).
fn primary_named_state(snapshot: &crate::commands::ProviderUsageSnapshot) -> Option<&str> {
    let row = snapshot.inactive_rate_windows.iter().find(|row| {
        row.id == "cursor-plan" || row.id == "cursor-monthly" || row.id == "claude-session"
    })?;
    Some(if row.state == "unavailable" {
        "unavailable"
    } else {
        "notEnforced"
    })
}

/// Tile text for a window that reports a named state instead of a reading.
fn strip_named_label(
    readout: &ConstrainingReadout<'_>,
    lang: codexbar::settings::Language,
) -> Option<String> {
    let key = match readout.named_state? {
        "unavailable" => codexbar::locale::LocaleKey::WindowUnavailable,
        _ => codexbar::locale::LocaleKey::NotCurrentlyEnforced,
    };
    Some(codexbar::locale::get_text(lang, key))
}

/// Tile-width spelling of [`strip_named_label`].
///
/// Same reason [`compact_amount_label`] exists: five providers on a crowded
/// taskbar leave roughly 51px for the headline, and "Unavailable" needs about
/// 72px at the 14px tile font. Without a short form the ladder falls through to
/// the em dash — the glyph a fetch error paints — so a known state would read as
/// an unknown one exactly when the strip is busiest (SBS-876).
fn compact_named_label(
    readout: &ConstrainingReadout<'_>,
    lang: codexbar::settings::Language,
) -> Option<String> {
    let key = match readout.named_state? {
        "unavailable" => codexbar::locale::LocaleKey::StripStateUnavailable,
        _ => codexbar::locale::LocaleKey::StripStateNotEnforced,
    };
    Some(codexbar::locale::get_text(lang, key))
}

/// Inline reset for the tile, when the user asked for it and the window is a
/// real reading. A named-state window has no quota to run out, so its
/// billing-cycle date is not a countdown (SBS-876).
fn strip_reset_label(readout: &ConstrainingReadout<'_>, show_reset_inline: bool) -> Option<String> {
    if !show_reset_inline || readout.named_state.is_some() {
        return None;
    }
    crate::tray_bridge::tooltip_short_reset(
        readout.window.resets_at.as_deref(),
        readout.window.reset_description.as_deref(),
    )
}

fn strip_readout_percent(readout: &ConstrainingReadout<'_>, show_as_used: bool) -> Option<u8> {
    if readout.named_state.is_some() {
        return None;
    }
    let value = if show_as_used {
        readout.window.used_percent
    } else {
        readout.window.remaining_percent
    };
    Some(value.clamp(0.0, 100.0).round() as u8)
}

fn strip_heat(
    snapshot: &crate::commands::ProviderUsageSnapshot,
    preference: codexbar::settings::MetricPreference,
) -> f64 {
    // Account ranking runs before `widget_model` drops errored snapshots, so a
    // failed account must rank below a named state or it wins the tile and then
    // has no readout to paint - an em dash where "Unavailable" was available
    // from the other account (SBS-876).
    if snapshot.error.is_some() {
        return f64::NEG_INFINITY;
    }
    // Rank by the window the tile will actually show, so a pinned Glance
    // metric picks the account that is hottest on that window (#411).
    let readout = preferred_readout(snapshot, preference);
    if readout.named_state.is_some() {
        return -1.0;
    }
    readout.window.used_percent
}

/// Tile text for a currency-billed lane.
///
/// Uncapped spend has no remaining figure to show, so "show remaining" falls
/// back to spend-to-date — the only honest number when there is no denominator.
fn strip_amount_label(
    amount: &crate::commands::WindowAmountBridge,
    show_as_used: bool,
) -> Option<String> {
    let Some(limit) = amount.limit.filter(|_| !show_as_used) else {
        return Some(amount.formatted_used.clone());
    };
    let remaining = (limit - amount.used).max(0.0);
    Some(codexbar::core::WindowAmount::new(remaining, amount.currency_code.clone()).format_used())
}

/// Whole-dollar spelling of [`strip_amount_label`], for tiles too narrow for cents.
///
/// A provider gets 72-104px on the strip and the paint does not clip, so an
/// overlong label runs into the neighbouring tile rather than being cut off.
/// "$1112.92" needs roughly 68px against a 51px budget on a crowded taskbar;
/// "$1113" fits, and the exact figure is a hover away in the flyout.
fn compact_amount_label(
    amount: &crate::commands::WindowAmountBridge,
    show_as_used: bool,
) -> Option<String> {
    let value = match amount.limit.filter(|_| !show_as_used) {
        Some(limit) => (limit - amount.used).max(0.0),
        None => amount.used,
    };
    let rounded = value.round();
    Some(match amount.currency_code.to_uppercase().as_str() {
        "USD" => format!("${rounded:.0}"),
        "EUR" => format!("€{rounded:.0}"),
        "GBP" => format!("£{rounded:.0}"),
        code => format!("{rounded:.0} {code}"),
    })
}

fn is_blocking_window(window: &crate::commands::RateWindowSnapshot) -> bool {
    window.is_exhausted || window.used_percent >= 100.0
}

fn reset_at_rank(window: &crate::commands::RateWindowSnapshot) -> i64 {
    window
        .resets_at
        .as_deref()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(i64::MAX)
}

/// Whether `candidate` should replace `best` as the constraining window.
fn outranks_window(
    candidate: &crate::commands::RateWindowSnapshot,
    best: &crate::commands::RateWindowSnapshot,
) -> bool {
    let candidate_blocking = is_blocking_window(candidate);
    let best_blocking = is_blocking_window(best);
    if candidate_blocking != best_blocking {
        return candidate_blocking;
    }
    match candidate.used_percent.total_cmp(&best.used_percent) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => reset_at_rank(candidate) < reset_at_rank(best),
    }
}

/// Cursor Auto + API only (not Plan/Monthly blend).
fn cursor_actionable_windows(
    snapshot: &crate::commands::ProviderUsageSnapshot,
) -> Vec<ConstrainingReadout<'_>> {
    let mut out = Vec::new();
    if let Some(window) = snapshot.secondary.as_ref() {
        let label = snapshot
            .secondary_label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .unwrap_or("Auto");
        out.push(ConstrainingReadout::new(Some(label), window));
    }
    for extra in &snapshot.extra_rate_windows {
        if extra.id != "cursor-api" {
            continue;
        }
        let label = match extra.title.trim() {
            "" => "API",
            label => label,
        };
        out.push(ConstrainingReadout {
            label: Some(label),
            window: &extra.window,
            amount: extra.amount.as_ref(),
            named_state: None,
        });
    }
    out
}

fn cursor_on_demand_readout(
    snapshot: &crate::commands::ProviderUsageSnapshot,
) -> Option<ConstrainingReadout<'_>> {
    snapshot
        .extra_rate_windows
        .iter()
        .find(|extra| extra.id == "cursor-on-demand")
        .map(|extra| ConstrainingReadout {
            label: Some(match extra.title.trim() {
                "" => "On-demand",
                label => label,
            }),
            window: &extra.window,
            amount: extra.amount.as_ref(),
            named_state: None,
        })
}

fn cursor_strip_readout(
    snapshot: &crate::commands::ProviderUsageSnapshot,
) -> ConstrainingReadout<'_> {
    let actionable = cursor_actionable_windows(snapshot);
    let on_demand = cursor_on_demand_readout(snapshot);
    let has_on_demand_spend = snapshot
        .extra_rate_windows
        .iter()
        .find(|extra| extra.id == "cursor-on-demand")
        .and_then(|extra| extra.amount.as_ref())
        .is_some_and(|amount| amount.used > 0.0);
    if has_on_demand_spend && let Some(readout) = on_demand {
        return readout;
    }
    if !actionable.is_empty() {
        // Hottest non-exhausted Auto/API lane.
        let mut with_room: Option<ConstrainingReadout<'_>> = None;
        for candidate in &actionable {
            if is_blocking_window(candidate.window) {
                continue;
            }
            let candidate = *candidate;
            let replace = match with_room {
                None => true,
                Some(best) => {
                    candidate.window.used_percent > best.window.used_percent
                        || (candidate.window.used_percent == best.window.used_percent
                            && reset_at_rank(candidate.window) < reset_at_rank(best.window))
                }
            };
            if replace {
                with_room = Some(candidate);
            }
        }
        if let Some(best) = with_room {
            return best;
        }
        if let Some(readout) = on_demand {
            return readout;
        }
        // All actionable lanes exhausted: soonest reset.
        let mut exhausted: Option<ConstrainingReadout<'_>> = None;
        for candidate in &actionable {
            if !is_blocking_window(candidate.window) {
                continue;
            }
            let candidate = *candidate;
            let replace = match exhausted {
                None => true,
                Some(best) => {
                    reset_at_rank(candidate.window) < reset_at_rank(best.window)
                        || (reset_at_rank(candidate.window) == reset_at_rank(best.window)
                            && candidate.window.used_percent > best.window.used_percent)
                }
            };
            if replace {
                exhausted = Some(candidate);
            }
        }
        if let Some(best) = exhausted {
            return best;
        }
    }
    if is_blocking_window(&snapshot.primary)
        && let Some(readout) = on_demand
    {
        return readout;
    }
    ConstrainingReadout {
        label: snapshot.primary_label.as_deref(),
        window: &snapshot.primary,
        amount: None,
        named_state: primary_named_state(snapshot),
    }
}

fn constraining_readout(
    snapshot: &crate::commands::ProviderUsageSnapshot,
) -> ConstrainingReadout<'_> {
    if snapshot.provider_id == "cursor" {
        return cursor_strip_readout(snapshot);
    }
    let mut best = ConstrainingReadout {
        label: snapshot.primary_label.as_deref(),
        window: &snapshot.primary,
        amount: None,
        named_state: primary_named_state(snapshot),
    };

    // Claude's per-model weekly caps are parallel sub-pools, not blockers: at
    // 100% you switch model rather than stop. The tile shows one lane, so
    // letting them compete meant a maxed "Fable only" (or the seven-day Opus
    // cap, which arrives in `model_specific`) took the whole Claude tile and
    // hid a Session and Weekly that still had room. Claude-only — other
    // providers put real pools in `model_specific`.
    let is_claude = snapshot.provider_id == "claude";

    let candidates: Vec<ConstrainingReadout<'_>> = {
        let mut out = Vec::new();
        if let Some(window) = snapshot.secondary.as_ref() {
            out.push(ConstrainingReadout::new(
                snapshot.secondary_label.as_deref(),
                window,
            ));
        }
        if let Some(window) = snapshot.model_specific.as_ref()
            && !is_claude
        {
            out.push(ConstrainingReadout::new(Some("Model"), window));
        }
        if let Some(window) = snapshot.tertiary.as_ref() {
            out.push(ConstrainingReadout::new(
                snapshot.tertiary_label.as_deref().or(Some("Extra")),
                window,
            ));
        }
        for extra in &snapshot.extra_rate_windows {
            if extra.id == "reset-credits" {
                continue;
            }
            if is_claude && extra.id.starts_with("claude-weekly-scoped-") {
                continue;
            }
            out.push(ConstrainingReadout {
                label: Some(extra.title.as_str()),
                window: &extra.window,
                amount: extra.amount.as_ref(),
                named_state: None,
            });
        }
        out
    };

    for candidate in candidates {
        if outranks_window(candidate.window, best.window) {
            best = candidate;
        }
    }

    best
}

/// Readout honoring the provider's Glance metric preference (#411).
///
/// `Automatic` keeps the constraining-window ranking above. A pinned
/// preference shows that window when the provider reports it and falls back to
/// the automatic choice when it does not, so a missing lane never blanks a
/// tile. ExtraUsage / Average / Credits have no single window a percentage
/// tile can show, so they keep the automatic ranking too.
fn preferred_readout<'a>(
    snapshot: &'a crate::commands::ProviderUsageSnapshot,
    preference: codexbar::settings::MetricPreference,
) -> ConstrainingReadout<'a> {
    match preference {
        codexbar::settings::MetricPreference::Session => ConstrainingReadout {
            label: snapshot.primary_label.as_deref(),
            window: &snapshot.primary,
            amount: None,
            named_state: primary_named_state(snapshot),
        },
        codexbar::settings::MetricPreference::Weekly => snapshot
            .secondary
            .as_ref()
            .map(|window| ConstrainingReadout::new(snapshot.secondary_label.as_deref(), window))
            .unwrap_or_else(|| constraining_readout(snapshot)),
        codexbar::settings::MetricPreference::Model => snapshot
            .model_specific
            .as_ref()
            .map(|window| ConstrainingReadout::new(Some("Model"), window))
            .unwrap_or_else(|| constraining_readout(snapshot)),
        codexbar::settings::MetricPreference::Tertiary => snapshot
            .tertiary
            .as_ref()
            .map(|window| {
                ConstrainingReadout::new(
                    snapshot.tertiary_label.as_deref().or(Some("Extra")),
                    window,
                )
            })
            .unwrap_or_else(|| constraining_readout(snapshot)),
        _ => constraining_readout(snapshot),
    }
}

/// Pick the reading to show on a one-tile-per-provider strip.
///
/// When `preferred_account_id` is set and that account is in the cache, use it.
/// Otherwise pick the account closest to its constraining limit (stable across
/// fetch order).
fn select_strip_snapshot<'a, I>(
    cache: I,
    provider_id: &str,
    preferred_account_id: Option<&str>,
    preference: codexbar::settings::MetricPreference,
) -> Option<&'a crate::commands::ProviderUsageSnapshot>
where
    I: IntoIterator<Item = &'a crate::commands::ProviderUsageSnapshot>,
{
    let candidates: Vec<_> = cache
        .into_iter()
        .filter(|snapshot| snapshot.provider_id == provider_id)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    if let Some(want) = preferred_account_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
        && let Some(hit) = candidates
            .iter()
            .find(|snapshot| snapshot.account_id.as_deref() == Some(want))
    {
        return Some(*hit);
    }
    candidates.into_iter().max_by(|a, b| {
        strip_heat(a, preference)
            .total_cmp(&strip_heat(b, preference))
            .then_with(|| b.account_id.cmp(&a.account_id))
    })
}

fn layout_is_enabled(layout: &TaskbarLayout, all_monitors: bool) -> bool {
    all_monitors || layout.primary
}

fn taskbar_remains_selected(
    existing_taskbar: isize,
    prepared_taskbars: &[isize],
    all_monitors: bool,
) -> bool {
    all_monitors || prepared_taskbars.contains(&existing_taskbar)
}

fn taskbar_placements(
    layouts: &[TaskbarLayout],
    all_monitors: bool,
    provider_count: usize,
) -> (Vec<isize>, Vec<isize>, Vec<(isize, ChildPlacement)>) {
    let mut discovered = Vec::new();
    let mut rejected = Vec::new();
    let mut placements = Vec::new();
    for layout in layouts
        .iter()
        .filter(|layout| layout_is_enabled(layout, all_monitors))
        .filter(|layout| layout.window_handle != 0)
    {
        discovered.push(layout.window_handle);
        match placement_outcome(layout, layout.landmarks, provider_count) {
            PlacementOutcome::Place(placement) => {
                placements.push((layout.window_handle, placement));
            }
            PlacementOutcome::VerifiedNoFit => rejected.push(layout.window_handle),
            PlacementOutcome::TransientLandmarks => {}
        }
    }
    (discovered, rejected, placements)
}

#[cfg(test)]
fn child_placement(
    layout: &TaskbarLayout,
    landmarks: TaskbarLandmarks,
    provider_count: usize,
) -> Option<ChildPlacement> {
    match placement_outcome(layout, landmarks, provider_count) {
        PlacementOutcome::Place(placement) => Some(placement),
        PlacementOutcome::TransientLandmarks | PlacementOutcome::VerifiedNoFit => None,
    }
}

/// Largest fully-empty sub-gap of `[lane_left, lane_right]` that is at
/// least `minimum_width` wide, after removing `obstacles` that fall inside
/// the lane. `obstacles` is expected to already be filtered to the taskbar
/// band (top/bottom) by the caller, since that filter doesn't depend on the
/// lane. Shared by `placement_outcome`'s two lanes so both use the exact
/// same gap-scan policy.
fn best_gap(
    lane_left: i32,
    lane_right: i32,
    obstacles: &[crate::floatbar::placement::Rect],
    minimum_width: i32,
) -> Option<(i32, i32)> {
    let mut obstacles = obstacles
        .iter()
        .copied()
        .filter(|rect| rect.right > lane_left && rect.left < lane_right)
        .collect::<Vec<_>>();
    obstacles.sort_by_key(|rect| (rect.left, rect.right));

    let mut gap_left = lane_left;
    let mut gaps = Vec::new();
    for obstacle in obstacles {
        let obstacle_left = obstacle.left.max(lane_left);
        if obstacle_left.saturating_sub(gap_left) >= minimum_width {
            gaps.push((gap_left, obstacle_left));
        }
        gap_left = gap_left.max(obstacle.right.saturating_add(8));
    }
    if lane_right.saturating_sub(gap_left) >= minimum_width {
        gaps.push((gap_left, lane_right));
    }
    gaps.into_iter()
        .max_by_key(|(left, right)| right.saturating_sub(*left))
}

fn placement_outcome(
    layout: &TaskbarLayout,
    landmarks: TaskbarLandmarks,
    provider_count: usize,
) -> PlacementOutcome {
    if layout.bounds.width() < layout.bounds.height() || provider_count == 0 {
        return PlacementOutcome::VerifiedNoFit;
    }

    let Some(start) = landmarks.start else {
        return PlacementOutcome::TransientLandmarks;
    };
    let bounds = layout.bounds;
    let overlaps_taskbar_band = |rect: crate::floatbar::placement::Rect| {
        rect.left >= bounds.left
            && rect.right <= bounds.right
            && rect.top < bounds.bottom
            && rect.bottom > bounds.top
    };
    if !overlaps_taskbar_band(start) {
        return PlacementOutcome::TransientLandmarks;
    }

    // Lane 1's left boundary. `None` means lane 1 doesn't exist: with
    // "Taskbar alignment = Left" and Windows Widgets enabled, Windows
    // renders the Widgets entry by the tray, so UIA reports it at or right
    // of Start permanently — that is real geometry, not a mid-animation
    // state, and treating it as transient froze a stale widget in place
    // while never consulting lane 2.
    let lane1_left = if let Some(widgets) = landmarks.widgets {
        if !overlaps_taskbar_band(widgets) {
            return PlacementOutcome::TransientLandmarks;
        }
        if widgets.right >= start.left {
            None
        } else {
            Some(widgets.right.saturating_add(8))
        }
    } else {
        Some(bounds.left.saturating_add(8))
    };
    let lane1_right = start.left.saturating_sub(8);
    let Ok(provider_count) = i32::try_from(provider_count) else {
        return PlacementOutcome::VerifiedNoFit;
    };
    // Leave enough room for labels such as "Weekly · 5d 21h" while keeping
    // both lines centered on the same axis. Squeezing 3 providers into 276px
    // put the final reset digits against the divider.
    let desired_width = provider_count.saturating_mul(104);
    let minimum_width = provider_count.saturating_mul(72);

    // UI Automation can expose Search, Task View, or pinned-app buttons in
    // either lane. Never cover one: use only a fully empty sub-gap and hide
    // the widget if no verified gap can fit in either lane.
    let mut band_obstacles = layout
        .obstacles
        .iter()
        .copied()
        .filter(|rect| rect.top < bounds.bottom && rect.bottom > bounds.top)
        .collect::<Vec<_>>();
    if lane1_left.is_none()
        && let Some(widgets) = landmarks.widgets
    {
        // The Widgets entry sits in lane 2's territory when it isn't a
        // lane-1 boundary — it's a real control there; never cover it.
        band_obstacles.push(widgets);
    }

    // Lane 1 (Widgets→Start), exactly today's policy, always preferred.
    let gap = match lane1_left
        .and_then(|lane1_left| best_gap(lane1_left, lane1_right, &band_obstacles, minimum_width))
    {
        Some(gap) => Some(gap),
        None => {
            // No qualifying gap in lane 1 — e.g. Start pinned at the
            // taskbar's left edge (stock "Taskbar alignment = Left", or
            // Windhawk's "Start button always on left"). Try a second lane
            // between Start and the tray, with the same obstacle
            // verification.
            let lane2_left = start.right.saturating_add(8);
            let lane2_right = match landmarks.tray {
                // A tray rect off the taskbar band is stale (mid layout or
                // DPI change) — wait for a clean pass instead of letting a
                // garbage rect widen the lane over the tray's true
                // position. Same policy as an off-band Start or Widgets.
                Some(tray) if !overlaps_taskbar_band(tray) => {
                    return PlacementOutcome::TransientLandmarks;
                }
                Some(tray) => tray.left.saturating_sub(8),
                // Secondary taskbars can omit TrayNotifyWnd.
                None => bounds.right.saturating_sub(8),
            };
            if lane2_right <= lane2_left {
                None
            } else {
                best_gap(lane2_left, lane2_right, &band_obstacles, minimum_width)
            }
        }
    };
    let Some((gap_left, gap_right)) = gap else {
        return PlacementOutcome::VerifiedNoFit;
    };
    let available_width = gap_right.saturating_sub(gap_left);

    let taskbar_height = layout.bounds.height();
    let width = desired_width.min(available_width);

    PlacementOutcome::Place(ChildPlacement {
        x: gap_left
            .saturating_add(available_width.saturating_sub(width) / 2)
            .saturating_sub(layout.bounds.left),
        y: 0,
        width,
        height: taskbar_height,
    })
}

/// Reported native taskbar widget visibility, mirrored to the Settings UI so
/// a hidden widget is never silent. Declared outside `windows_host` so
/// `get_taskbar_widget_status` compiles (and returns `Unavailable`) on
/// non-Windows targets.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TaskbarWidgetStatus {
    /// Not Windows; the native widget does not exist on this platform.
    #[cfg_attr(windows, allow(dead_code))]
    Unavailable,
    /// Native taskbar mode is turned off in Settings.
    Disabled,
    NoProviders,
    WaitingLandmarks,
    NoFit,
    Active {
        taskbars: usize,
    },
}

/// Why [`prepare_widgets`] could not produce placements this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrepareFailure {
    Disabled,
    NoProviders,
    /// No taskbar produced a placement, but none was conclusively rejected
    /// either — Start/Widgets/Search were transiently unavailable to UI
    /// Automation. Distinct from [`PlacementOutcome::VerifiedNoFit`], which
    /// is conclusive.
    TransientLandmarks,
}

impl std::fmt::Display for PrepareFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PrepareFailure::Disabled => "Native taskbar mode is disabled",
            PrepareFailure::NoProviders => {
                "No enabled providers are available for the taskbar widget"
            }
            PrepareFailure::TransientLandmarks => {
                "No verified taskbar lane can fit the native widget"
            }
        })
    }
}

struct PreparedWidget {
    taskbar: isize,
    placement: ChildPlacement,
}

struct PreparedWidgets {
    widgets: Vec<PreparedWidget>,
    rejected_taskbars: Vec<isize>,
    all_monitors: bool,
    model: WidgetModel,
}

/// Pure mapping from a preparation attempt to the status it implies, absent
/// debounce. `None` for [`PrepareFailure::TransientLandmarks`]: the caller
/// must consult the debounce counter (see [`should_report_waiting_landmarks`])
/// to decide whether to keep the previous status or surface
/// `WaitingLandmarks`.
fn status_from_preparation(
    prepared: &Result<PreparedWidgets, PrepareFailure>,
) -> Option<TaskbarWidgetStatus> {
    match prepared {
        Ok(prepared) if !prepared.widgets.is_empty() => Some(TaskbarWidgetStatus::Active {
            taskbars: prepared.widgets.len(),
        }),
        Ok(_) => Some(TaskbarWidgetStatus::NoFit),
        Err(PrepareFailure::Disabled) => Some(TaskbarWidgetStatus::Disabled),
        Err(PrepareFailure::NoProviders) => Some(TaskbarWidgetStatus::NoProviders),
        Err(PrepareFailure::TransientLandmarks) => None,
    }
}

/// Whether `streak` consecutive transient-landmark misses should flip the
/// status to `WaitingLandmarks`. Pure so the debounce threshold is testable
/// without touching the watchdog's global counter.
fn should_report_waiting_landmarks(streak: u32) -> bool {
    streak >= TRANSIENT_LANDMARKS_DEBOUNCE
}

/// Status to report while `apply_state` keeps the widget hidden. Native mode
/// on without a configured provider must surface as `NoProviders`: the
/// Settings row hides `Disabled`, which would leave that misconfiguration
/// silent — the exact failure mode this status exists to prevent.
#[cfg_attr(not(windows), allow(dead_code))]
fn status_when_hidden(native_mode_enabled: bool) -> TaskbarWidgetStatus {
    if native_mode_enabled {
        TaskbarWidgetStatus::NoProviders
    } else {
        TaskbarWidgetStatus::Disabled
    }
}

pub fn install(app: &tauri::AppHandle) {
    #[cfg(windows)]
    windows_host::install(app);
    #[cfg(not(windows))]
    let _ = app;
}

pub fn apply_state(app: &tauri::AppHandle, settings: &codexbar::settings::Settings) {
    #[cfg(windows)]
    windows_host::apply_state(app, settings);
    #[cfg(not(windows))]
    let _ = (app, settings);
}

/// Return a sampled RGB color from Explorer's current taskbar material. The
/// native flyout uses this as its base tint so custom Windows accent colors do
/// not leave Ceiling looking like a separate, bolted-on surface.
#[tauri::command]
pub fn get_taskbar_surface_color() -> Option<String> {
    #[cfg(windows)]
    return windows_host::taskbar_surface_color();
    #[cfg(not(windows))]
    None
}

/// Current native taskbar widget visibility, mirrored from the same
/// watchdog pass that shows or hides the strip, so the Settings row and the
/// strip never disagree.
#[tauri::command]
pub fn get_taskbar_widget_status() -> TaskbarWidgetStatus {
    #[cfg(windows)]
    return windows_host::current_status();
    #[cfg(not(windows))]
    TaskbarWidgetStatus::Unavailable
}

#[cfg(windows)]
mod windows_host {
    use super::*;
    use std::sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    };
    use tauri::Manager;

    const CLASS_NAME: &str = "CeilingNativeTaskbarWidget";
    const WINDOW_TITLE: &str = "Ceiling taskbar widget";

    const WS_VISIBLE: u32 = 0x1000_0000;
    const WS_POPUP: u32 = 0x8000_0000;
    const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
    const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    const WS_EX_LAYERED: u32 = 0x0008_0000;
    const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
    const LWA_COLORKEY: u32 = 0x0000_0001;
    const LWA_ALPHA: u32 = 0x0000_0002;
    const SW_HIDE: i32 = 0;
    const SW_SHOWNA: i32 = 8;
    const SWP_NOACTIVATE: u32 = 0x0010;
    const SWP_NOOWNERZORDER: u32 = 0x0200;

    const WM_DESTROY: u32 = 0x0002;
    const WM_PAINT: u32 = 0x000F;
    const WM_ERASEBKGND: u32 = 0x0014;
    const WM_SETCURSOR: u32 = 0x0020;
    const WM_MOUSEACTIVATE: u32 = 0x0021;
    const WM_TIMER: u32 = 0x0113;
    const WM_MOUSEMOVE: u32 = 0x0200;
    const WM_LBUTTONUP: u32 = 0x0202;
    const WM_MOUSELEAVE: u32 = 0x02A3;
    const MA_NOACTIVATE: isize = 3;
    const IDC_ARROW: usize = 32512;
    const TME_LEAVE: u32 = 0x0000_0002;
    const HOVER_TIMER_ID: usize = 0xCE11;
    const HOVER_DWELL_MS: u32 = 150;
    const HOVER_DISMISS_GRACE: std::time::Duration = std::time::Duration::from_millis(180);
    const HOVER_POINTER_POLL: std::time::Duration = std::time::Duration::from_millis(50);
    const TRANSPARENT: i32 = 1;
    const PS_SOLID: i32 = 0;
    const FONT_QUALITY_ANTIALIASED: u32 = 4;
    // A deliberately uncommon key color. Pixels left in this color are
    // transparent, allowing Explorer's own taskbar material to show through.
    const TRANSPARENT_KEY: u32 = rgb(1, 2, 3);

    #[derive(Debug, Default)]
    struct HostedWidget {
        hwnd: isize,
        taskbar: isize,
    }

    #[derive(Debug, Default)]
    struct HostState {
        widgets: Vec<HostedWidget>,
        model: WidgetModel,
    }

    static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
    static HOST: OnceLock<Mutex<HostState>> = OnceLock::new();
    static CLASS_REGISTERED: OnceLock<bool> = OnceLock::new();
    static RECOVERY_PENDING: AtomicBool = AtomicBool::new(false);
    static HOVER_TRACKING: AtomicBool = AtomicBool::new(false);
    static HOVER_FLYOUT_OPEN: AtomicBool = AtomicBool::new(false);
    static STATUS: Mutex<TaskbarWidgetStatus> = Mutex::new(TaskbarWidgetStatus::Disabled);
    static TRANSIENT_STREAK: AtomicU32 = AtomicU32::new(0);

    pub(super) fn install(app: &tauri::AppHandle) {
        let _ = APP.set(app.clone());
        schedule_recovery(app);

        let refresh_app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = crate::commands::do_refresh_providers_if_stale(&refresh_app).await {
                tracing::warn!(%error, "Initial taskbar provider refresh failed");
            }
            schedule_recovery(&refresh_app);
        });

        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(WATCHDOG_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                schedule_recovery(&app);
            }
        });
    }

    pub(super) fn apply_state(app: &tauri::AppHandle, settings: &codexbar::settings::Settings) {
        if native_mode_enabled(settings) && native_mode_has_configured_provider(settings) {
            schedule_recovery(app);
        } else {
            hide_existing();
            TRANSIENT_STREAK.store(0, Ordering::Release);
            set_status(app, status_when_hidden(native_mode_enabled(settings)));
        }
    }

    pub(super) fn current_status() -> TaskbarWidgetStatus {
        // A poisoned lock still holds the last stored status. Recover it: a
        // possibly-stale reading beats reporting `Disabled`, which would hide
        // the status row — the silent failure this feature exists to prevent.
        STATUS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Persist the widget status and emit `taskbar-widget-status-changed`
    /// only when it actually changes, so Settings doesn't re-fetch on every
    /// watchdog tick.
    fn set_status(app: &tauri::AppHandle, status: TaskbarWidgetStatus) {
        let changed = {
            let mut guard = STATUS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *guard != status {
                *guard = status;
                true
            } else {
                false
            }
        };
        if changed {
            crate::events::emit_taskbar_widget_status_changed(app);
        }
    }

    #[cfg(test)]
    mod status_lock_tests {
        use super::*;

        #[test]
        fn current_status_survives_a_poisoned_lock() {
            *STATUS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                TaskbarWidgetStatus::Active { taskbars: 2 };
            let poisoner = std::thread::spawn(|| {
                let _guard = STATUS.lock();
                panic!("poison the status lock");
            });
            assert!(poisoner.join().is_err());
            assert_eq!(
                current_status(),
                TaskbarWidgetStatus::Active { taskbars: 2 }
            );
        }
    }

    pub(super) fn taskbar_surface_color() -> Option<String> {
        let taskbar = unsafe { find_primary_taskbar()? };
        let mut rect = WinRect::default();
        if unsafe { GetWindowRect(taskbar, (&mut rect as *mut WinRect).cast()) } == 0 {
            return None;
        }
        let dc = unsafe { GetDC(0) };
        if dc == 0 {
            return None;
        }

        // The upper edge is normally free of buttons, hover states, and text.
        // Sample several points and take the median per channel to reject an
        // occasional icon/accent pixel without needing screen capture APIs.
        let width = rect.right.saturating_sub(rect.left).max(1);
        let y = rect.top.saturating_add(3);
        let mut reds = Vec::new();
        let mut greens = Vec::new();
        let mut blues = Vec::new();
        for fraction in [1, 2, 3, 4, 5] {
            let x = rect.left.saturating_add(width.saturating_mul(fraction) / 6);
            let color = unsafe { GetPixel(dc, x, y) };
            if color == u32::MAX {
                continue;
            }
            reds.push((color & 0xff) as u8);
            greens.push(((color >> 8) & 0xff) as u8);
            blues.push(((color >> 16) & 0xff) as u8);
        }
        unsafe { ReleaseDC(0, dc) };
        if reds.is_empty() {
            return None;
        }
        reds.sort_unstable();
        greens.sort_unstable();
        blues.sort_unstable();
        let middle = reds.len() / 2;
        Some(format!(
            "#{:02x}{:02x}{:02x}",
            reds[middle], greens[middle], blues[middle]
        ))
    }

    fn schedule_recovery(app: &tauri::AppHandle) {
        if RECOVERY_PENDING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let prepared = tauri::async_runtime::spawn_blocking(prepare_widgets).await;
            let status_app = app.clone();
            let dispatched = app.run_on_main_thread(move || {
                match prepared {
                    Ok(outcome) => {
                        let status = status_from_preparation(&outcome);
                        match outcome {
                            Ok(prepared) => {
                                TRANSIENT_STREAK.store(0, Ordering::Release);
                                if let Err(error) = apply_prepared(prepared) {
                                    tracing::warn!(%error, "Native taskbar widget proof update failed");
                                } else if let Some(status) = status {
                                    set_status(&status_app, status);
                                }
                            }
                            Err(error @ PrepareFailure::TransientLandmarks) => {
                                // Start, Search, Widgets, and other Explorer surfaces
                                // can temporarily make UI Automation landmarks
                                // unavailable. Keep the last known healthy child
                                // visible rather than turning a transient discovery
                                // miss into user-visible flicker and a slow
                                // rediscovery cycle; only report `WaitingLandmarks`
                                // after several consecutive misses so a single
                                // missed watchdog tick doesn't flash the Settings row.
                                let streak = TRANSIENT_STREAK.fetch_add(1, Ordering::AcqRel) + 1;
                                tracing::debug!(%error, streak, "Native taskbar widget recovery deferred; preserving the current widget");
                                if should_report_waiting_landmarks(streak) {
                                    set_status(&status_app, TaskbarWidgetStatus::WaitingLandmarks);
                                }
                            }
                            Err(error) => {
                                TRANSIENT_STREAK.store(0, Ordering::Release);
                                tracing::debug!(%error, "Native taskbar widget recovery deferred; preserving the current widget");
                                if let Some(status) = status {
                                    set_status(&status_app, status);
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "Native taskbar discovery worker failed; preserving the current widget");
                    }
                }
                RECOVERY_PENDING.store(false, Ordering::Release);
            });
            if dispatched.is_err() {
                RECOVERY_PENDING.store(false, Ordering::Release);
            }
        });
    }

    fn prepare_widgets() -> Result<PreparedWidgets, PrepareFailure> {
        let settings = codexbar::settings::Settings::load();
        if !native_mode_enabled(&settings) {
            return Err(PrepareFailure::Disabled);
        }
        let model = match widget_model() {
            Ok(model) => model,
            Err(error) => {
                // App-handle/state-poisoning failures are internal and rare;
                // fold them into the transient-landmarks bucket rather than
                // adding a fourth status the UI cannot act on differently.
                tracing::debug!(%error, "Native taskbar widget model unavailable this pass");
                return Err(PrepareFailure::TransientLandmarks);
            }
        };
        if model.providers.is_empty() {
            return Err(PrepareFailure::NoProviders);
        }
        let layouts = crate::floatbar::taskbar::discover_all();
        let (_, rejected_taskbars, placements) = taskbar_placements(
            &layouts,
            settings.taskbar_widget_all_monitors,
            model.providers.len(),
        );
        let widgets = placements
            .into_iter()
            .map(|(taskbar, placement)| PreparedWidget { taskbar, placement })
            .collect::<Vec<_>>();
        if widgets.is_empty() && rejected_taskbars.is_empty() {
            return Err(PrepareFailure::TransientLandmarks);
        }

        Ok(PreparedWidgets {
            widgets,
            rejected_taskbars,
            all_monitors: settings.taskbar_widget_all_monitors,
            model,
        })
    }

    fn apply_prepared(prepared: PreparedWidgets) -> Result<(), String> {
        let mut state = HOST
            .get_or_init(|| Mutex::new(HostState::default()))
            .lock()
            .map_err(|_| "Native taskbar widget state is poisoned".to_string())?;

        let model_changed = state.model != prepared.model;
        state.model = prepared.model;
        let prepared_taskbars = prepared
            .widgets
            .iter()
            .map(|candidate| candidate.taskbar)
            .collect::<Vec<_>>();
        state.widgets.retain(|widget| {
            // Start/Search can temporarily remove one monitor's taskbar from a
            // UI Automation discovery pass. In mirrored mode, preserve any
            // still-valid Explorer host instead of treating that partial pass
            // as a monitor removal. When mirroring is disabled, retain only the
            // primary taskbar selected by this successful preparation.
            let host_is_alive = widget.hwnd != 0
                && unsafe { IsWindow(widget.hwnd) } != 0
                && unsafe { IsWindow(widget.taskbar) } != 0
                && unsafe { GetParent(widget.hwnd) } == widget.taskbar;
            let selected =
                taskbar_remains_selected(widget.taskbar, &prepared_taskbars, prepared.all_monitors);
            let conclusively_rejected = prepared.rejected_taskbars.contains(&widget.taskbar);
            let keep = host_is_alive && selected && !conclusively_rejected;
            if !keep && widget.hwnd != 0 && unsafe { IsWindow(widget.hwnd) } != 0 {
                unsafe { DestroyWindow(widget.hwnd) };
            }
            keep
        });

        for prepared_widget in prepared.widgets {
            let index = state
                .widgets
                .iter()
                .position(|widget| widget.taskbar == prepared_widget.taskbar)
                .unwrap_or_else(|| {
                    state.widgets.push(HostedWidget {
                        hwnd: 0,
                        taskbar: prepared_widget.taskbar,
                    });
                    state.widgets.len() - 1
                });
            let widget = &mut state.widgets[index];
            let window_alive = widget.hwnd != 0 && unsafe { IsWindow(widget.hwnd) } != 0;
            let correctly_parented =
                window_alive && unsafe { GetParent(widget.hwnd) } == prepared_widget.taskbar;
            if !correctly_parented {
                if window_alive {
                    unsafe { DestroyWindow(widget.hwnd) };
                }
                widget.hwnd = unsafe { create_widget(prepared_widget.taskbar)? };
                tracing::info!("Created native Ceiling taskbar widget");
            }

            unsafe {
                SetWindowRgn(widget.hwnd, 0, 1);
                SetWindowPos(
                    widget.hwnd,
                    0,
                    prepared_widget.placement.x,
                    prepared_widget.placement.y,
                    prepared_widget.placement.width,
                    prepared_widget.placement.height,
                    SWP_NOACTIVATE | SWP_NOOWNERZORDER,
                );
                ShowWindow(widget.hwnd, SW_SHOWNA);
                if model_changed {
                    InvalidateRect(widget.hwnd, std::ptr::null(), 0);
                }
            }
        }
        Ok(())
    }

    fn widget_model() -> Result<WidgetModel, String> {
        let app = APP
            .get()
            .ok_or_else(|| "Native taskbar widget app handle is unavailable".to_string())?;
        let settings = codexbar::settings::Settings::load();
        let preferred_ids = taskbar_strip_provider_ids(&settings);
        let state = app.state::<Mutex<crate::state::AppState>>();
        let guard = state
            .lock()
            .map_err(|_| "Ceiling provider state is poisoned".to_string())?;

        let providers = preferred_ids
            .into_iter()
            .map(|provider_id| {
                // One tile per provider. Default picks the account closest to
                // its limit (stable across fetch order). Users can pin a
                // specific Codex/Claude account in Settings → Taskbar Usage.
                let preferred = settings.taskbar_account_for(&provider_id);
                // The Glance metric drives which window the tile shows, so the
                // account ranking uses the same window (#411).
                let preference = codexbar::core::ProviderId::from_cli_name(&provider_id)
                    .map(|id| settings.get_provider_metric(id))
                    .unwrap_or_default();
                let snapshot = super::select_strip_snapshot(
                    guard.provider_cache.iter(),
                    &provider_id,
                    preferred,
                    preference,
                );
                // One-number strip: surface the constraining window, not always
                // the primary session. Claude weekly at 100% with a fresh 5h
                // session must read as Weekly / 100%, not 5h / 0%. A pinned
                // Glance metric overrides the ranking (#411).
                let constraining = snapshot
                    .filter(|snapshot| snapshot.error.is_none())
                    .map(|snapshot| super::preferred_readout(snapshot, preference));
                let percent = constraining
                    .and_then(|readout| strip_readout_percent(&readout, settings.show_as_used));
                // A spend lane's headline is the money, not the fraction.
                let spend = constraining.and_then(|readout| readout.amount);
                let amount_label =
                    spend.and_then(|amount| strip_amount_label(amount, settings.show_as_used));
                let amount_label_compact =
                    spend.and_then(|amount| compact_amount_label(amount, settings.show_as_used));
                ProviderReadout {
                    provider_id,
                    percent,
                    amount_label,
                    amount_label_compact,
                    // Window label only (Weekly / 5h). Account identity lives in
                    // the flyout (On strip + account line); long tags collide
                    // with the next tile on the compact strip.
                    window_label: compact_window_label(
                        constraining.and_then(|readout| readout.label).or_else(|| {
                            snapshot.and_then(|snapshot| snapshot.primary_label.as_deref())
                        }),
                        constraining
                            .map(|readout| readout.window.window_minutes)
                            .unwrap_or_else(|| {
                                snapshot.and_then(|snapshot| snapshot.primary.window_minutes)
                            }),
                    ),
                    reset: constraining.and_then(|readout| {
                        strip_reset_label(&readout, settings.float_bar_show_reset_inline)
                    }),
                    named_label: constraining
                        .and_then(|readout| strip_named_label(&readout, settings.ui_language)),
                    named_label_compact: constraining
                        .and_then(|readout| compact_named_label(&readout, settings.ui_language)),
                }
            })
            .collect();

        // The taskbar surface follows Windows. Manual contrast is retained only
        // for the free-floating bar where the desktop background is unknown.
        let dark_text = system_uses_light_theme();

        Ok(WidgetModel {
            providers,
            dark_text,
            open_on_hover: settings.taskbar_widget_open_on_hover,
        })
    }

    fn system_uses_light_theme() -> bool {
        const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
        const RRF_RT_REG_DWORD: u32 = 0x0000_0018;
        let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let name = wide("SystemUsesLightTheme");
        let mut value = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut value as *mut u32).cast(),
                &mut size,
            ) == 0
                && value != 0
        }
    }

    fn compact_window_label(label: Option<&str>, window_minutes: Option<u32>) -> String {
        let label = label.map(str::trim).filter(|label| !label.is_empty());
        if let Some(label) = label {
            let normalized = label.to_ascii_lowercase();
            if normalized.contains("5-hour") || normalized.contains("5 hour") {
                return "5h".to_string();
            }
            if normalized.contains("weekly") || normalized == "week" {
                return "Weekly".to_string();
            }
            if normalized.contains("monthly") || normalized == "month" {
                return "Monthly".to_string();
            }
            if normalized.contains("session") && window_minutes == Some(300) {
                return "5h".to_string();
            }
            return label.chars().take(9).collect();
        }

        match window_minutes {
            Some(minutes) if minutes <= 360 => format!("{}h", (minutes / 60).max(1)),
            Some(minutes) if minutes <= 10_080 => "Weekly".to_string(),
            Some(minutes) if minutes >= 40_320 => "Monthly".to_string(),
            _ => "Usage".to_string(),
        }
    }

    fn hide_existing() {
        let Some(host) = HOST.get() else {
            return;
        };
        let Ok(state) = host.lock() else {
            tracing::warn!("Native taskbar widget state is poisoned while hiding");
            return;
        };
        for widget in &state.widgets {
            if widget.hwnd != 0 && unsafe { IsWindow(widget.hwnd) } != 0 {
                unsafe { ShowWindow(widget.hwnd, SW_HIDE) };
            }
        }
    }

    unsafe fn find_primary_taskbar() -> Option<isize> {
        let class = wide("Shell_TrayWnd");
        let hwnd = unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) };
        (hwnd != 0).then_some(hwnd)
    }

    unsafe fn create_widget(taskbar: isize) -> Result<isize, String> {
        if !*CLASS_REGISTERED.get_or_init(|| unsafe { register_class() }) {
            return Err("Could not register the native widget window class".to_string());
        }
        let class = wide(CLASS_NAME);
        let title = wide(WINDOW_TITLE);
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP | WS_VISIBLE | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                taskbar,
                0,
                instance,
                std::ptr::null(),
            )
        };
        if hwnd == 0 {
            return Err("CreateWindowExW failed for the taskbar widget".to_string());
        }
        // Keep the popup style when attaching to Explorer. This is the hosting
        // model used by the approved taskbar build: it remains non-activating
        // while avoiding the composed taskbar painting over an ordinary child
        // whenever Start, Search, or Widgets opens.
        let previous = unsafe { SetParent(hwnd, taskbar) };
        if previous == 0 && unsafe { GetParent(hwnd) } != taskbar {
            unsafe { DestroyWindow(hwnd) };
            return Err("Could not attach native widget to the taskbar".to_string());
        }
        if unsafe { GetParent(hwnd) } != taskbar {
            unsafe { DestroyWindow(hwnd) };
            return Err("Could not attach native widget to the taskbar".to_string());
        }
        if unsafe {
            SetLayeredWindowAttributes(hwnd, TRANSPARENT_KEY, 255, LWA_COLORKEY | LWA_ALPHA)
        } == 0
        {
            unsafe { DestroyWindow(hwnd) };
            return Err("Could not enable native widget composition".to_string());
        }
        Ok(hwnd)
    }

    unsafe fn register_class() -> bool {
        let class = wide(CLASS_NAME);
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let wc = WndClassExW {
            size: std::mem::size_of::<WndClassExW>() as u32,
            style: 0,
            window_proc: Some(widget_window_proc),
            class_extra: 0,
            window_extra: 0,
            instance,
            icon: 0,
            cursor: unsafe { LoadCursorW(0, IDC_ARROW as *const u16) },
            background: 0,
            menu_name: std::ptr::null(),
            class_name: class.as_ptr(),
            small_icon: 0,
        };
        unsafe { RegisterClassExW(&wc) != 0 }
    }

    unsafe extern "system" fn widget_window_proc(
        hwnd: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        match message {
            WM_PAINT => {
                unsafe { paint_widget(hwnd) };
                0
            }
            WM_ERASEBKGND => 1,
            WM_MOUSEACTIVATE => MA_NOACTIVATE,
            WM_SETCURSOR => {
                unsafe { SetCursor(LoadCursorW(0, IDC_ARROW as *const u16)) };
                1
            }
            WM_MOUSEMOVE => {
                begin_hover_dwell(hwnd);
                0
            }
            WM_MOUSELEAVE => {
                cancel_hover_dwell(hwnd);
                0
            }
            WM_TIMER if wparam == HOVER_TIMER_ID => {
                unsafe { KillTimer(hwnd, HOVER_TIMER_ID) };
                if hover_open_enabled() {
                    open_flyout(hwnd);
                }
                0
            }
            WM_LBUTTONUP => {
                // A deliberate click owns the interaction until the pointer
                // leaves, so the pending hover timer cannot immediately undo
                // a click-to-close action.
                unsafe { KillTimer(hwnd, HOVER_TIMER_ID) };
                HOVER_FLYOUT_OPEN.store(false, Ordering::Release);
                toggle_flyout(hwnd);
                0
            }
            WM_DESTROY => {
                cancel_hover_dwell(hwnd);
                0
            }
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }

    fn hover_open_enabled() -> bool {
        HOST.get()
            .and_then(|host| host.try_lock().ok().map(|state| state.model.open_on_hover))
            .unwrap_or(false)
    }

    fn begin_hover_dwell(hwnd: isize) {
        if !hover_open_enabled()
            || HOVER_TRACKING
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }

        let mut tracking = TrackMouseEventParams {
            size: std::mem::size_of::<TrackMouseEventParams>() as u32,
            flags: TME_LEAVE,
            hwnd_track: hwnd,
            hover_time: 0,
        };
        let leave_armed = unsafe { TrackMouseEvent(&mut tracking) } != 0;
        let timer_armed = unsafe { SetTimer(hwnd, HOVER_TIMER_ID, HOVER_DWELL_MS, None) } != 0;
        if !leave_armed || !timer_armed {
            cancel_hover_dwell(hwnd);
        }
    }

    fn cancel_hover_dwell(hwnd: isize) {
        unsafe { KillTimer(hwnd, HOVER_TIMER_ID) };
        HOVER_TRACKING.store(false, Ordering::Release);
    }

    fn remember_flyout_anchor(app: &tauri::AppHandle, hwnd: isize) {
        // Treat the widget rectangle as the tray anchor so the existing
        // compact flyout opens visually connected to this taskbar surface.
        // Never wait for AppState from Explorer's mouse-message path.
        let mut rect = WinRect::default();
        if unsafe { GetWindowRect(hwnd, (&mut rect as *mut WinRect).cast()) } != 0
            && let Some(state) = app.try_state::<Mutex<crate::state::AppState>>()
            && let Ok(mut state) = state.try_lock()
        {
            state.tray_anchor = Some(crate::state::TrayAnchor {
                x: rect.left,
                y: rect.top,
                width: rect.right.saturating_sub(rect.left).max(1) as u32,
                height: rect.bottom.saturating_sub(rect.top).max(1) as u32,
            });
        }
    }

    fn open_flyout(hwnd: isize) {
        let Some(app) = APP.get().cloned() else {
            return;
        };
        remember_flyout_anchor(&app, hwnd);
        tauri::async_runtime::spawn(async move {
            if let Err(error) = crate::shell::flyout_window::open_or_focus(&app, None) {
                HOVER_FLYOUT_OPEN.store(false, Ordering::Release);
                tracing::warn!(%error, "Could not open native taskbar widget flyout on hover");
                return;
            }
            if !HOVER_FLYOUT_OPEN.swap(true, Ordering::AcqRel) {
                monitor_hover_flyout(app, hwnd).await;
            }
        });
    }

    async fn monitor_hover_flyout(app: tauri::AppHandle, widget_hwnd: isize) {
        let mut outside_since = None;
        loop {
            tokio::time::sleep(HOVER_POINTER_POLL).await;
            if !HOVER_FLYOUT_OPEN.load(Ordering::Acquire) {
                return;
            }

            let Some(pointer) = cursor_position() else {
                continue;
            };
            if point_is_inside_window(widget_hwnd, pointer) || point_is_inside_flyout(&app, pointer)
            {
                outside_since = None;
                continue;
            }

            let since = outside_since.get_or_insert_with(std::time::Instant::now);
            if since.elapsed() < HOVER_DISMISS_GRACE {
                continue;
            }

            if HOVER_FLYOUT_OPEN.swap(false, Ordering::AcqRel)
                && let Err(error) = crate::shell::flyout_window::hide(&app)
            {
                tracing::warn!(%error, "Could not dismiss native taskbar hover flyout");
            }
            return;
        }
    }

    fn cursor_position() -> Option<WinPoint> {
        let mut point = WinPoint { x: 0, y: 0 };
        (unsafe { GetCursorPos(&mut point) } != 0).then_some(point)
    }

    fn point_is_inside_window(hwnd: isize, point: WinPoint) -> bool {
        if hwnd == 0 || unsafe { IsWindow(hwnd) } == 0 {
            return false;
        }
        let mut rect = WinRect::default();
        (unsafe { GetWindowRect(hwnd, (&mut rect as *mut WinRect).cast()) }) != 0
            && point_is_inside_rect(point, &rect)
    }

    fn point_is_inside_flyout(app: &tauri::AppHandle, point: WinPoint) -> bool {
        let Some(window) = app.get_webview_window(crate::shell::flyout_window::FLYOUT_LABEL) else {
            return false;
        };
        if !window.is_visible().unwrap_or(false) {
            HOVER_FLYOUT_OPEN.store(false, Ordering::Release);
            return false;
        }
        let (Ok(position), Ok(size)) = (window.outer_position(), window.outer_size()) else {
            return false;
        };
        let rect = WinRect {
            left: position.x,
            top: position.y,
            right: position.x.saturating_add(size.width as i32),
            bottom: position.y.saturating_add(size.height as i32),
        };
        point_is_inside_rect(point, &rect)
    }

    fn point_is_inside_rect(point: WinPoint, rect: &WinRect) -> bool {
        point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
    }

    fn toggle_flyout(hwnd: isize) {
        let Some(app) = APP.get().cloned() else {
            return;
        };
        remember_flyout_anchor(&app, hwnd);

        tauri::async_runtime::spawn(async move {
            let flyout = app.get_webview_window(crate::shell::flyout_window::FLYOUT_LABEL);
            let visible = flyout
                .as_ref()
                .and_then(|window| window.is_visible().ok())
                .unwrap_or(false);
            let result = if visible {
                crate::shell::flyout_window::hide(&app)
            } else {
                crate::shell::flyout_window::open_or_focus(&app, None)
            };
            if let Err(error) = result {
                tracing::warn!(%error, "Could not toggle native taskbar widget flyout");
            }
        });
    }

    unsafe fn paint_widget(hwnd: isize) {
        let mut paint = PaintStruct::default();
        let hdc = unsafe { BeginPaint(hwnd, &mut paint) };
        if hdc == 0 {
            return;
        }

        let model = HOST
            .get()
            .and_then(|host| host.try_lock().ok().map(|state| state.model.clone()))
            .unwrap_or_default();
        let mut rect = WinRect::default();
        unsafe { GetClientRect(hwnd, &mut rect) };
        let background = unsafe { CreateSolidBrush(TRANSPARENT_KEY) };
        unsafe {
            FillRect(hdc, &rect, background);
            DeleteObject(background);
            SetBkMode(hdc, TRANSPARENT);
        }

        // Match Windows 11's compact taskbar typography. The Small optical cut
        // keeps counters and spacing legible at the compact sizes used by
        // Widgets (Weather), with a restrained medium/regular hierarchy.
        let face = wide("Segoe UI Variable Small");
        let primary_font = unsafe {
            CreateFontW(
                -14,
                0,
                0,
                0,
                500,
                0,
                0,
                0,
                1,
                0,
                0,
                FONT_QUALITY_ANTIALIASED,
                0,
                face.as_ptr(),
            )
        };
        let detail_font = unsafe {
            CreateFontW(
                -12,
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                1,
                0,
                0,
                FONT_QUALITY_ANTIALIASED,
                0,
                face.as_ptr(),
            )
        };
        let old_font = unsafe { SelectObject(hdc, primary_font) };
        let count = i32::try_from(model.providers.len()).unwrap_or(1).max(1);
        let item_width = (rect.right - rect.left) / count;
        let middle = (rect.bottom - rect.top) / 2;
        let text_color = if model.dark_text {
            rgb(24, 24, 24)
        } else {
            rgb(255, 255, 255)
        };

        for (index, provider) in model.providers.iter().enumerate() {
            let item_left = i32::try_from(index).unwrap_or(0) * item_width;
            let color = provider_color(&provider.provider_id, model.dark_text);
            const ICON_WIDTH: i32 = 16;
            const ICON_TEXT_GAP: i32 = 5;
            // Widest spelling that stays inside this tile. The strip paints
            // without clipping and `centered_content_x` pins overlong content to
            // the cell's left edge, so an unchecked "$1112.92" on a crowded
            // taskbar would run across the divider into the next provider.
            let percent_label = provider.percent.map(|percent| format!("{percent}%"));
            let budget = item_width
                .saturating_sub(ICON_WIDTH)
                .saturating_sub(ICON_TEXT_GAP);
            let mut best: Option<(Vec<u16>, i32)> = None;
            for candidate in [
                provider.amount_label.as_deref(),
                provider.amount_label_compact.as_deref(),
                percent_label.as_deref(),
                // "Unavailable", then "n/a", before the em dash: a placeholder
                // window is a known state, not the unknown a fetch error leaves
                // (SBS-876).
                provider.named_label.as_deref(),
                provider.named_label_compact.as_deref(),
                Some("—"),
            ]
            .into_iter()
            .flatten()
            {
                let wide = wide_without_nul(candidate);
                let width = unsafe { text_width(hdc, &wide) };
                let fits = width <= budget;
                // Take the first that fits; failing that, keep the narrowest so
                // a tile too small for any spelling overruns as little as possible.
                let better = match &best {
                    None => true,
                    Some((_, best_width)) => *best_width > budget && width < *best_width,
                };
                if better {
                    best = Some((wide, width));
                }
                if fits {
                    break;
                }
            }
            let (label, label_width) = best.expect("the em dash always yields a candidate");
            let primary_width = ICON_WIDTH
                .saturating_add(ICON_TEXT_GAP)
                .saturating_add(label_width);
            let primary_left = centered_content_x(item_left, item_width, primary_width);
            unsafe {
                draw_provider_icon(
                    hdc,
                    &provider.provider_id,
                    primary_left + ICON_WIDTH / 2,
                    middle - 7,
                    color,
                )
            };
            unsafe {
                SetTextColor(hdc, text_color);
                TextOutW(
                    hdc,
                    primary_left + ICON_WIDTH + ICON_TEXT_GAP,
                    middle - 16,
                    label.as_ptr(),
                    label.len() as i32,
                );
            }

            // Window label (+ optional reset). Account identity is flyout-only
            // so long tags don't collide with the next tile.
            let spellings = detail_spellings(&provider.window_label, provider.reset.as_deref());
            unsafe {
                // Measure in the font the line is actually drawn in, not the
                // headline one selected above.
                SelectObject(hdc, detail_font);
                let budget = item_width.saturating_sub(DETAIL_GUTTER);
                let chosen = fit_detail(&spellings, budget, |text| {
                    text_width(hdc, &wide_without_nul(text))
                });
                let detail = wide_without_nul(chosen);
                SetTextColor(hdc, text_color);
                let detail_width = text_width(hdc, &detail);
                TextOutW(
                    hdc,
                    centered_content_x(item_left, item_width, detail_width),
                    middle + 1,
                    detail.as_ptr(),
                    detail.len() as i32,
                );
                SelectObject(hdc, primary_font);
            }

            if index + 1 < model.providers.len() {
                let separator = unsafe { CreatePen(PS_SOLID, 1, rgb(118, 127, 140)) };
                let old_pen = unsafe { SelectObject(hdc, separator) };
                unsafe {
                    MoveToEx(
                        hdc,
                        item_left + item_width - 1,
                        middle - 13,
                        std::ptr::null_mut(),
                    );
                    LineTo(hdc, item_left + item_width - 1, middle + 13);
                    SelectObject(hdc, old_pen);
                    DeleteObject(separator);
                }
            }
        }

        unsafe {
            SelectObject(hdc, old_font);
            DeleteObject(primary_font);
            DeleteObject(detail_font);
            EndPaint(hwnd, &paint);
        }
    }

    unsafe fn draw_provider_icon(hdc: isize, provider_id: &str, x: i32, y: i32, color: u32) {
        const CODEX: [u16; 16] = [
            0x0000, 0x0000, 0x03e0, 0x1f10, 0x10d8, 0x2f54, 0x39d4, 0x2654, 0x2a64, 0x2bdc, 0x2af4,
            0x1b08, 0x0cf8, 0x07c0, 0x0000, 0x0000,
        ];
        const CLAUDE: [u16; 16] = [
            0x0000, 0x0000, 0x0320, 0x1b60, 0x0d48, 0x0ff8, 0x07e0, 0x3fc4, 0x1ff8, 0x37e0, 0x0ff8,
            0x1f68, 0x04a0, 0x0080, 0x0000, 0x0000,
        ];
        const CURSOR: [u16; 16] = [
            0x0000, 0x0000, 0x03c0, 0x0ff0, 0x1ff8, 0x200c, 0x303c, 0x307c, 0x38fc, 0x38fc, 0x3cfc,
            0x1ef8, 0x0ef0, 0x03c0, 0x0000, 0x0000,
        ];
        // 16x16 raster of the official Grok monogram (same path as ProviderIcon-grok.svg).
        const GROK: [u16; 16] = [
            0x0000, 0x0000, 0x47c0, 0x27f0, 0x3038, 0x3818, 0x340c, 0x320c, 0x310c, 0x300c, 0x3018,
            0x1818, 0x0fec, 0x07e4, 0x0000, 0x0000,
        ];
        // 16x16 four-point star approximating ProviderIcon-gemini.svg (was falling
        // through to a hollow ring, so Gemini looked like a blank circle on the strip).
        const GEMINI: [u16; 16] = [
            0x01c0, 0x03e0, 0x03e0, 0x03e0, 0x03e0, 0x3dde, 0x7ebf, 0x7fff, 0x7ebf, 0x3dde, 0x03e0,
            0x03e0, 0x03e0, 0x03e0, 0x01c0, 0x0000,
        ];
        // Compact Antigravity mark: two upper lobes over a center stem.
        const ANTIGRAVITY: [u16; 16] = [
            0x03c0, 0x07e0, 0x03e0, 0x07f0, 0x0f78, 0x1e3c, 0x3ff8, 0x1ff0, 0x0fe0, 0x07c0, 0x03c0,
            0x03c0, 0x03c0, 0x03c0, 0x0000, 0x0000,
        ];
        // OpenCode mark: square ring (outer square minus an inner square hole),
        // matching ProviderIcon-opencode.svg and the shared OpenCode/OpenCode Go brand.
        const OPENCODE: [u16; 16] = [
            0x0000, 0x3ffc, 0x3ffc, 0x3ffc, 0x381c, 0x381c, 0x381c, 0x381c, 0x381c, 0x381c, 0x381c,
            0x381c, 0x3ffc, 0x3ffc, 0x3ffc, 0x0000,
        ];

        let mask = match provider_id {
            "codex" => Some(&CODEX),
            "claude" => Some(&CLAUDE),
            "cursor" => Some(&CURSOR),
            "grok" => Some(&GROK),
            "gemini" => Some(&GEMINI),
            "antigravity" | "agy" => Some(&ANTIGRAVITY),
            "opencode" | "opencodego" => Some(&OPENCODE),
            _ => None,
        };
        if let Some(mask) = mask {
            unsafe { draw_icon_mask(hdc, mask, x - 8, y - 8, color) };
            return;
        }

        // Unknown providers: solid disc so they still read as a mark, not an empty ring.
        let pen = unsafe { CreatePen(PS_SOLID, 1, color) };
        let brush = unsafe { CreateSolidBrush(color) };
        let old_pen = unsafe { SelectObject(hdc, pen) };
        let old_brush = unsafe { SelectObject(hdc, brush) };
        unsafe {
            Ellipse(hdc, x - 6, y - 6, x + 6, y + 6);
            SelectObject(hdc, old_brush);
            DeleteObject(brush);
            SelectObject(hdc, old_pen);
            DeleteObject(pen);
        }
    }

    unsafe fn draw_icon_mask(hdc: isize, rows: &[u16; 16], x: i32, y: i32, color: u32) {
        for (row_index, row) in rows.iter().copied().enumerate() {
            for column in 0..16 {
                if row & (1 << column) != 0 {
                    unsafe { SetPixelV(hdc, x + column, y + row_index as i32, color) };
                }
            }
        }
    }

    /// Tile color for a provider mark.
    ///
    /// `dark_text` is the strip's own read of `SystemUsesLightTheme`, so it is
    /// also the answer to "is the taskbar behind this mark light?". A brand with
    /// a real hue reads on either chrome and ignores it. A monochrome brand does
    /// not: it ships as black-on-white *and* white-on-black precisely because one
    /// shade cannot serve both, so painting one fixed shade leaves it at roughly
    /// 1:1 against half of all taskbars (SBS-876 follow-up).
    fn provider_color(provider_id: &str, dark_text: bool) -> u32 {
        match provider_id {
            "claude" => rgb(216, 116, 75),
            "cursor" => rgb(15, 201, 181),
            "codex" => rgb(64, 196, 222),
            // Match the web registry brand colors so strip and dashboard agree.
            "gemini" => rgb(171, 135, 234),
            "antigravity" | "agy" => rgb(96, 186, 126),
            "copilot" => rgb(168, 85, 247),
            // Monochrome marks; each takes the half of its own brand that the
            // current taskbar can actually show.
            "grok" if dark_text => rgb(24, 24, 24),
            "grok" => rgb(231, 233, 234),
            // #211E1E is OpenCode's own black, straight off the brand asset.
            "opencode" | "opencodego" if dark_text => rgb(33, 30, 30),
            "opencode" | "opencodego" => rgb(231, 233, 234),
            _ if dark_text => rgb(72, 79, 88),
            _ => rgb(204, 211, 220),
        }
    }

    /// Pixels held back from the detail line's own tile.
    ///
    /// Kept small on purpose: the placer gives a provider 104px at most and
    /// 72px when the gap is tight, so a pixel spent here is roughly a character
    /// of reset, and enough of them drop the countdown altogether.
    ///
    /// Four clears the divider with a pixel to spare on every tile the placer
    /// can build. `centered_content_x` puts content of width `w` on a tile of
    /// width `t` at `(t - w) / 2`, and the separator is drawn at `t - 1` while
    /// the strip does not clip. Three is arguably enough on the arithmetic
    /// alone, but that margin assumes exact glyph extents, and antialiasing on
    /// the trailing character does not respect them.
    /// See `an_accepted_line_never_reaches_the_separator`.
    const DETAIL_GUTTER: i32 = 4;

    /// Spellings of the detail line, widest first.
    ///
    /// Three rungs, because the drop from a full countdown to none is too far
    /// to take in one step. `tooltip_short_reset` writes two units (`25d 23h`,
    /// and `23h 59m` once under a day), and the second of those is the widest
    /// string this line ever carries — wide enough to miss even the roomiest
    /// tile. Losing the countdown there means losing it for the last day before
    /// a reset, which is when it is worth most. So the middle rung keeps the
    /// leading unit and drops the finer one: a coarser countdown beats none.
    ///
    /// The last rung is the bare window name, which still says *which* ceiling
    /// this is; the figure is a hover away in the flyout.
    ///
    /// A blank reset is treated as no reset. It formatted to `"Monthly · "`,
    /// which is wider than the label alone and so would win the ladder and draw
    /// a separator with nothing after it.
    fn detail_spellings(window_label: &str, reset: Option<&str>) -> Vec<String> {
        let Some(reset) = reset.filter(|reset| !reset.trim().is_empty()) else {
            return vec![window_label.to_string()];
        };
        let reset = reset.trim();
        let mut spellings = vec![format!("{window_label} · {reset}")];
        if let Some(lead) = reset.split_whitespace().next()
            && lead != reset
        {
            spellings.push(format!("{window_label} · {lead}"));
        }
        spellings.push(window_label.to_string());
        spellings
    }

    /// Widest spelling that fits `budget`; the narrowest when none do.
    ///
    /// Same ladder the headline uses, and for the same reason. The rule here
    /// used to be `chars().take(15)`, which is a guess at a pixel width and was
    /// wrong in both directions: "Monthly · 10d 1h" is 16 characters and lost
    /// its trailing "h" on every monthly window with a two-digit day, while
    /// "Weekly · 3d 23h" fit at exactly 15 and a narrow five-provider strip
    /// could overrun on 15 wide characters anyway. Characters are not a unit of
    /// width, so measure the text (SBS-889).
    fn fit_detail(spellings: &[String], budget: i32, measure: impl Fn(&str) -> i32) -> &str {
        let mut best: Option<(&str, i32)> = None;
        for candidate in spellings {
            let width = measure(candidate);
            // Take the first that fits; failing that, keep the narrowest so a
            // tile too small for any spelling overruns as little as possible.
            let better = match best {
                None => true,
                Some((_, best_width)) => best_width > budget && width < best_width,
            };
            if better {
                best = Some((candidate.as_str(), width));
            }
            if width <= budget {
                break;
            }
        }
        best.map_or("", |(text, _)| text)
    }

    #[cfg(test)]
    mod detail_fit_tests {
        use super::*;

        /// Stand-in for `text_width`: the real one needs a device context, so
        /// the ladder takes the measurement as a closure and the test supplies
        /// a fixed width per character.
        fn measure(text: &str) -> i32 {
            text.chars().count() as i32 * 7
        }

        #[test]
        fn keeps_the_reset_when_the_tile_can_hold_it() {
            let spellings = detail_spellings("Monthly", Some("10d 1h"));
            assert_eq!(fit_detail(&spellings, 200, measure), "Monthly · 10d 1h");
        }

        #[test]
        fn drops_a_whole_unit_rather_than_a_character_of_one() {
            // The bug this replaces: a 15-character cut left "Monthly · 10d 1",
            // which reads as a real countdown that is off by an hour rather than
            // as a line that was cut. Every rung below the full spelling is a
            // countdown someone can trust, just a coarser one.
            let spellings = detail_spellings("Monthly", Some("10d 1h"));
            let budget = measure("Monthly · 10d 1h") - 1;
            assert_eq!(fit_detail(&spellings, budget, measure), "Monthly · 10d");
        }

        #[test]
        fn a_label_that_cannot_fit_overruns_as_little_as_possible() {
            // No rung fits, so the narrowest wins. It still overruns, but by the
            // least it can, and it is never a partial reset.
            let spellings = detail_spellings("Monthly", Some("10d 1h"));
            assert_eq!(fit_detail(&spellings, 1, measure), "Monthly");
        }

        #[test]
        fn a_window_with_no_reset_has_one_rung() {
            let spellings = detail_spellings("Weekly", None);
            assert_eq!(spellings.len(), 1);
            assert_eq!(fit_detail(&spellings, 200, measure), "Weekly");
            assert_eq!(fit_detail(&spellings, 1, measure), "Weekly");
        }

        /// The widest line this row ever carries is the last-day form, because
        /// `tooltip_short_reset` switches to `{h}h {m}m` under a day and two
        /// digits of hours plus minutes beats `{d}d {h}h`. It can miss even a
        /// 104px tile, and dropping straight to the bare label would blank the
        /// countdown for the final day, when it matters most.
        #[test]
        fn a_last_day_countdown_gets_coarser_before_it_disappears() {
            let spellings = detail_spellings("Monthly", Some("23h 59m"));
            assert_eq!(
                spellings,
                vec![
                    "Monthly · 23h 59m".to_string(),
                    "Monthly · 23h".to_string(),
                    "Monthly".to_string(),
                ]
            );

            // Widths standing in for Segoe UI Variable Small at the detail size:
            // the full line misses a 104px tile's 100px budget, the hours-only
            // rung clears it comfortably.
            let widths = |text: &str| match text {
                "Monthly · 23h 59m" => 103,
                "Monthly · 23h" => 75,
                "Monthly" => 45,
                other => panic!("unexpected spelling {other:?}"),
            };
            let budget = 104 - DETAIL_GUTTER;
            assert_eq!(fit_detail(&spellings, budget, widths), "Monthly · 23h");
            // A tile too tight for even that still keeps the name.
            assert_eq!(fit_detail(&spellings, 50, widths), "Monthly");
        }

        #[test]
        fn a_single_unit_reset_has_no_middle_rung() {
            // "45m" has nothing coarser to fall back to.
            let spellings = detail_spellings("Weekly", Some("45m"));
            assert_eq!(
                spellings,
                vec!["Weekly · 45m".to_string(), "Weekly".to_string()]
            );
        }

        #[test]
        fn a_day_and_hour_reset_falls_back_to_days() {
            let spellings = detail_spellings("Monthly", Some("25d 23h"));
            assert_eq!(
                spellings,
                vec![
                    "Monthly · 25d 23h".to_string(),
                    "Monthly · 25d".to_string(),
                    "Monthly".to_string(),
                ]
            );
        }

        #[test]
        fn a_blank_reset_is_treated_as_no_reset() {
            // `"Monthly · "` is wider than `"Monthly"`, so it would win the
            // ladder wherever it fits and draw a separator with nothing after.
            for blank in ["", " ", "   "] {
                let spellings = detail_spellings("Monthly", Some(blank));
                assert_eq!(spellings, vec!["Monthly".to_string()], "{blank:?}");
            }
        }

        /// Whatever the ladder accepts has to stay off the divider, on every
        /// tile the placer can produce.
        ///
        /// Arithmetic over the real placer range rather than a font
        /// measurement, so it holds whatever Segoe does with a given string.
        #[test]
        fn an_accepted_line_never_reaches_the_separator() {
            // `placement` gives each provider 104px, or 72px when space is tight.
            for tile in 72..=104 {
                let budget = tile - DETAIL_GUTTER;
                let separator = tile - 1;
                for width in 1..=budget {
                    let right_edge = centered_content_x(0, tile, width) + width;
                    assert!(
                        right_edge < separator,
                        "tile {tile}, content {width}: right edge {right_edge} \
                         reaches separator at {separator}"
                    );
                }
            }
        }
    }

    #[cfg(test)]
    mod tile_color_tests {
        use super::*;

        /// Relative luminance of a GDI COLORREF (0x00bbggrr), per WCAG 2.x.
        fn luminance(color: u32) -> f64 {
            let channel = |shift: u32| {
                let value = ((color >> shift) & 0xff) as f64 / 255.0;
                if value <= 0.03928 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(0) + 0.7152 * channel(8) + 0.0722 * channel(16)
        }

        fn contrast(a: u32, b: u32) -> f64 {
            let (x, y) = (luminance(a), luminance(b));
            let (hi, lo) = if x > y { (x, y) } else { (y, x) };
            (hi + 0.05) / (lo + 0.05)
        }

        #[test]
        fn monochrome_marks_follow_the_taskbar_theme() {
            // Windows 11 taskbars ship in both shades, and `dark_text` is the
            // strip's own read of which one it is sitting on. A brand with no hue
            // has to pick the half of itself the current chrome can show; one
            // fixed shade left the mark at roughly 1:1 on half of all taskbars.
            const DARK_TASKBAR: u32 = rgb(32, 32, 32);
            const LIGHT_TASKBAR: u32 = rgb(243, 243, 243);

            for id in ["opencode", "opencodego", "grok"] {
                let on_dark = provider_color(id, false);
                let on_light = provider_color(id, true);
                assert_ne!(on_dark, on_light, "{id} must not paint one fixed shade");
                assert!(
                    contrast(on_dark, DARK_TASKBAR) > 3.0,
                    "{id} is unreadable on a dark taskbar"
                );
                assert!(
                    contrast(on_light, LIGHT_TASKBAR) > 3.0,
                    "{id} is unreadable on a light taskbar"
                );
            }

            // A brand with a real hue reads on either chrome and stays put.
            for id in ["claude", "cursor", "codex", "gemini", "copilot"] {
                assert_eq!(
                    provider_color(id, false),
                    provider_color(id, true),
                    "{id} has a brand hue and should not switch"
                );
            }
        }
    }

    const fn rgb(red: u8, green: u8, blue: u8) -> u32 {
        red as u32 | ((green as u32) << 8) | ((blue as u32) << 16)
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn wide_without_nul(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }

    unsafe fn text_width(hdc: isize, text: &[u16]) -> i32 {
        let mut size = WinSize::default();
        if text.is_empty()
            || unsafe { GetTextExtentPoint32W(hdc, text.as_ptr(), text.len() as i32, &mut size) }
                == 0
        {
            return i32::try_from(text.len()).unwrap_or(0).saturating_mul(7);
        }
        size.cx.max(0)
    }

    #[repr(C)]
    struct WndClassExW {
        size: u32,
        style: u32,
        window_proc: Option<unsafe extern "system" fn(isize, u32, usize, isize) -> isize>,
        class_extra: i32,
        window_extra: i32,
        instance: isize,
        icon: isize,
        cursor: isize,
        background: isize,
        menu_name: *const u16,
        class_name: *const u16,
        small_icon: isize,
    }

    #[repr(C)]
    #[derive(Default)]
    struct WinRect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct WinPoint {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct WinSize {
        cx: i32,
        cy: i32,
    }

    #[repr(C)]
    struct TrackMouseEventParams {
        size: u32,
        flags: u32,
        hwnd_track: isize,
        hover_time: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct PaintStruct {
        hdc: isize,
        erase: i32,
        paint: WinRect,
        restore: i32,
        incremental_update: i32,
        reserved: [u8; 32],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(module_name: *const u16) -> isize;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassExW(class: *const WndClassExW) -> u16;
        fn CreateWindowExW(
            extended_style: u32,
            class_name: *const u16,
            window_name: *const u16,
            style: u32,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            parent: isize,
            menu: isize,
            instance: isize,
            param: *const std::ffi::c_void,
        ) -> isize;
        fn DefWindowProcW(hwnd: isize, message: u32, wparam: usize, lparam: isize) -> isize;
        fn FindWindowW(class_name: *const u16, window_name: *const u16) -> isize;
        fn GetParent(hwnd: isize) -> isize;
        fn SetParent(child: isize, new_parent: isize) -> isize;
        fn SetLayeredWindowAttributes(hwnd: isize, color_key: u32, alpha: u8, flags: u32) -> i32;
        fn DestroyWindow(hwnd: isize) -> i32;
        fn IsWindow(hwnd: isize) -> i32;
        fn SetWindowPos(
            hwnd: isize,
            insert_after: isize,
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            flags: u32,
        ) -> i32;
        fn ShowWindow(hwnd: isize, command: i32) -> i32;
        fn InvalidateRect(hwnd: isize, rect: *const WinRect, erase: i32) -> i32;
        fn LoadCursorW(instance: isize, cursor_name: *const u16) -> isize;
        fn SetCursor(cursor: isize) -> isize;
        fn TrackMouseEvent(event: *mut TrackMouseEventParams) -> i32;
        fn SetTimer(
            hwnd: isize,
            event_id: usize,
            interval_ms: u32,
            callback: Option<unsafe extern "system" fn(isize, u32, usize, u32)>,
        ) -> usize;
        fn KillTimer(hwnd: isize, event_id: usize) -> i32;
        fn BeginPaint(hwnd: isize, paint: *mut PaintStruct) -> isize;
        fn EndPaint(hwnd: isize, paint: *const PaintStruct) -> i32;
        fn GetClientRect(hwnd: isize, rect: *mut WinRect) -> i32;
        fn GetWindowRect(hwnd: isize, rect: *mut std::ffi::c_void) -> i32;
        fn GetCursorPos(point: *mut WinPoint) -> i32;
        fn GetDC(hwnd: isize) -> isize;
        fn ReleaseDC(hwnd: isize, hdc: isize) -> i32;
        fn FillRect(hdc: isize, rect: *const WinRect, brush: isize) -> i32;
        fn SetWindowRgn(hwnd: isize, region: isize, redraw: i32) -> i32;
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            key: isize,
            sub_key: *const u16,
            value: *const u16,
            flags: u32,
            value_type: *mut u32,
            data: *mut std::ffi::c_void,
            data_size: *mut u32,
        ) -> i32;
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn GetPixel(hdc: isize, x: i32, y: i32) -> u32;
        fn CreateSolidBrush(color: u32) -> isize;
        fn CreatePen(style: i32, width: i32, color: u32) -> isize;
        fn CreateFontW(
            height: i32,
            width: i32,
            escapement: i32,
            orientation: i32,
            weight: i32,
            italic: u32,
            underline: u32,
            strike_out: u32,
            char_set: u32,
            output_precision: u32,
            clip_precision: u32,
            quality: u32,
            pitch_and_family: u32,
            face: *const u16,
        ) -> isize;
        fn DeleteObject(object: isize) -> i32;
        fn SelectObject(hdc: isize, object: isize) -> isize;
        fn SetBkMode(hdc: isize, mode: i32) -> i32;
        fn SetTextColor(hdc: isize, color: u32) -> u32;
        fn GetTextExtentPoint32W(
            hdc: isize,
            text: *const u16,
            count: i32,
            size: *mut WinSize,
        ) -> i32;
        fn TextOutW(hdc: isize, x: i32, y: i32, text: *const u16, count: i32) -> i32;
        fn MoveToEx(hdc: isize, x: i32, y: i32, previous: *mut WinPoint) -> i32;
        fn LineTo(hdc: isize, x: i32, y: i32) -> i32;
        fn Ellipse(hdc: isize, left: i32, top: i32, right: i32, bottom: i32) -> i32;
        fn SetPixelV(hdc: isize, x: i32, y: i32, color: u32) -> i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::floatbar::placement::Rect;
    use codexbar::settings::Settings;

    #[test]
    fn provider_content_is_centered_inside_its_segment() {
        assert_eq!(centered_content_x(92, 92, 48), 114);
        assert_eq!(centered_content_x(184, 92, 60), 200);
    }

    #[test]
    fn oversized_provider_content_stays_at_the_segment_start() {
        assert_eq!(centered_content_x(92, 72, 90), 92);
    }

    #[test]
    fn mirrored_widget_survives_a_partial_taskbar_discovery_pass() {
        assert!(taskbar_remains_selected(2, &[1], true));
        assert!(!taskbar_remains_selected(2, &[1], false));
        assert!(taskbar_remains_selected(1, &[1], false));
    }

    fn layout(bounds: Rect, obstacles: Vec<Rect>) -> TaskbarLayout {
        TaskbarLayout {
            window_handle: 1,
            bounds,
            monitor_bounds: Rect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            obstacles,
            landmarks: TaskbarLandmarks::default(),
            primary: true,
        }
    }

    #[test]
    fn multi_monitor_setting_includes_secondary_taskbars_only_when_enabled() {
        let primary = TaskbarLayout {
            primary: true,
            ..layout(
                Rect {
                    left: 0,
                    top: 1392,
                    right: 2560,
                    bottom: 1440,
                },
                Vec::new(),
            )
        };
        let secondary = TaskbarLayout {
            window_handle: 2,
            primary: false,
            ..layout(
                Rect {
                    left: -1920,
                    top: 1032,
                    right: 0,
                    bottom: 1080,
                },
                Vec::new(),
            )
        };

        assert!(layout_is_enabled(&primary, false));
        assert!(!layout_is_enabled(&secondary, false));
        assert!(layout_is_enabled(&primary, true));
        assert!(layout_is_enabled(&secondary, true));
    }

    #[test]
    fn mixed_resolution_taskbars_receive_independent_local_placements() {
        let primary_bounds = Rect {
            left: 0,
            top: 1392,
            right: 2560,
            bottom: 1440,
        };
        let secondary_bounds = Rect {
            left: -1920,
            top: 1032,
            right: 0,
            bottom: 1080,
        };
        let primary = TaskbarLayout {
            window_handle: 1,
            landmarks: landmarks(
                Rect {
                    left: 0,
                    top: 1392,
                    right: 160,
                    bottom: 1440,
                },
                Rect {
                    left: 1120,
                    top: 1392,
                    right: 1168,
                    bottom: 1440,
                },
            ),
            ..layout(primary_bounds, Vec::new())
        };
        let secondary = TaskbarLayout {
            window_handle: 2,
            bounds: secondary_bounds,
            monitor_bounds: Rect {
                left: -1920,
                top: 0,
                right: 0,
                bottom: 1080,
            },
            landmarks: TaskbarLandmarks {
                widgets: None,
                start: Some(Rect {
                    left: -1040,
                    top: 1032,
                    right: -992,
                    bottom: 1080,
                }),
                tray: None,
            },
            primary: false,
            ..layout(secondary_bounds, Vec::new())
        };

        let (discovered, rejected, placements) = taskbar_placements(&[primary, secondary], true, 3);
        assert_eq!(discovered, vec![1, 2]);
        assert!(rejected.is_empty());
        assert_eq!(placements.len(), 2);
        assert!(placements.iter().all(|(_, placement)| placement.x >= 0));
        assert_eq!(placements[0].1.width, 312);
        assert_eq!(placements[1].1.width, 312);
        assert!(
            placements
                .iter()
                .all(|(_, placement)| placement.height == 48)
        );
    }

    fn landmarks(widgets: Rect, start: Rect) -> TaskbarLandmarks {
        TaskbarLandmarks {
            widgets: Some(widgets),
            start: Some(start),
            tray: None,
        }
    }

    #[test]
    fn native_mode_requires_at_least_one_enabled_selected_provider() {
        let mut settings = Settings {
            float_bar_enabled: true,
            float_bar_style: "taskbar".to_string(),
            enabled_providers: ["codex".to_string()].into_iter().collect(),
            float_bar_provider_ids: vec!["claude".to_string()],
            ..Settings::default()
        };

        assert!(!native_mode_has_configured_provider(&settings));
        settings.float_bar_provider_ids = vec!["codex".to_string()];
        assert!(native_mode_has_configured_provider(&settings));
    }

    #[test]
    fn taskbar_strip_auto_includes_enabled_providers_after_cursor() {
        let settings = Settings {
            enabled_providers: ["codex", "claude", "cursor", "grok"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            float_bar_provider_ids: Vec::new(),
            ..Settings::default()
        };
        assert_eq!(
            taskbar_strip_provider_ids(&settings),
            vec![
                "codex".to_string(),
                "claude".to_string(),
                "cursor".to_string(),
                "grok".to_string(),
            ]
        );
    }

    #[test]
    fn taskbar_strip_respects_explicit_order_and_cap() {
        let settings = Settings {
            enabled_providers: ["codex", "claude", "cursor", "grok", "gemini"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            float_bar_provider_ids: vec![
                "grok".to_string(),
                "cursor".to_string(),
                "claude".to_string(),
                "codex".to_string(),
                "gemini".to_string(),
                "openaiapi".to_string(),
            ],
            ..Settings::default()
        };
        let ids = taskbar_strip_provider_ids(&settings);
        assert_eq!(
            ids,
            vec![
                "grok".to_string(),
                "cursor".to_string(),
                "claude".to_string(),
                "codex".to_string(),
                "gemini".to_string(),
            ]
        );
        assert_eq!(ids.len(), MAX_TASKBAR_WIDGET_PROVIDERS);
    }

    #[test]
    fn native_widget_uses_left_lane_taskbar_client_coordinates() {
        let taskbar = layout(
            Rect {
                left: -1920,
                top: 1032,
                right: 0,
                bottom: 1080,
            },
            vec![
                Rect {
                    left: -1920,
                    top: 1032,
                    right: -1760,
                    bottom: 1080,
                },
                Rect {
                    left: -1200,
                    top: 1032,
                    right: -800,
                    bottom: 1080,
                },
            ],
        );

        let placement = child_placement(
            &taskbar,
            landmarks(
                Rect {
                    left: -1920,
                    top: 1032,
                    right: -1760,
                    bottom: 1080,
                },
                Rect {
                    left: -1100,
                    top: 1032,
                    right: -1052,
                    bottom: 1080,
                },
            ),
            3,
        )
        .expect("the Widgets-to-Start lane should fit");
        assert_eq!(placement.x, 288);
        assert_eq!(placement.y, 0);
        assert_eq!(placement.width, 312);
        assert_eq!(placement.height, 48);
    }

    #[test]
    fn proof_widget_refuses_vertical_taskbars_for_now() {
        let taskbar = layout(
            Rect {
                left: 0,
                top: 0,
                right: 48,
                bottom: 1080,
            },
            vec![],
        );
        assert_eq!(
            child_placement(
                &taskbar,
                landmarks(
                    Rect {
                        left: 0,
                        top: 0,
                        right: 48,
                        bottom: 60,
                    },
                    Rect {
                        left: 0,
                        top: 500,
                        right: 48,
                        bottom: 548,
                    },
                ),
                3,
            ),
            None
        );
    }

    #[test]
    fn proof_widget_hides_when_no_complete_gap_exists() {
        let taskbar = layout(
            Rect {
                left: 0,
                top: 1032,
                right: 500,
                bottom: 1080,
            },
            vec![Rect {
                left: 0,
                top: 1032,
                right: 500,
                bottom: 1080,
            }],
        );
        assert_eq!(
            child_placement(
                &taskbar,
                landmarks(
                    Rect {
                        left: 0,
                        top: 1032,
                        right: 200,
                        bottom: 1080,
                    },
                    Rect {
                        left: 340,
                        top: 1032,
                        right: 388,
                        bottom: 1080,
                    },
                ),
                3,
            ),
            None
        );
        assert_eq!(
            placement_outcome(
                &taskbar,
                landmarks(
                    Rect {
                        left: 0,
                        top: 1032,
                        right: 200,
                        bottom: 1080,
                    },
                    Rect {
                        left: 340,
                        top: 1032,
                        right: 388,
                        bottom: 1080,
                    },
                ),
                3,
            ),
            PlacementOutcome::VerifiedNoFit
        );
    }

    #[test]
    fn missing_start_landmark_is_transient_instead_of_a_verified_rejection() {
        let taskbar = layout(
            Rect {
                left: 0,
                top: 1032,
                right: 1920,
                bottom: 1080,
            },
            vec![],
        );

        assert_eq!(
            placement_outcome(&taskbar, TaskbarLandmarks::default(), 3),
            PlacementOutcome::TransientLandmarks
        );
    }

    #[test]
    fn native_widget_uses_taskbar_edge_when_windows_widgets_are_disabled() {
        let taskbar = layout(
            Rect {
                left: 0,
                top: 1032,
                right: 1920,
                bottom: 1080,
            },
            vec![],
        );
        let placement = child_placement(
            &taskbar,
            TaskbarLandmarks {
                widgets: None,
                start: Some(Rect {
                    left: 800,
                    top: 1032,
                    right: 848,
                    bottom: 1080,
                }),
                tray: None,
            },
            3,
        )
        .expect("the taskbar edge-to-Start lane should fit");
        assert_eq!(placement.x, 244);
        assert_eq!(placement.width, 312);
    }

    #[test]
    fn native_widget_rejects_stale_landmarks_outside_the_taskbar() {
        let taskbar = layout(
            Rect {
                left: 0,
                top: 1032,
                right: 1920,
                bottom: 1080,
            },
            vec![],
        );

        assert_eq!(
            child_placement(
                &taskbar,
                landmarks(
                    Rect {
                        left: -160,
                        top: 1032,
                        right: 0,
                        bottom: 1080,
                    },
                    Rect {
                        left: 800,
                        top: 1032,
                        right: 848,
                        bottom: 1080,
                    },
                ),
                3,
            ),
            None
        );
    }

    #[test]
    fn native_widget_uses_only_a_verified_empty_sub_gap() {
        let taskbar = layout(
            Rect {
                left: 0,
                top: 1032,
                right: 1920,
                bottom: 1080,
            },
            vec![Rect {
                left: 300,
                top: 1032,
                right: 420,
                bottom: 1080,
            }],
        );

        let placement = child_placement(
            &taskbar,
            landmarks(
                Rect {
                    left: 0,
                    top: 1032,
                    right: 160,
                    bottom: 1080,
                },
                Rect {
                    left: 800,
                    top: 1032,
                    right: 848,
                    bottom: 1080,
                },
            ),
            3,
        )
        .expect("the verified gap after the obstacle should fit");

        assert_eq!(placement.x, 454);
        assert_eq!(placement.width, 312);
    }

    /// Left-aligned-Start fixture shared by the lane-2 fallback tests:
    /// Start pinned at the taskbar's left edge (lane 1's right edge goes
    /// negative), one icon-row obstacle at 56..700, tray as given.
    fn left_aligned_taskbar(tray: Option<Rect>) -> (TaskbarLayout, TaskbarLandmarks) {
        let taskbar = layout(
            Rect {
                left: 0,
                top: 1032,
                right: 1920,
                bottom: 1080,
            },
            vec![Rect {
                left: 56,
                top: 1032,
                right: 700,
                bottom: 1080,
            }],
        );
        let landmarks = TaskbarLandmarks {
            widgets: None,
            start: Some(Rect {
                left: 0,
                top: 1032,
                right: 48,
                bottom: 1080,
            }),
            tray,
        };
        (taskbar, landmarks)
    }

    /// This is the test that closes #261: stock Windows 11 "Taskbar
    /// alignment = Left" (or Windhawk's "Start button always on left") pins
    /// Start at the taskbar's left edge, starving lane 1 (its right edge
    /// goes negative). The widget now falls into lane 2, between Start and
    /// the tray, centered in the verified gap after the last icon.
    #[test]
    fn left_aligned_start_falls_back_to_the_tray_lane() {
        let (taskbar, landmarks) = left_aligned_taskbar(Some(Rect {
            left: 1700,
            top: 1032,
            right: 1920,
            bottom: 1080,
        }));

        let placement =
            child_placement(&taskbar, landmarks, 3).expect("lane 2 should fit after the icons");
        assert_eq!(placement.x, 1044);
        assert_eq!(placement.width, 312);
    }

    /// Same left-aligned-Start scenario, but the taskbar has no
    /// `TrayNotifyWnd` (secondary taskbars can omit it) — lane 2's right
    /// edge falls back to the taskbar's own right edge.
    #[test]
    fn missing_tray_landmark_falls_back_to_the_taskbar_right_edge() {
        let (taskbar, landmarks) = left_aligned_taskbar(None);

        let placement =
            child_placement(&taskbar, landmarks, 3).expect("lane 2 should fit after the icons");
        assert_eq!(placement.x, 1154);
        assert_eq!(placement.width, 312);
    }

    /// A tray rect present but off the taskbar band is stale (mid layout or
    /// DPI change). Treating it as "missing" would widen lane 2 over the
    /// tray's true position, so it reports transient instead — the same
    /// policy as an off-band Start or Widgets rect — and the next watchdog
    /// pass retries with fresh rects.
    #[test]
    fn tray_rect_failing_the_band_check_is_transient() {
        // Off-band: entirely above the taskbar's top edge.
        let (taskbar, landmarks) = left_aligned_taskbar(Some(Rect {
            left: 1700,
            top: 0,
            right: 1920,
            bottom: 40,
        }));

        assert_eq!(
            placement_outcome(&taskbar, landmarks, 3),
            PlacementOutcome::TransientLandmarks
        );
    }

    /// Stock Windows 11 with "Taskbar alignment = Left" AND Windows Widgets
    /// enabled (the default): Windows renders the Widgets entry by the
    /// tray, so UIA reports it right of Start — permanent geometry, not a
    /// mid-animation state. The old guard returned TransientLandmarks here
    /// forever, freezing a stale widget in place. Now lane 1 is simply
    /// absent, lane 2 places the widget, and the Widgets entry joins the
    /// obstacle set so the verified gap ends before it.
    #[test]
    fn widgets_rendered_by_the_tray_becomes_a_lane_two_obstacle() {
        let (taskbar, mut landmarks) = left_aligned_taskbar(Some(Rect {
            left: 1700,
            top: 1032,
            right: 1920,
            bottom: 1080,
        }));
        landmarks.widgets = Some(Rect {
            left: 1600,
            top: 1032,
            right: 1690,
            bottom: 1080,
        });

        let placement = child_placement(&taskbar, landmarks, 3)
            .expect("lane 2 should fit between the icons and the Widgets entry");
        // Largest verified gap is (708, 1600): after the icon row, ending at
        // the Widgets entry — not at the tray. Centered 312px within it.
        assert_eq!(placement.x, 998);
        assert_eq!(placement.width, 312);
        assert!(placement.x + placement.width <= 1600);
    }

    /// An off-band Widgets rect is still stale-landmark territory — the
    /// transient policy is unchanged for genuinely garbage rects.
    #[test]
    fn off_band_widgets_rect_is_still_transient() {
        let (taskbar, mut landmarks) = left_aligned_taskbar(None);
        landmarks.widgets = Some(Rect {
            left: 0,
            top: 0,
            right: 48,
            bottom: 40,
        });

        assert_eq!(
            placement_outcome(&taskbar, landmarks, 3),
            PlacementOutcome::TransientLandmarks
        );
    }

    /// Centered alignment (today's common case): lane 1 has ample room and a
    /// tray landmark is also present with room to spare in lane 2. Lane 1
    /// must still win — placement is pixel-identical to a world with no
    /// tray landmark at all.
    #[test]
    fn centered_alignment_still_prefers_lane_one_even_when_lane_two_would_fit() {
        let taskbar = layout(
            Rect {
                left: 0,
                top: 1032,
                right: 1920,
                bottom: 1080,
            },
            vec![],
        );
        let landmarks = TaskbarLandmarks {
            widgets: Some(Rect {
                left: 0,
                top: 1032,
                right: 160,
                bottom: 1080,
            }),
            start: Some(Rect {
                left: 960,
                top: 1032,
                right: 1008,
                bottom: 1080,
            }),
            tray: Some(Rect {
                left: 1700,
                top: 1032,
                right: 1920,
                bottom: 1080,
            }),
        };

        let placement = child_placement(&taskbar, landmarks, 3).expect("lane 1 has plenty of room");
        assert_eq!(placement.x, 404);
        assert_eq!(placement.width, 312);
    }

    /// Lane 1 has a non-negative but too-small gap (a crowded centered
    /// taskbar, not the left-aligned #261 case) — falls through to lane 2.
    #[test]
    fn lane_one_too_small_but_non_negative_falls_through_to_lane_two() {
        let taskbar = layout(
            Rect {
                left: 0,
                top: 1032,
                right: 1920,
                bottom: 1080,
            },
            vec![],
        );
        let landmarks = TaskbarLandmarks {
            widgets: Some(Rect {
                left: 0,
                top: 1032,
                right: 780,
                bottom: 1080,
            }),
            start: Some(Rect {
                left: 800,
                top: 1032,
                right: 848,
                bottom: 1080,
            }),
            tray: Some(Rect {
                left: 1700,
                top: 1032,
                right: 1920,
                bottom: 1080,
            }),
        };

        // Lane 1 is (788, 792) -- 4px wide, well under the 216px minimum for
        // 3 providers -- non-negative but too small, unlike the left-aligned
        // #261 case where lane 1's right edge goes negative.
        let placement = child_placement(&taskbar, landmarks, 3).expect("lane 2 should fit");
        assert_eq!(placement.x, 1118);
        assert_eq!(placement.width, 312);
    }

    /// Lane 2 fully obstructed (an icon row spans the whole taskbar) →
    /// neither lane has a verified gap, so the widget hides.
    #[test]
    fn fully_obstructed_lane_two_still_reports_verified_no_fit() {
        let taskbar = layout(
            Rect {
                left: 0,
                top: 1032,
                right: 1920,
                bottom: 1080,
            },
            vec![Rect {
                left: 0,
                top: 1032,
                right: 1920,
                bottom: 1080,
            }],
        );
        let landmarks = TaskbarLandmarks {
            widgets: None,
            start: Some(Rect {
                left: 0,
                top: 1032,
                right: 48,
                bottom: 1080,
            }),
            tray: Some(Rect {
                left: 1700,
                top: 1032,
                right: 1920,
                bottom: 1080,
            }),
        };

        assert_eq!(
            placement_outcome(&taskbar, landmarks, 3),
            PlacementOutcome::VerifiedNoFit
        );
    }

    /// A vertical taskbar rejects placement before either lane (or the tray
    /// landmark) is ever consulted.
    #[test]
    fn vertical_taskbar_early_return_is_unaffected_by_a_tray_landmark() {
        let taskbar = layout(
            Rect {
                left: 0,
                top: 0,
                right: 48,
                bottom: 1080,
            },
            vec![],
        );
        let landmarks = TaskbarLandmarks {
            widgets: Some(Rect {
                left: 0,
                top: 0,
                right: 48,
                bottom: 60,
            }),
            start: Some(Rect {
                left: 0,
                top: 500,
                right: 48,
                bottom: 548,
            }),
            tray: Some(Rect {
                left: 0,
                top: 1000,
                right: 48,
                bottom: 1080,
            }),
        };

        assert_eq!(
            placement_outcome(&taskbar, landmarks, 3),
            PlacementOutcome::VerifiedNoFit
        );
    }

    /// Multi-monitor: both taskbars are left-aligned (lane 2), and each must
    /// anchor against its own tray rect, not the other's.
    #[test]
    fn multi_monitor_lane_two_uses_each_taskbars_own_tray_rect() {
        let primary = TaskbarLayout {
            window_handle: 1,
            landmarks: TaskbarLandmarks {
                widgets: None,
                start: Some(Rect {
                    left: 0,
                    top: 1392,
                    right: 48,
                    bottom: 1440,
                }),
                tray: Some(Rect {
                    left: 2400,
                    top: 1392,
                    right: 2560,
                    bottom: 1440,
                }),
            },
            ..layout(
                Rect {
                    left: 0,
                    top: 1392,
                    right: 2560,
                    bottom: 1440,
                },
                Vec::new(),
            )
        };
        let secondary_bounds = Rect {
            left: -1920,
            top: 1032,
            right: 0,
            bottom: 1080,
        };
        let secondary = TaskbarLayout {
            window_handle: 2,
            primary: false,
            landmarks: TaskbarLandmarks {
                widgets: None,
                start: Some(Rect {
                    left: -1920,
                    top: 1032,
                    right: -1872,
                    bottom: 1080,
                }),
                tray: Some(Rect {
                    left: -260,
                    top: 1032,
                    right: 0,
                    bottom: 1080,
                }),
            },
            ..layout(secondary_bounds, Vec::new())
        };

        let (discovered, rejected, placements) = taskbar_placements(&[primary, secondary], true, 3);
        assert_eq!(discovered, vec![1, 2]);
        assert!(rejected.is_empty());
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].0, 1);
        assert_eq!(placements[0].1.x, 1068);
        assert_eq!(placements[1].0, 2);
        assert_eq!(placements[1].1.x, 698);
        assert!(
            placements
                .iter()
                .all(|(_, placement)| placement.width == 312 && placement.height == 48)
        );
    }

    fn rate_window(used: f64, minutes: Option<u32>) -> crate::commands::RateWindowSnapshot {
        crate::commands::RateWindowSnapshot {
            used_percent: used,
            remaining_percent: 100.0 - used,
            window_minutes: minutes,
            resets_at: None,
            reset_description: None,
            is_exhausted: used >= 100.0,
            reserve_percent: None,
            reserve_description: None,
            reserve_will_last_to_reset: false,
            reserve_eta_seconds: None,
        }
    }

    fn snap(
        provider_id: &str,
        account_id: Option<&str>,
        used: f64,
    ) -> crate::commands::ProviderUsageSnapshot {
        crate::commands::ProviderUsageSnapshot {
            provider_id: provider_id.into(),
            display_name: provider_id.into(),
            primary: rate_window(used, Some(300)),
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
            account_email: None,
            source_label: "test".into(),
            updated_at: String::new(),
            error: None,
            pace: None,
            account_organization: None,
            tray_status_label: None,
            account_id: account_id.map(str::to_string),
            account_label: account_id.map(str::to_string),
            account_tint: None,
            fetch_duration_ms: None,
            wayfinder_usage: None,
        }
    }

    #[test]
    fn strip_snapshot_defaults_to_hottest_account() {
        let cache = [
            snap("codex", Some("personal"), 20.0),
            snap("codex", Some("work"), 80.0),
        ];
        let picked = select_strip_snapshot(
            cache.iter(),
            "codex",
            None,
            codexbar::settings::MetricPreference::Automatic,
        )
        .unwrap();
        assert_eq!(picked.account_id.as_deref(), Some("work"));
    }

    /// Claude snapshot with independent Session (primary) and Weekly
    /// (secondary) readings, for Glance metric tests.
    fn claude_session_weekly_snap(
        account_id: Option<&str>,
        session: f64,
        weekly: f64,
    ) -> crate::commands::ProviderUsageSnapshot {
        let mut snapshot = snap("claude", account_id, session);
        snapshot.secondary = Some(rate_window(weekly, Some(10_080)));
        snapshot.secondary_label = Some("Weekly".into());
        snapshot
    }

    /// #411: a pinned Glance metric must drive the tile. Session shows the
    /// primary window even when the constraining ranking would pick Weekly.
    #[test]
    fn glance_metric_session_pins_the_primary_window() {
        let snapshot = claude_session_weekly_snap(Some("personal"), 10.0, 95.0);
        let readout =
            super::preferred_readout(&snapshot, codexbar::settings::MetricPreference::Session);
        assert_eq!(readout.label, Some("Session"));
        assert_eq!(readout.window.used_percent, 10.0);
    }

    #[test]
    fn glance_metric_weekly_pins_the_secondary_window() {
        let snapshot = claude_session_weekly_snap(Some("personal"), 80.0, 5.0);
        let readout =
            super::preferred_readout(&snapshot, codexbar::settings::MetricPreference::Weekly);
        assert_eq!(readout.label, Some("Weekly"));
        assert_eq!(readout.window.used_percent, 5.0);
    }

    /// A pinned metric falls back to the automatic choice when the provider
    /// does not report that lane, so the tile never goes blank.
    #[test]
    fn glance_metric_falls_back_when_the_lane_is_missing() {
        let snapshot = snap("codex", Some("personal"), 42.0);
        let readout =
            super::preferred_readout(&snapshot, codexbar::settings::MetricPreference::Weekly);
        assert_eq!(readout.window.used_percent, 42.0);
    }

    /// Account ranking uses the pinned window too: with Session pinned, the
    /// account that is hottest on Session wins even when the other seat's
    /// Weekly is hotter.
    #[test]
    fn strip_ranking_uses_the_pinned_window() {
        let cache = [
            claude_session_weekly_snap(Some("a"), 10.0, 90.0),
            claude_session_weekly_snap(Some("b"), 80.0, 5.0),
        ];
        let automatic = select_strip_snapshot(
            cache.iter(),
            "claude",
            None,
            codexbar::settings::MetricPreference::Automatic,
        )
        .unwrap();
        assert_eq!(automatic.account_id.as_deref(), Some("a"));
        let session = select_strip_snapshot(
            cache.iter(),
            "claude",
            None,
            codexbar::settings::MetricPreference::Session,
        )
        .unwrap();
        assert_eq!(session.account_id.as_deref(), Some("b"));
    }

    /// SBS-876: ranking happens before `widget_model` filters errored
    /// snapshots. A failed account's primary reads 0%, which used to outrank a
    /// successful account whose Plan is unavailable (heat -1), so the tile was
    /// handed a snapshot it then refused to read and painted an em dash - while
    /// the other account could have said "Unavailable".
    #[test]
    fn strip_snapshot_prefers_a_named_state_over_a_failed_account() {
        let mut unavailable = snap("cursor", Some("good"), 0.0);
        unavailable.primary_label = Some("Plan".into());
        unavailable
            .inactive_rate_windows
            .push(crate::commands::InactiveRateWindowSnapshot {
                id: "cursor-plan".into(),
                title: "Plan".into(),
                description: "No usage reported".into(),
                state: "unavailable".into(),
            });
        let mut failed = snap("cursor", Some("broken"), 0.0);
        failed.error = Some("network timeout".into());

        let cache = [failed, unavailable];
        let picked = select_strip_snapshot(
            cache.iter(),
            "cursor",
            None,
            codexbar::settings::MetricPreference::Automatic,
        )
        .unwrap();

        assert_eq!(picked.account_id.as_deref(), Some("good"));
        assert!(picked.error.is_none());

        // A real reading still beats both.
        let mut cache = cache.to_vec();
        cache.push(snap("cursor", Some("hot"), 42.0));
        let picked = select_strip_snapshot(
            cache.iter(),
            "cursor",
            None,
            codexbar::settings::MetricPreference::Automatic,
        )
        .unwrap();
        assert_eq!(picked.account_id.as_deref(), Some("hot"));
    }

    #[test]
    fn strip_snapshot_respects_pinned_account() {
        let cache = [
            snap("codex", Some("personal"), 20.0),
            snap("codex", Some("work"), 80.0),
        ];
        let picked = select_strip_snapshot(
            cache.iter(),
            "codex",
            Some("personal"),
            codexbar::settings::MetricPreference::Automatic,
        )
        .unwrap();
        assert_eq!(picked.account_id.as_deref(), Some("personal"));
    }

    #[test]
    fn strip_snapshot_falls_back_to_hottest_when_pin_missing() {
        let cache = [
            snap("codex", Some("personal"), 20.0),
            snap("codex", Some("work"), 80.0),
        ];
        let picked = select_strip_snapshot(
            cache.iter(),
            "codex",
            Some("gone"),
            codexbar::settings::MetricPreference::Automatic,
        )
        .unwrap();
        assert_eq!(picked.account_id.as_deref(), Some("work"));
    }

    #[test]
    fn constraining_readout_surfaces_hot_weekly_over_fresh_session() {
        // SOU-288 / taskbar: Claude session at 0% used must not hide a maxed weekly.
        let mut snapshot = snap("claude", None, 0.0);
        snapshot.primary_label = Some("Session (5h)".into());
        snapshot.secondary = Some(rate_window(100.0, Some(10_080)));
        snapshot.secondary_label = Some("Weekly".into());

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("Weekly"));
        assert_eq!(readout.window.used_percent, 100.0);
        assert_eq!(readout.window.window_minutes, Some(10_080));
    }

    #[test]
    fn constraining_readout_keeps_session_when_it_is_hotter() {
        let mut snapshot = snap("claude", None, 92.0);
        snapshot.primary_label = Some("Session (5h)".into());
        snapshot.secondary = Some(rate_window(40.0, Some(10_080)));
        snapshot.secondary_label = Some("Weekly".into());

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("Session (5h)"));
        assert_eq!(readout.window.used_percent, 92.0);
    }

    /// The reported taskbar symptom: a maxed Claude per-model cap took the
    /// whole tile and hid a Session and Weekly that still had capacity. This
    /// mirrors `keeps a maxed model-scoped lane off the strip` in
    /// `capacityPresentation.test.ts` — the two selectors must not drift.
    #[test]
    fn constraining_readout_keeps_claude_model_caps_off_the_tile() {
        let mut snapshot = snap("claude", None, 34.0);
        snapshot.primary_label = Some("Session (5h)".into());
        snapshot.secondary = Some(rate_window(12.0, Some(10_080)));
        snapshot.secondary_label = Some("Weekly".into());
        // Both shapes Claude reports a per-model cap in.
        snapshot.model_specific = Some(rate_window(100.0, Some(10_080)));
        snapshot.extra_rate_windows = vec![crate::commands::NamedRateWindowSnapshot {
            id: "claude-weekly-scoped-fable".into(),
            title: "Fable only".into(),
            window: rate_window(100.0, Some(10_080)),
            amount: None,
        }];

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("Session (5h)"));
        assert_eq!(readout.window.used_percent, 34.0);
    }

    /// Claude-only. Other providers put real pools in `model_specific` (Codex
    /// code review, Gemini Pro quota) and those must still bind the tile.
    #[test]
    fn constraining_readout_still_ranks_model_windows_for_other_providers() {
        let mut snapshot = snap("codex", None, 20.0);
        snapshot.model_specific = Some(rate_window(90.0, Some(10_080)));

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("Model"));
        assert_eq!(readout.window.used_percent, 90.0);
    }

    /// Account selection ranks by the same readout, so a maxed model cap must
    /// not decide which Claude seat owns the single tile either.
    #[test]
    fn strip_snapshot_ignores_claude_model_caps_when_ranking_accounts() {
        let mut quiet_but_fable_maxed = snap("claude", Some("personal"), 30.0);
        quiet_but_fable_maxed.extra_rate_windows = vec![crate::commands::NamedRateWindowSnapshot {
            id: "claude-weekly-scoped-fable".into(),
            title: "Fable only".into(),
            window: rate_window(100.0, Some(10_080)),
            amount: None,
        }];
        let genuinely_hot = snap("claude", Some("work"), 80.0);

        let cache = [quiet_but_fable_maxed, genuinely_hot];
        let picked = select_strip_snapshot(
            cache.iter(),
            "claude",
            None,
            codexbar::settings::MetricPreference::Automatic,
        )
        .unwrap();
        assert_eq!(picked.account_id.as_deref(), Some("work"));
    }

    #[test]
    fn constraining_readout_uses_tertiary_label_when_it_binds() {
        // OpenCode Go monthly bar: the tertiary window's own label must show on
        // the taskbar, not the generic "Extra".
        let mut snapshot = snap("opencodego", None, 10.0);
        snapshot.primary_label = Some("Rolling".into());
        snapshot.tertiary = Some(rate_window(100.0, Some(43_200)));
        snapshot.tertiary_label = Some("Monthly".into());

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("Monthly"));
        assert_eq!(readout.window.used_percent, 100.0);
    }

    #[test]
    fn constraining_readout_falls_back_to_extra_for_unnamed_tertiary() {
        let mut snapshot = snap("opencodego", None, 10.0);
        snapshot.primary_label = Some("Rolling".into());
        snapshot.tertiary = Some(rate_window(100.0, Some(43_200)));
        snapshot.tertiary_label = None;

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("Extra"));
        assert_eq!(readout.window.used_percent, 100.0);
    }

    #[test]
    fn cursor_strip_prefers_auto_when_api_is_maxed() {
        // Parallel pools: maxed API must not hide Auto that still has room.
        let mut snapshot = snap("cursor", None, 40.0);
        snapshot.primary_label = Some("Monthly".into());
        snapshot.secondary = Some(rate_window(60.0, Some(10_080)));
        snapshot.secondary_label = Some("Auto".into());
        snapshot
            .extra_rate_windows
            .push(crate::commands::NamedRateWindowSnapshot {
                id: "cursor-api".into(),
                title: "API".into(),
                window: rate_window(100.0, Some(10_080)),
                amount: None,
            });

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("Auto"));
        assert_eq!(readout.window.used_percent, 60.0);
    }

    #[test]
    fn cursor_strip_prefers_api_when_auto_is_maxed() {
        let mut snapshot = snap("cursor", None, 40.0);
        snapshot.primary_label = Some("Monthly".into());
        snapshot.secondary = Some(rate_window(100.0, Some(10_080)));
        snapshot.secondary_label = Some("Auto".into());
        snapshot
            .extra_rate_windows
            .push(crate::commands::NamedRateWindowSnapshot {
                id: "cursor-api".into(),
                title: "API".into(),
                window: rate_window(40.0, Some(10_080)),
                amount: None,
            });

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("API"));
        assert_eq!(readout.window.used_percent, 40.0);
    }

    #[test]
    fn cursor_strip_trims_labels_and_falls_back_when_blank() {
        let mut snapshot = snap("cursor", None, 40.0);
        snapshot.secondary = Some(rate_window(60.0, Some(10_080)));
        snapshot.secondary_label = Some("  Auto Custom  ".into());
        snapshot
            .extra_rate_windows
            .push(crate::commands::NamedRateWindowSnapshot {
                id: "cursor-api".into(),
                title: "   ".into(),
                window: rate_window(70.0, Some(10_080)),
                amount: None,
            });

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("API"));

        snapshot.extra_rate_windows.clear();
        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("Auto Custom"));

        snapshot.secondary_label = Some("   ".into());
        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("Auto"));
    }

    #[test]
    fn cursor_strip_ignores_hotter_plan_when_auto_api_exist() {
        let mut snapshot = snap("cursor", None, 95.0);
        snapshot.primary_label = Some("Monthly".into());
        snapshot.secondary = Some(rate_window(55.0, Some(10_080)));
        snapshot.secondary_label = Some("Auto".into());
        snapshot
            .extra_rate_windows
            .push(crate::commands::NamedRateWindowSnapshot {
                id: "cursor-api".into(),
                title: "API".into(),
                window: rate_window(30.0, Some(10_080)),
                amount: None,
            });

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("Auto"));
        assert_eq!(readout.window.used_percent, 55.0);
    }

    #[test]
    fn cursor_strip_picks_soonest_reset_when_both_exhausted() {
        let mut soon = rate_window(100.0, Some(10_080));
        soon.resets_at = Some("2026-07-21T04:00:00Z".into());
        let mut later = rate_window(100.0, Some(10_080));
        later.resets_at = Some("2026-07-28T04:00:00Z".into());

        let mut snapshot = snap("cursor", None, 50.0);
        snapshot.primary_label = Some("Monthly".into());
        snapshot.secondary = Some(later);
        snapshot.secondary_label = Some("Auto".into());
        snapshot
            .extra_rate_windows
            .push(crate::commands::NamedRateWindowSnapshot {
                id: "cursor-api".into(),
                title: "API".into(),
                window: soon,
                amount: None,
            });

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("API"));
        assert_eq!(readout.window.used_percent, 100.0);
    }

    #[test]
    fn cursor_strip_surfaces_on_demand_after_included_usage_is_exhausted() {
        let mut snapshot = snap("cursor", None, 100.0);
        snapshot.primary_label = Some("Plan".into());
        snapshot.secondary = Some(rate_window(100.0, Some(43_200)));
        snapshot.secondary_label = Some("Auto".into());
        snapshot
            .extra_rate_windows
            .push(crate::commands::NamedRateWindowSnapshot {
                id: "cursor-api".into(),
                title: "API".into(),
                window: rate_window(100.0, Some(43_200)),
                amount: None,
            });
        snapshot
            .extra_rate_windows
            .push(crate::commands::NamedRateWindowSnapshot {
                id: "cursor-on-demand".into(),
                title: "On-demand".into(),
                window: rate_window(56.0, Some(43_200)),
                amount: Some(crate::commands::WindowAmountBridge {
                    used: 1002.16,
                    limit: Some(1800.0),
                    currency_code: "USD".into(),
                    formatted_used: "$1,002.16".into(),
                    formatted_limit: Some("$1,800.00".into()),
                }),
            });

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("On-demand"));
        assert_eq!(readout.window.used_percent, 56.0);
    }

    #[test]
    fn cursor_strip_surfaces_zero_spend_when_actionable_lanes_are_exhausted() {
        let mut snapshot = snap("cursor", None, 50.0);
        snapshot.primary_label = Some("Plan".into());
        snapshot.secondary = Some(rate_window(100.0, Some(43_200)));
        snapshot.secondary_label = Some("Auto".into());
        snapshot
            .extra_rate_windows
            .push(crate::commands::NamedRateWindowSnapshot {
                id: "cursor-api".into(),
                title: "API".into(),
                window: rate_window(100.0, Some(43_200)),
                amount: None,
            });
        snapshot
            .extra_rate_windows
            .push(crate::commands::NamedRateWindowSnapshot {
                id: "cursor-on-demand".into(),
                title: "On-demand".into(),
                window: rate_window(0.0, Some(43_200)),
                amount: Some(crate::commands::WindowAmountBridge {
                    used: 0.0,
                    limit: Some(1800.0),
                    currency_code: "USD".into(),
                    formatted_used: "$0.00".into(),
                    formatted_limit: Some("$1,800.00".into()),
                }),
            });

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.label, Some("On-demand"));
        assert_eq!(readout.window.used_percent, 0.0);
    }

    /// SBS-191: picking the on-demand lane was never the hard part — carrying
    /// its money to the tile was. The readout dropped `amount` on the floor, so
    /// the strip could only ever render "62%".
    #[test]
    fn cursor_strip_readout_carries_on_demand_money() {
        let mut snapshot = snap("cursor", None, 100.0);
        snapshot.primary_label = Some("Plan".into());
        snapshot.secondary = Some(rate_window(100.0, Some(43_200)));
        snapshot.secondary_label = Some("Auto".into());
        snapshot
            .extra_rate_windows
            .push(crate::commands::NamedRateWindowSnapshot {
                id: "cursor-api".into(),
                title: "API".into(),
                window: rate_window(100.0, Some(43_200)),
                amount: None,
            });
        snapshot
            .extra_rate_windows
            .push(crate::commands::NamedRateWindowSnapshot {
                id: "cursor-on-demand".into(),
                title: "On-demand".into(),
                window: rate_window(62.0, Some(43_200)),
                amount: Some(crate::commands::WindowAmountBridge {
                    used: 1112.92,
                    limit: Some(1800.0),
                    currency_code: "USD".into(),
                    formatted_used: "$1112.92".into(),
                    formatted_limit: Some("$1800.00".into()),
                }),
            });

        let readout = constraining_readout(&snapshot);
        let amount = readout.amount.expect("on-demand money reaches the tile");
        assert_eq!(
            strip_amount_label(amount, true).as_deref(),
            Some("$1112.92")
        );
        // "Show remaining" on a capped spend lane is headroom, not spend.
        assert_eq!(
            strip_amount_label(amount, false).as_deref(),
            Some("$687.08")
        );
    }

    /// SBS-876: Cursor still writes 0% primary when monthly is missing, plus
    /// `cursor-plan` unavailable. The native tile must not round that
    /// placeholder to Some(0) or Some(100).
    #[test]
    fn cursor_strip_omits_percent_when_plan_is_unavailable() {
        let mut snapshot = snap("cursor", None, 0.0);
        snapshot.primary_label = Some("Plan".into());
        snapshot
            .inactive_rate_windows
            .push(crate::commands::InactiveRateWindowSnapshot {
                id: "cursor-plan".into(),
                title: "Plan".into(),
                description: "No usage reported".into(),
                state: "unavailable".into(),
            });

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.named_state, Some("unavailable"));
        assert_eq!(strip_readout_percent(&readout, true), None);
        assert_eq!(strip_readout_percent(&readout, false), None);

        // Auto still wins the strip when present; the placeholder does not.
        snapshot.secondary = Some(rate_window(42.0, Some(43_200)));
        snapshot.secondary_label = Some("Auto".into());
        let with_auto = constraining_readout(&snapshot);
        assert_eq!(with_auto.label, Some("Auto"));
        assert!(with_auto.named_state.is_none());
        assert_eq!(strip_readout_percent(&with_auto, true), Some(42));
    }

    /// SBS-1040: Claude writes 0% primary when five_hour/utilization is
    /// missing, plus `claude-session` unavailable. The tile must not paint 0%.
    #[test]
    fn claude_strip_omits_percent_when_session_is_unavailable() {
        let mut snapshot = snap("claude", None, 0.0);
        snapshot.primary_label = Some("Session (5h)".into());
        snapshot
            .inactive_rate_windows
            .push(crate::commands::InactiveRateWindowSnapshot {
                id: "claude-session".into(),
                title: "Session (5h)".into(),
                description: "No usage reported".into(),
                state: "unavailable".into(),
            });

        let readout = constraining_readout(&snapshot);
        assert_eq!(readout.named_state, Some("unavailable"));
        assert_eq!(strip_readout_percent(&readout, true), None);
        assert_eq!(strip_readout_percent(&readout, false), None);

        snapshot.secondary = Some(rate_window(23.0, Some(10_080)));
        snapshot.secondary_label = Some("Weekly".into());
        let with_weekly = constraining_readout(&snapshot);
        assert_eq!(with_weekly.label, Some("Weekly"));
        assert!(with_weekly.named_state.is_none());
        assert_eq!(strip_readout_percent(&with_weekly, true), Some(23));
    }

    /// SBS-876: omitting the percent is only half the job. Without a label the
    /// tile falls through to the em dash, which is what a fetch error paints —
    /// the user cannot tell "no reading exists" from "the fetch broke".
    #[test]
    fn cursor_strip_labels_the_named_state_instead_of_an_em_dash() {
        let mut snapshot = snap("cursor", None, 0.0);
        snapshot.primary_label = Some("Plan".into());
        snapshot.primary.resets_at = Some("2099-01-01T00:00:00Z".into());
        snapshot
            .inactive_rate_windows
            .push(crate::commands::InactiveRateWindowSnapshot {
                id: "cursor-plan".into(),
                title: "Plan".into(),
                description: "No usage reported".into(),
                state: "unavailable".into(),
            });

        let readout = constraining_readout(&snapshot);
        let lang = codexbar::settings::Language::default();
        assert_eq!(
            strip_named_label(&readout, lang).as_deref(),
            Some("Unavailable")
        );

        // A lifted limit is a different sentence from a missing reading.
        snapshot.inactive_rate_windows[0].state = "notEnforced".into();
        let lifted = constraining_readout(&snapshot);
        assert_eq!(
            strip_named_label(&lifted, lang).as_deref(),
            Some("Not currently enforced")
        );

        // A real reading has no named label to paint.
        snapshot.inactive_rate_windows.clear();
        assert!(strip_named_label(&constraining_readout(&snapshot), lang).is_none());
    }

    /// SBS-876: five providers leave roughly 51px for a tile headline, which
    /// "Unavailable" (~72px at the 14px tile font) overruns. Without a short
    /// spelling the ladder reaches the em dash and the named state reads as a
    /// fetch error on exactly the strips that are busiest.
    #[test]
    fn named_state_has_a_spelling_that_fits_a_crowded_tile() {
        let mut snapshot = snap("cursor", None, 0.0);
        snapshot.primary_label = Some("Plan".into());
        snapshot
            .inactive_rate_windows
            .push(crate::commands::InactiveRateWindowSnapshot {
                id: "cursor-plan".into(),
                title: "Plan".into(),
                description: "No usage reported".into(),
                state: "unavailable".into(),
            });
        let lang = codexbar::settings::Language::default();

        for state in ["unavailable", "notEnforced"] {
            snapshot.inactive_rate_windows[0].state = state.into();
            let readout = constraining_readout(&snapshot);
            let full = strip_named_label(&readout, lang).expect("full spelling");
            let compact = compact_named_label(&readout, lang).expect("compact spelling");
            assert!(
                compact.chars().count() < full.chars().count(),
                "{state}: compact {compact:?} must be shorter than {full:?}"
            );
            // The narrowest tile budget fits about 8 characters of the headline
            // font. Anything longer lands back on the em dash.
            assert!(
                compact.chars().count() <= 8,
                "{state}: {compact:?} is too wide for a five-provider strip"
            );
        }

        // A real reading still has no named spelling at either width.
        snapshot.inactive_rate_windows.clear();
        let real = constraining_readout(&snapshot);
        assert!(compact_named_label(&real, lang).is_none());
    }

    /// SBS-876: the billing-cycle date on a placeholder Plan is not a countdown,
    /// so the tile must not print it beside the named state.
    #[test]
    fn cursor_strip_omits_reset_when_plan_is_unavailable() {
        let mut snapshot = snap("cursor", None, 0.0);
        snapshot.primary_label = Some("Plan".into());
        snapshot.primary.reset_description = Some("Resets monthly".into());
        snapshot
            .inactive_rate_windows
            .push(crate::commands::InactiveRateWindowSnapshot {
                id: "cursor-plan".into(),
                title: "Plan".into(),
                description: "No usage reported".into(),
                state: "unavailable".into(),
            });

        assert_eq!(
            strip_reset_label(&constraining_readout(&snapshot), true),
            None
        );

        // A real reading still shows its reset when the setting is on.
        snapshot.inactive_rate_windows.clear();
        assert!(strip_reset_label(&constraining_readout(&snapshot), true).is_some());
        assert_eq!(
            strip_reset_label(&constraining_readout(&snapshot), false),
            None
        );
    }

    #[test]
    fn uncapped_spend_reports_spend_even_in_remaining_mode() {
        // No denominator means no headroom figure exists. Falling back to
        // spend-to-date beats blanking the tile for exactly the people running
        // on-demand without a cap.
        let uncapped = crate::commands::WindowAmountBridge {
            used: 42.5,
            limit: None,
            currency_code: "USD".into(),
            formatted_used: "$42.50".into(),
            formatted_limit: None,
        };
        assert_eq!(
            strip_amount_label(&uncapped, false).as_deref(),
            Some("$42.50")
        );
    }

    #[test]
    fn compact_spend_drops_cents_for_narrow_tiles() {
        let capped = crate::commands::WindowAmountBridge {
            used: 1112.92,
            limit: Some(1800.0),
            currency_code: "USD".into(),
            formatted_used: "$1112.92".into(),
            formatted_limit: Some("$1800.00".into()),
        };
        // Three characters narrower than "$1112.92", which is the difference
        // between fitting a 72px tile and crossing into the next provider.
        assert_eq!(
            compact_amount_label(&capped, true).as_deref(),
            Some("$1113")
        );
        assert_eq!(
            compact_amount_label(&capped, false).as_deref(),
            Some("$687")
        );

        let uncapped = crate::commands::WindowAmountBridge {
            used: 42.5,
            limit: None,
            currency_code: "EUR".into(),
            formatted_used: "€42.50".into(),
            formatted_limit: None,
        };
        assert_eq!(
            compact_amount_label(&uncapped, false).as_deref(),
            Some("€43")
        );
    }

    #[test]
    fn percentage_lanes_leave_the_tile_on_percent() {
        let snapshot = snap("claude", None, 41.0);
        let readout = constraining_readout(&snapshot);
        assert!(readout.amount.is_none());
    }

    #[test]
    fn strip_snapshot_ranks_accounts_by_constraining_window() {
        // Account A: calm primary, maxed weekly. Account B: busy primary, calm weekly.
        // Strip should pick A because weekly is the real constraint.
        let mut calm_session = snap("claude", Some("a"), 5.0);
        calm_session.secondary = Some(rate_window(100.0, Some(10_080)));
        calm_session.secondary_label = Some("Weekly".into());

        let mut busy_session = snap("claude", Some("b"), 70.0);
        busy_session.secondary = Some(rate_window(20.0, Some(10_080)));
        busy_session.secondary_label = Some("Weekly".into());

        let cache = [calm_session, busy_session];
        let picked = select_strip_snapshot(
            cache.iter(),
            "claude",
            None,
            codexbar::settings::MetricPreference::Automatic,
        )
        .unwrap();
        assert_eq!(picked.account_id.as_deref(), Some("a"));
    }

    fn prepared_widgets(taskbar_count: usize, rejected_count: usize) -> PreparedWidgets {
        PreparedWidgets {
            widgets: (0..taskbar_count)
                .map(|index| PreparedWidget {
                    taskbar: index as isize + 1,
                    placement: ChildPlacement {
                        x: 0,
                        y: 0,
                        width: 104,
                        height: 48,
                    },
                })
                .collect(),
            rejected_taskbars: (0..rejected_count)
                .map(|index| index as isize + 100)
                .collect(),
            all_monitors: false,
            model: WidgetModel::default(),
        }
    }

    #[test]
    fn status_from_preparation_reports_active_with_the_taskbar_count() {
        let result: Result<PreparedWidgets, PrepareFailure> = Ok(prepared_widgets(2, 0));
        assert_eq!(
            status_from_preparation(&result),
            Some(TaskbarWidgetStatus::Active { taskbars: 2 })
        );
    }

    #[test]
    fn status_from_preparation_reports_no_fit_when_every_taskbar_is_rejected() {
        let result: Result<PreparedWidgets, PrepareFailure> = Ok(prepared_widgets(0, 1));
        assert_eq!(
            status_from_preparation(&result),
            Some(TaskbarWidgetStatus::NoFit)
        );
    }

    #[test]
    fn status_from_preparation_maps_disabled_and_no_providers() {
        assert_eq!(
            status_from_preparation(&Err(PrepareFailure::Disabled)),
            Some(TaskbarWidgetStatus::Disabled)
        );
        assert_eq!(
            status_from_preparation(&Err(PrepareFailure::NoProviders)),
            Some(TaskbarWidgetStatus::NoProviders)
        );
    }

    #[test]
    fn status_from_preparation_defers_transient_landmarks_to_the_debounce() {
        assert_eq!(
            status_from_preparation(&Err(PrepareFailure::TransientLandmarks)),
            None
        );
    }

    #[test]
    fn hidden_state_reports_no_providers_while_native_mode_is_still_enabled() {
        assert_eq!(status_when_hidden(true), TaskbarWidgetStatus::NoProviders);
        assert_eq!(status_when_hidden(false), TaskbarWidgetStatus::Disabled);
    }

    #[test]
    fn debounce_waits_for_six_consecutive_transient_misses() {
        for streak in 1..TRANSIENT_LANDMARKS_DEBOUNCE {
            assert!(
                !should_report_waiting_landmarks(streak),
                "streak {streak} should not yet report WaitingLandmarks"
            );
        }
        assert!(should_report_waiting_landmarks(
            TRANSIENT_LANDMARKS_DEBOUNCE
        ));
        assert!(should_report_waiting_landmarks(
            TRANSIENT_LANDMARKS_DEBOUNCE + 1
        ));
    }
}
