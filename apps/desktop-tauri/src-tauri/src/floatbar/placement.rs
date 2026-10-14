//! Pure taskbar-placement math.
//!
//! Windows discovery lives in `taskbar.rs`; this module stays platform-free so
//! mixed-DPI, obstacle, and monitor-boundary behavior can be unit tested on CI.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(self) -> i32 {
        self.right.saturating_sub(self.left)
    }

    pub fn height(self) -> i32 {
        self.bottom.saturating_sub(self.top)
    }

    fn intersects(self, other: Self) -> bool {
        self.left < other.right
            && self.right > other.left
            && self.top < other.bottom
            && self.bottom > other.top
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    start: i32,
    end: i32,
}

/// Find the nearest taskbar position where the complete widget fits.
///
/// Obstacles are expanded by `clearance`, and the widget is always centered on
/// the taskbar's cross axis. Returning `None` means the caller should keep its
/// last known-good position rather than overlap taskbar controls.
pub fn place_in_taskbar(
    taskbar: Rect,
    obstacles: &[Rect],
    widget_width: i32,
    widget_height: i32,
    preferred: Point,
    edge_padding: i32,
    clearance: i32,
) -> Option<Point> {
    if taskbar.width() <= 0 || taskbar.height() <= 0 || widget_width <= 0 || widget_height <= 0 {
        return None;
    }

    let horizontal = taskbar.width() >= taskbar.height();
    let (bounds, widget_extent, preferred_start) = if horizontal {
        (
            Span {
                start: taskbar.left,
                end: taskbar.right,
            },
            widget_width,
            preferred.x,
        )
    } else {
        (
            Span {
                start: taskbar.top,
                end: taskbar.bottom,
            },
            widget_height,
            preferred.y,
        )
    };

    let obstacle_spans = obstacles
        .iter()
        .copied()
        .filter(|obstacle| obstacle.intersects(taskbar))
        .map(|obstacle| {
            if horizontal {
                Span {
                    start: obstacle.left,
                    end: obstacle.right,
                }
            } else {
                Span {
                    start: obstacle.top,
                    end: obstacle.bottom,
                }
            }
        })
        .collect::<Vec<_>>();

    let gaps = free_spans(bounds, &obstacle_spans, edge_padding, clearance);
    let start = nearest_fitting_start(preferred_start, &gaps, widget_extent)?;

    if horizontal {
        Some(Point {
            x: start,
            y: taskbar.top + (taskbar.height() - widget_height) / 2,
        })
    } else {
        Some(Point {
            x: taskbar.left + (taskbar.width() - widget_width) / 2,
            y: start,
        })
    }
}

fn free_spans(bounds: Span, obstacles: &[Span], edge_padding: i32, clearance: i32) -> Vec<Span> {
    let start = bounds.start.saturating_add(edge_padding.max(0));
    let end = bounds.end.saturating_sub(edge_padding.max(0));
    if end <= start {
        return Vec::new();
    }

    let mut blocked = obstacles
        .iter()
        .filter_map(|obstacle| {
            let blocked_start = obstacle.start.saturating_sub(clearance.max(0)).max(start);
            let blocked_end = obstacle.end.saturating_add(clearance.max(0)).min(end);
            (blocked_end > blocked_start).then_some(Span {
                start: blocked_start,
                end: blocked_end,
            })
        })
        .collect::<Vec<_>>();
    blocked.sort_by_key(|span| span.start);

    let mut gaps = Vec::new();
    let mut cursor = start;
    for obstacle in blocked {
        if obstacle.start > cursor {
            gaps.push(Span {
                start: cursor,
                end: obstacle.start,
            });
        }
        cursor = cursor.max(obstacle.end);
        if cursor >= end {
            break;
        }
    }
    if cursor < end {
        gaps.push(Span { start: cursor, end });
    }
    gaps
}

fn nearest_fitting_start(preferred: i32, gaps: &[Span], extent: i32) -> Option<i32> {
    gaps.iter()
        .filter(|gap| gap.end.saturating_sub(gap.start) >= extent)
        .map(|gap| preferred.clamp(gap.start, gap.end - extent))
        .min_by_key(|candidate| (i64::from(*candidate) - i64::from(preferred)).abs())
}

/// Whether a remembered bar rectangle would reopen where it can be seen.
///
/// The bar's center must land on a connected monitor. A minimized position
/// (-32000, -32000) or a monitor unplugged since the last run would otherwise
/// reopen the bar off-screen, and its skip-taskbar window leaves no way to
/// bring it back (#501). With no monitors to compare against, the stored
/// position is trusted as before.
pub fn is_on_any_monitor(window: Rect, monitors: &[Rect]) -> bool {
    let center = Point {
        x: window.left.saturating_add(window.width() / 2),
        y: window.top.saturating_add(window.height() / 2),
    };
    monitors.is_empty()
        || monitors.iter().any(|monitor| {
            (monitor.left..monitor.right).contains(&center.x)
                && (monitor.top..monitor.bottom).contains(&center.y)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASKBAR: Rect = Rect {
        left: 0,
        top: 1032,
        right: 1920,
        bottom: 1080,
    };

    #[test]
    fn keeps_widget_inside_taskbar_bounds() {
        let point = place_in_taskbar(TASKBAR, &[], 360, 36, Point { x: 1800, y: 0 }, 8, 6).unwrap();
        assert_eq!(point, Point { x: 1552, y: 1038 });
    }

    #[test]
    fn chooses_nearest_complete_gap() {
        let obstacles = [
            Rect {
                left: 0,
                top: 1032,
                right: 150,
                bottom: 1080,
            },
            Rect {
                left: 760,
                top: 1032,
                right: 1160,
                bottom: 1080,
            },
            Rect {
                left: 1680,
                top: 1032,
                right: 1920,
                bottom: 1080,
            },
        ];
        let point =
            place_in_taskbar(TASKBAR, &obstacles, 360, 36, Point { x: 200, y: 0 }, 8, 6).unwrap();
        assert_eq!(point.x, 200);
        assert_eq!(point.y, 1038);
    }

    #[test]
    fn refuses_to_overlap_when_no_gap_fits() {
        let obstacle = Rect {
            left: 300,
            top: 1032,
            right: 1700,
            bottom: 1080,
        };
        assert_eq!(
            place_in_taskbar(TASKBAR, &[obstacle], 400, 36, Point { x: 500, y: 0 }, 8, 6,),
            None
        );
    }

    #[test]
    fn vertical_taskbars_use_the_vertical_axis() {
        let taskbar = Rect {
            left: 0,
            top: 0,
            right: 48,
            bottom: 1080,
        };
        let tray = Rect {
            left: 0,
            top: 880,
            right: 48,
            bottom: 1080,
        };
        let point =
            place_in_taskbar(taskbar, &[tray], 36, 280, Point { x: 0, y: 700 }, 8, 6).unwrap();
        assert_eq!(point, Point { x: 6, y: 594 });
    }

    #[test]
    fn preserves_negative_coordinates_on_a_left_hand_monitor() {
        let taskbar = Rect {
            left: -1920,
            top: 1032,
            right: 0,
            bottom: 1080,
        };
        let tray = Rect {
            left: -250,
            top: 1032,
            right: 0,
            bottom: 1080,
        };
        let point =
            place_in_taskbar(taskbar, &[tray], 360, 36, Point { x: -600, y: 0 }, 8, 6).unwrap();
        // The preferred point would clip the tray clearance by 16px, so it is
        // shifted left while remaining in the monitor's negative coordinate space.
        assert_eq!(point, Point { x: -616, y: 1038 });
    }

    #[test]
    fn overlapping_obstacles_are_merged_before_gap_selection() {
        let obstacles = [
            Rect {
                left: 100,
                top: 1032,
                right: 500,
                bottom: 1080,
            },
            Rect {
                left: 450,
                top: 1032,
                right: 900,
                bottom: 1080,
            },
        ];
        let point =
            place_in_taskbar(TASKBAR, &obstacles, 360, 36, Point { x: 600, y: 0 }, 8, 6).unwrap();
        assert_eq!(point.x, 906);
    }

    const PRIMARY: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };
    const LEFT_MONITOR: Rect = Rect {
        left: -1280,
        top: 0,
        right: 0,
        bottom: 1024,
    };

    fn bar_at(left: i32, top: i32) -> Rect {
        Rect {
            left,
            top,
            right: left + 222,
            bottom: top + 44,
        }
    }

    #[test]
    fn minimized_position_is_not_on_any_monitor() {
        assert!(!is_on_any_monitor(
            bar_at(-32000, -32000),
            &[PRIMARY, LEFT_MONITOR]
        ));
    }

    #[test]
    fn position_on_a_disconnected_monitor_is_not_restorable() {
        assert!(!is_on_any_monitor(bar_at(-900, 400), &[PRIMARY]));
    }

    #[test]
    fn positions_on_connected_monitors_are_restorable() {
        assert!(is_on_any_monitor(bar_at(800, 1038), &[PRIMARY]));
        assert!(is_on_any_monitor(
            bar_at(-900, 400),
            &[PRIMARY, LEFT_MONITOR]
        ));
    }

    #[test]
    fn bar_mostly_past_a_monitor_edge_is_not_restorable() {
        // Only 20px of the bar remains on the primary monitor.
        assert!(!is_on_any_monitor(bar_at(1900, 400), &[PRIMARY]));
    }

    #[test]
    fn stored_position_is_trusted_without_monitor_information() {
        assert!(is_on_any_monitor(bar_at(-32000, -32000), &[]));
    }
}
