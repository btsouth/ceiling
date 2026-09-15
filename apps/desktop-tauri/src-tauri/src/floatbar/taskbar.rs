//! Windows taskbar discovery for the floating bar.

use super::placement::{Point, Rect, place_in_taskbar};

const EDGE_PADDING: i32 = 8;
const OBSTACLE_CLEARANCE: i32 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskbarLayout {
    pub window_handle: isize,
    pub bounds: Rect,
    pub monitor_bounds: Rect,
    pub obstacles: Vec<Rect>,
    pub landmarks: TaskbarLandmarks,
    pub primary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TaskbarLandmarks {
    pub widgets: Option<Rect>,
    pub start: Option<Rect>,
    /// The `TrayNotifyWnd` notification-area rect, when found. Anchor point
    /// for the taskbar widget's Start-to-tray fallback lane.
    pub tray: Option<Rect>,
}

impl TaskbarLayout {
    pub fn preferred_anchor(&self) -> Point {
        if self.bounds.width() >= self.bounds.height() {
            Point {
                // Leave a taskbar-height-scaled lane for Start/Widgets on a
                // first run. Actual child-window obstacles refine this.
                x: self.bounds.left + self.bounds.height().saturating_mul(10) / 3,
                y: self.bounds.top,
            }
        } else {
            Point {
                x: self.bounds.left,
                y: self.bounds.top + self.bounds.width().saturating_mul(2),
            }
        }
    }

    pub fn place(&self, widget_width: i32, widget_height: i32, preferred: Point) -> Option<Point> {
        place_in_taskbar(
            self.bounds,
            &self.obstacles,
            widget_width,
            widget_height,
            preferred,
            EDGE_PADDING,
            OBSTACLE_CLEARANCE,
        )
    }
}

pub fn primary_layout(layouts: &[TaskbarLayout]) -> Option<&TaskbarLayout> {
    layouts
        .iter()
        .find(|layout| layout.primary)
        .or_else(|| layouts.first())
}

/// Choose the taskbar belonging to the monitor containing `point`.
///
/// Windows' virtual screen coordinates may be negative and monitors may have
/// unrelated physical sizes or DPI scales. Monitor rectangles from Win32 are
/// therefore compared directly without converting through logical pixels.
pub fn select_for_point(layouts: &[TaskbarLayout], point: Point) -> Option<&TaskbarLayout> {
    layouts
        .iter()
        .find(|layout| contains(layout.monitor_bounds, point))
        .or_else(|| {
            layouts
                .iter()
                .min_by_key(|layout| distance_squared(layout.monitor_bounds, point))
        })
}

fn contains(rect: Rect, point: Point) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}

fn distance_squared(rect: Rect, point: Point) -> i64 {
    let dx = if point.x < rect.left {
        i64::from(rect.left) - i64::from(point.x)
    } else if point.x >= rect.right {
        i64::from(point.x) - i64::from(rect.right.saturating_sub(1))
    } else {
        0
    };
    let dy = if point.y < rect.top {
        i64::from(rect.top) - i64::from(point.y)
    } else if point.y >= rect.bottom {
        i64::from(point.y) - i64::from(rect.bottom.saturating_sub(1))
    } else {
        0
    };
    dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
}

#[cfg(not(windows))]
pub fn discover_all() -> Vec<TaskbarLayout> {
    Vec::new()
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[cfg(windows)]
impl From<WinRect> for Rect {
    fn from(value: WinRect) -> Self {
        Self {
            left: value.left,
            top: value.top,
            right: value.right,
            bottom: value.bottom,
        }
    }
}

#[cfg(windows)]
struct ChildEnumContext {
    taskbar: Rect,
    obstacles: Vec<Rect>,
}

#[cfg(windows)]
struct TaskbarEnumContext {
    layouts: Vec<TaskbarLayout>,
}

#[cfg(windows)]
pub fn discover_all() -> Vec<TaskbarLayout> {
    unsafe extern "system" fn collect_taskbar(hwnd: isize, lparam: isize) -> i32 {
        unsafe {
            let Some(class_name) = window_class(hwnd) else {
                return 1;
            };
            let primary = class_name == "Shell_TrayWnd";
            if !primary && class_name != "Shell_SecondaryTrayWnd" {
                return 1;
            }

            let context = &mut *(lparam as *mut TaskbarEnumContext);
            if let Some(layout) = layout_for_taskbar(hwnd, primary) {
                context.layouts.push(layout);
            }
            1
        }
    }

    unsafe {
        let mut context = TaskbarEnumContext {
            layouts: Vec::new(),
        };
        EnumWindows(
            Some(collect_taskbar),
            (&mut context as *mut TaskbarEnumContext).cast::<std::ffi::c_void>() as isize,
        );
        context.layouts.sort_by_key(|layout| {
            (
                !layout.primary,
                layout.monitor_bounds.left,
                layout.monitor_bounds.top,
                layout.bounds.left,
                layout.bounds.top,
            )
        });
        context.layouts.dedup_by(|left, right| {
            left.monitor_bounds == right.monitor_bounds && left.bounds == right.bounds
        });
        context.layouts
    }
}

#[cfg(windows)]
unsafe fn layout_for_taskbar(hwnd: isize, primary: bool) -> Option<TaskbarLayout> {
    unsafe {
        let bounds = window_rect(hwnd)?;
        if bounds.width() <= 0 || bounds.height() <= 0 {
            return None;
        }

        let monitor_bounds = monitor_rect(hwnd).unwrap_or(bounds);
        let mut context = ChildEnumContext {
            taskbar: bounds,
            obstacles: Vec::new(),
        };
        EnumChildWindows(
            hwnd,
            Some(collect_child),
            (&mut context as *mut ChildEnumContext).cast::<std::ffi::c_void>() as isize,
        );

        let mut landmarks = TaskbarLandmarks::default();

        // Reserve the notification area when this taskbar exposes one. Some
        // Windows versions omit TrayNotifyWnd from secondary taskbars; child
        // enumeration still captures the controls that are actually present.
        // Also promote it to a landmark: it's the anchor for the taskbar
        // widget's Start-to-tray fallback lane.
        let tray_class = wide("TrayNotifyWnd");
        let tray = FindWindowExW(hwnd, 0, tray_class.as_ptr(), std::ptr::null());
        if let Some(tray_rect) = window_rect(tray) {
            context.obstacles.push(tray_rect);
            landmarks.tray = Some(tray_rect);
        }

        // Windows 11 renders Widgets, Start, Search, and app buttons as XAML.
        // Those controls are invisible to EnumChildWindows, so use UI
        // Automation as an obstacle source when Explorer exposes the tree.
        // Failure is intentionally non-fatal; the native widget then uses the
        // conservative preferred anchor and classic HWND obstacles.
        let automation_buttons = uia_buttons(hwnd);
        for button in &automation_buttons {
            match button.automation_id.as_str() {
                "WidgetsButton" => landmarks.widgets = Some(button.bounds),
                "StartButton" => landmarks.start = Some(button.bounds),
                _ => {}
            }
        }
        context
            .obstacles
            .extend(automation_buttons.into_iter().map(|button| button.bounds));

        context
            .obstacles
            .sort_by_key(|rect| (rect.left, rect.top, rect.right, rect.bottom));
        context.obstacles.dedup();
        Some(TaskbarLayout {
            window_handle: hwnd,
            bounds,
            monitor_bounds,
            obstacles: context.obstacles,
            landmarks,
            primary,
        })
    }
}

#[cfg(windows)]
struct AutomationButton {
    automation_id: String,
    bounds: Rect,
}

#[cfg(windows)]
unsafe fn uia_buttons(hwnd: isize) -> Vec<AutomationButton> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{
        CUIAutomation8, IUIAutomation, TreeScope_Descendants, UIA_ButtonControlTypeId,
        UIA_ControlTypePropertyId,
    };

    unsafe {
        // Taskbar discovery may run on a short-lived worker thread. Initialize
        // COM there explicitly; UI Automation must never rely on WebView or
        // Tauri having initialized that thread as a side effect.
        let uninitialize = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let result = (|| {
            let Ok(automation) =
                CoCreateInstance::<_, IUIAutomation>(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
            else {
                return Vec::new();
            };
            let Ok(root) = automation.ElementFromHandle(HWND(hwnd as *mut std::ffi::c_void)) else {
                return Vec::new();
            };
            let control_type = VARIANT::from(UIA_ButtonControlTypeId.0);
            let Ok(condition) =
                automation.CreatePropertyCondition(UIA_ControlTypePropertyId, &control_type)
            else {
                return Vec::new();
            };
            let Ok(buttons) = root.FindAll(TreeScope_Descendants, &condition) else {
                return Vec::new();
            };
            let Ok(length) = buttons.Length() else {
                return Vec::new();
            };

            (0..length)
                .filter_map(|index| buttons.GetElement(index).ok())
                .filter_map(|element| {
                    let automation_id = element.CurrentAutomationId().ok()?.to_string();
                    let rect = element.CurrentBoundingRectangle().ok()?;
                    Some(AutomationButton {
                        automation_id,
                        bounds: Rect {
                            left: rect.left,
                            top: rect.top,
                            right: rect.right,
                            bottom: rect.bottom,
                        },
                    })
                })
                .filter(|button| button.bounds.width() > 0 && button.bounds.height() > 0)
                .collect()
        })();
        if uninitialize {
            CoUninitialize();
        }
        result
    }
}

#[cfg(windows)]
unsafe extern "system" fn collect_child(hwnd: isize, lparam: isize) -> i32 {
    unsafe {
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        if window_class(hwnd).as_deref() == Some("CeilingNativeTaskbarWidget") {
            return 1;
        }
        let context = &mut *(lparam as *mut ChildEnumContext);
        let Some(rect) = window_rect(hwnd) else {
            return 1;
        };
        let width = rect.width();
        let height = rect.height();
        let horizontal = context.taskbar.width() >= context.taskbar.height();
        let taskbar_major = if horizontal {
            context.taskbar.width()
        } else {
            context.taskbar.height()
        };
        let rect_major = if horizontal { width } else { height };
        let rect_cross = if horizontal { height } else { width };
        let taskbar_cross = if horizontal {
            context.taskbar.height()
        } else {
            context.taskbar.width()
        };

        // Ignore invalid rectangles and shell composition containers that
        // span almost the complete taskbar. Smaller visible children are
        // useful obstacle hints on both classic and modern taskbars.
        if width > 0
            && height > 0
            && rect_major < taskbar_major.saturating_mul(3) / 4
            && rect_cross <= taskbar_cross.saturating_mul(2)
            && rect.left < context.taskbar.right
            && rect.right > context.taskbar.left
            && rect.top < context.taskbar.bottom
            && rect.bottom > context.taskbar.top
        {
            context.obstacles.push(rect);
        }
        1
    }
}

#[cfg(windows)]
unsafe fn window_rect(hwnd: isize) -> Option<Rect> {
    if hwnd == 0 {
        return None;
    }
    let mut raw = WinRect {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    (unsafe { GetWindowRect(hwnd, (&mut raw as *mut WinRect).cast()) } != 0)
        .then_some(Rect::from(raw))
}

#[cfg(windows)]
unsafe fn monitor_rect(hwnd: isize) -> Option<Rect> {
    let (left, top, right, bottom) = crate::shell::dwm::monitor_bounds_for_window(hwnd)?;
    Some(Rect {
        left,
        top,
        right,
        bottom,
    })
}

#[cfg(windows)]
unsafe fn window_class(hwnd: isize) -> Option<String> {
    let mut class_name = [0_u16; 64];
    let length = unsafe {
        GetClassNameW(
            hwnd,
            class_name.as_mut_ptr(),
            class_name.len().try_into().ok()?,
        )
    };
    (length > 0).then(|| String::from_utf16_lossy(&class_name[..length as usize]))
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn EnumWindows(
        callback: Option<unsafe extern "system" fn(isize, isize) -> i32>,
        lparam: isize,
    ) -> i32;
    fn FindWindowExW(
        parent: isize,
        child_after: isize,
        class_name: *const u16,
        window_name: *const u16,
    ) -> isize;
    fn GetWindowRect(hwnd: isize, rect: *mut std::ffi::c_void) -> i32;
    fn GetClassNameW(hwnd: isize, class_name: *mut u16, max_count: i32) -> i32;
    fn EnumChildWindows(
        parent: isize,
        callback: Option<unsafe extern "system" fn(isize, isize) -> i32>,
        lparam: isize,
    ) -> i32;
    fn IsWindowVisible(hwnd: isize) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(monitor_bounds: Rect, bounds: Rect, primary: bool) -> TaskbarLayout {
        TaskbarLayout {
            window_handle: 0,
            bounds,
            monitor_bounds,
            obstacles: Vec::new(),
            landmarks: TaskbarLandmarks::default(),
            primary,
        }
    }

    #[test]
    fn mixed_1440p_and_1080p_monitors_select_by_physical_center() {
        let layouts = [
            layout(
                Rect {
                    left: 0,
                    top: 0,
                    right: 2560,
                    bottom: 1440,
                },
                Rect {
                    left: 0,
                    top: 1392,
                    right: 2560,
                    bottom: 1440,
                },
                true,
            ),
            layout(
                Rect {
                    left: -1920,
                    top: 365,
                    right: 0,
                    bottom: 1445,
                },
                Rect {
                    left: -1920,
                    top: 1397,
                    right: 0,
                    bottom: 1445,
                },
                false,
            ),
        ];

        assert!(
            select_for_point(&layouts, Point { x: 1800, y: 1400 })
                .unwrap()
                .primary
        );
        assert!(
            !select_for_point(&layouts, Point { x: -900, y: 1405 })
                .unwrap()
                .primary
        );
    }

    #[test]
    fn mixed_dpi_selection_never_converts_physical_coordinates() {
        // The 1440p monitor may be 2048x1152 logical pixels at 125%, but the
        // Win32 monitor/taskbar rectangles remain 2560x1440 physical pixels.
        let layouts = [
            layout(
                Rect {
                    left: 0,
                    top: 0,
                    right: 2560,
                    bottom: 1440,
                },
                Rect {
                    left: 0,
                    top: 1392,
                    right: 2560,
                    bottom: 1440,
                },
                true,
            ),
            layout(
                Rect {
                    left: 2560,
                    top: 365,
                    right: 4480,
                    bottom: 1445,
                },
                Rect {
                    left: 2560,
                    top: 1397,
                    right: 4480,
                    bottom: 1445,
                },
                false,
            ),
        ];

        assert_eq!(
            select_for_point(&layouts, Point { x: 3200, y: 1405 })
                .unwrap()
                .monitor_bounds,
            layouts[1].monitor_bounds
        );
    }

    #[test]
    fn point_between_monitors_uses_the_nearest_physical_monitor() {
        let layouts = [
            layout(
                Rect {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                },
                Rect {
                    left: 0,
                    top: 1032,
                    right: 1920,
                    bottom: 1080,
                },
                true,
            ),
            layout(
                Rect {
                    left: 2200,
                    top: 0,
                    right: 4760,
                    bottom: 1440,
                },
                Rect {
                    left: 2200,
                    top: 1392,
                    right: 4760,
                    bottom: 1440,
                },
                false,
            ),
        ];

        assert!(
            select_for_point(&layouts, Point { x: 2000, y: 700 })
                .unwrap()
                .primary
        );
        assert!(
            !select_for_point(&layouts, Point { x: 2150, y: 700 })
                .unwrap()
                .primary
        );
    }

    #[test]
    fn primary_layout_falls_back_deterministically() {
        let secondary = layout(
            Rect {
                left: 1920,
                top: 0,
                right: 3840,
                bottom: 1080,
            },
            Rect {
                left: 1920,
                top: 1032,
                right: 3840,
                bottom: 1080,
            },
            false,
        );
        assert_eq!(
            primary_layout(std::slice::from_ref(&secondary)),
            Some(&secondary)
        );
        assert_eq!(primary_layout(&[]), None);
    }
}
