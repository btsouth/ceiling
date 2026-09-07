//! Constraining-window ranking shared by MCP `get_status` and the widget
//! snapshot seat picker.
//!
//! Mirrors desktop `capacityPresentation.constrainingWindow` /
//! `cursorStripWindow`. Claude/Codex keep exhausted-first ranking across the
//! stored slots. Cursor's Auto and API are parallel pools, so Plan must not
//! outrank Auto and a maxed API must not hide Auto that still has room.

use super::{NamedRateWindow, ProviderId, RateWindow};

const CURSOR_API_ID: &str = "cursor-api";
const CURSOR_ON_DEMAND_ID: &str = "cursor-on-demand";

/// Window that actually constrains this provider — the same pick the desktop
/// strip uses for its one number.
pub fn constraining_rate_window<'a>(
    provider: ProviderId,
    primary: Option<&'a RateWindow>,
    secondary: Option<&'a RateWindow>,
    tertiary: Option<&'a RateWindow>,
    extras: &'a [NamedRateWindow],
) -> Option<&'a RateWindow> {
    if provider == ProviderId::Cursor {
        return cursor_strip_window(primary, secondary, extras);
    }
    generic_constraining_window(primary, secondary, tertiary)
}

fn generic_constraining_window<'a>(
    primary: Option<&'a RateWindow>,
    secondary: Option<&'a RateWindow>,
    tertiary: Option<&'a RateWindow>,
) -> Option<&'a RateWindow> {
    let mut best = primary;
    for candidate in [secondary, tertiary].into_iter().flatten() {
        match best {
            None => best = Some(candidate),
            Some(current) if window_outranks(candidate, current) => best = Some(candidate),
            _ => {}
        }
    }
    best
}

/// Exhausted/maxed outranks everything, then highest used %, then soonest reset.
fn window_outranks(candidate: &RateWindow, best: &RateWindow) -> bool {
    let candidate_blocking = candidate.is_exhausted();
    let best_blocking = best.is_exhausted();
    if candidate_blocking != best_blocking {
        return candidate_blocking;
    }
    match candidate.used_percent.total_cmp(&best.used_percent) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => reset_at_rank(candidate) < reset_at_rank(best),
    }
}

fn reset_at_rank(window: &RateWindow) -> i64 {
    window
        .resets_at
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(i64::MAX)
}

/// Cursor strip / taskbar readout: show **actionable remaining** capacity.
///
/// Auto and API are parallel product pools. A maxed API lane does not stop Auto,
/// so the strip prefers the hottest lane that still has room. Only when every
/// actionable lane is exhausted do we surface an exhausted bar (or on-demand).
/// Plan/Monthly never wins the strip when Auto or API is present.
fn cursor_strip_window<'a>(
    primary: Option<&'a RateWindow>,
    secondary: Option<&'a RateWindow>,
    extras: &'a [NamedRateWindow],
) -> Option<&'a RateWindow> {
    let api = extras
        .iter()
        .find(|extra| extra.id == CURSOR_API_ID)
        .map(|extra| &extra.window);
    let on_demand = extras.iter().find(|extra| extra.id == CURSOR_ON_DEMAND_ID);
    let has_on_demand_spend = on_demand
        .and_then(|extra| extra.amount.as_ref())
        .is_some_and(|amount| amount.used > 0.0);
    if has_on_demand_spend {
        return on_demand.map(|extra| &extra.window);
    }

    let actionable: Vec<&RateWindow> = [secondary, api].into_iter().flatten().collect();
    if !actionable.is_empty() {
        if let Some(with_room) = hottest_with_room(&actionable) {
            return Some(with_room);
        }
        if let Some(extra) = on_demand {
            return Some(&extra.window);
        }
        if let Some(exhausted) = soonest_exhausted(&actionable) {
            return Some(exhausted);
        }
    }

    if let Some(extra) = on_demand
        && cursor_on_demand_is_active(primary, &actionable, extra)
    {
        return Some(&extra.window);
    }

    primary
}

fn hottest_with_room<'a>(windows: &[&'a RateWindow]) -> Option<&'a RateWindow> {
    let mut best: Option<&RateWindow> = None;
    for candidate in windows.iter().copied() {
        if candidate.is_exhausted() {
            continue;
        }
        let replace = match best {
            None => true,
            Some(current) => {
                candidate.used_percent > current.used_percent
                    || (candidate.used_percent == current.used_percent
                        && reset_at_rank(candidate) < reset_at_rank(current))
            }
        };
        if replace {
            best = Some(candidate);
        }
    }
    best
}

fn soonest_exhausted<'a>(windows: &[&'a RateWindow]) -> Option<&'a RateWindow> {
    let mut best: Option<&RateWindow> = None;
    for candidate in windows.iter().copied() {
        if !candidate.is_exhausted() {
            continue;
        }
        let replace = match best {
            None => true,
            Some(current) => {
                reset_at_rank(candidate) < reset_at_rank(current)
                    || (reset_at_rank(candidate) == reset_at_rank(current)
                        && candidate.used_percent > current.used_percent)
            }
        };
        if replace {
            best = Some(candidate);
        }
    }
    best
}

fn cursor_on_demand_is_active(
    primary: Option<&RateWindow>,
    actionable: &[&RateWindow],
    on_demand: &NamedRateWindow,
) -> bool {
    if on_demand
        .amount
        .as_ref()
        .is_some_and(|amount| amount.used > 0.0)
    {
        return true;
    }
    if !actionable.is_empty() {
        return hottest_with_room(actionable).is_none();
    }
    primary.is_some_and(RateWindow::is_exhausted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{NamedRateWindow, WindowAmount};
    use chrono::{TimeZone, Utc};

    fn extra(id: &str, used: f64) -> NamedRateWindow {
        NamedRateWindow::new(id, id, RateWindow::new(used))
    }

    fn extra_with_spend(id: &str, used: f64, dollars: f64) -> NamedRateWindow {
        NamedRateWindow::new(id, id, RateWindow::new(used))
            .with_amount(WindowAmount::new(dollars, "USD").with_limit(1_800.0))
    }

    fn remaining(
        provider: ProviderId,
        primary: Option<f64>,
        secondary: Option<f64>,
        tertiary: Option<f64>,
        extras: &[NamedRateWindow],
    ) -> Option<f64> {
        let primary = primary.map(RateWindow::new);
        let secondary = secondary.map(RateWindow::new);
        let tertiary = tertiary.map(RateWindow::new);
        constraining_rate_window(
            provider,
            primary.as_ref(),
            secondary.as_ref(),
            tertiary.as_ref(),
            extras,
        )
        .map(RateWindow::remaining_percent)
    }

    /// SBS-1055: generic ranking still surfaces an exhausted Weekly.
    #[test]
    fn generic_ranking_surfaces_exhausted_weekly_over_healthy_session() {
        assert_eq!(
            remaining(ProviderId::Claude, Some(42.0), Some(100.0), None, &[]),
            Some(0.0)
        );
    }

    /// Without cursorStripWindow, generic ranking would pick Plan (5% remaining).
    #[test]
    fn cursor_prefers_auto_over_hotter_plan() {
        assert_eq!(
            remaining(ProviderId::Cursor, Some(95.0), Some(55.0), None, &[]),
            Some(45.0)
        );
    }

    /// Without extras + strip ranking, exhausted Auto would bind remaining 0.
    #[test]
    fn cursor_prefers_api_with_room_over_exhausted_auto() {
        let extras = [extra(CURSOR_API_ID, 40.0)];
        assert_eq!(
            remaining(ProviderId::Cursor, Some(40.0), Some(100.0), None, &extras),
            Some(60.0)
        );
    }

    #[test]
    fn cursor_prefers_hottest_open_api_over_auto() {
        let extras = [extra(CURSOR_API_ID, 70.0)];
        assert_eq!(
            remaining(ProviderId::Cursor, Some(90.0), Some(55.0), None, &extras),
            Some(30.0)
        );
    }

    #[test]
    fn cursor_ignores_maxed_api_when_auto_has_room() {
        let extras = [extra(CURSOR_API_ID, 100.0)];
        assert_eq!(
            remaining(ProviderId::Cursor, Some(40.0), Some(60.0), None, &extras),
            Some(40.0)
        );
    }

    #[test]
    fn cursor_surfaces_on_demand_after_included_lanes_exhaust() {
        let extras = [
            extra(CURSOR_API_ID, 100.0),
            extra_with_spend(CURSOR_ON_DEMAND_ID, 56.0, 1_002.16),
        ];
        assert_eq!(
            remaining(ProviderId::Cursor, Some(100.0), Some(100.0), None, &extras),
            Some(44.0)
        );
    }

    #[test]
    fn cursor_surfaces_zero_spend_on_demand_at_included_boundary() {
        let extras = [
            extra(CURSOR_API_ID, 100.0),
            extra_with_spend(CURSOR_ON_DEMAND_ID, 0.0, 0.0),
        ];
        assert_eq!(
            remaining(ProviderId::Cursor, Some(50.0), Some(100.0), None, &extras),
            Some(100.0)
        );
    }

    #[test]
    fn cursor_keeps_unused_on_demand_hidden_while_auto_has_room() {
        let extras = [
            extra(CURSOR_API_ID, 100.0),
            extra_with_spend(CURSOR_ON_DEMAND_ID, 0.0, 0.0),
        ];
        assert_eq!(
            remaining(ProviderId::Cursor, Some(100.0), Some(20.0), None, &extras),
            Some(80.0)
        );
    }

    #[test]
    fn cursor_falls_back_to_plan_when_auto_and_api_are_absent() {
        assert_eq!(
            remaining(ProviderId::Cursor, Some(42.0), None, None, &[]),
            Some(58.0)
        );
    }

    #[test]
    fn cursor_picks_soonest_reset_when_auto_and_api_are_exhausted() {
        let soon = Utc.with_ymd_and_hms(2026, 7, 21, 4, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 7, 28, 4, 0, 0).unwrap();
        let auto = RateWindow::with_details(100.0, Some(10_080), Some(later), None);
        let api = NamedRateWindow::new(
            CURSOR_API_ID,
            "API",
            RateWindow::with_details(100.0, Some(10_080), Some(soon), None),
        );
        let plan = RateWindow::new(50.0);
        let extras = [api];
        let window =
            constraining_rate_window(ProviderId::Cursor, Some(&plan), Some(&auto), None, &extras)
                .expect("window");
        assert_eq!(window.window_minutes, Some(10_080));
        assert_eq!(window.resets_at, Some(soon));
    }

    #[test]
    fn generic_ranking_does_not_consult_cursor_api_extras() {
        let extras = [extra(CURSOR_API_ID, 90.0)];
        assert_eq!(
            remaining(ProviderId::Claude, Some(10.0), Some(20.0), None, &extras),
            Some(80.0)
        );
    }
}
