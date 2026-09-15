//! Detached "FloatBar" window: a small always-on-top transparent strip
//! that shows remaining capacity per provider. Runs as an auxiliary
//! Tauri window labeled `floatbar`, independent of the main surface
//! state machine.

#[cfg(windows)]
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use tauri::{LogicalPosition, LogicalSize, Manager, PhysicalPosition, WebviewUrl};

use crate::geometry_store;

use super::placement::Point;
use super::taskbar::TaskbarLayout;

pub const FLOATBAR_LABEL: &str = "floatbar";
pub const FLOAT_BAR_CONFIG_CHANGED_EVENT: &str = "float-bar-config-changed";
const FLOATBAR_DEFAULT_WIDTH_H: f64 = 360.0;
const FLOATBAR_DEFAULT_HEIGHT_H: f64 = 36.0;
const FLOATBAR_DEFAULT_WIDTH_V: f64 = 80.0;
const FLOATBAR_DEFAULT_HEIGHT_V: f64 = 280.0;
#[cfg(windows)]
const Z_ORDER_GUARD_INTERVAL: Duration = Duration::from_millis(100);
#[cfg(windows)]
const Z_ORDER_SAFETY_TICKS: u64 = 50;
#[cfg(windows)]
const TASKBAR_LAYOUT_SAFETY_TICKS: u64 = 300;
#[cfg_attr(not(windows), allow(dead_code))]
const WINDOW_RECOVERY_TICKS: u64 = 50;
#[cfg(windows)]
static Z_ORDER_DIRTY: AtomicBool = AtomicBool::new(true);
#[cfg(windows)]
static TASKBAR_LAYOUT_DIRTY: AtomicBool = AtomicBool::new(true);

/// Initial dimensions (logical pixels) for the floating bar given an
/// orientation string. Unknown values fall back to horizontal so callers
/// don't have to pre-validate.
pub fn initial_size(orientation: &str) -> (f64, f64) {
    match orientation {
        "vertical" => (FLOATBAR_DEFAULT_WIDTH_V, FLOATBAR_DEFAULT_HEIGHT_V),
        _ => (FLOATBAR_DEFAULT_WIDTH_H, FLOATBAR_DEFAULT_HEIGHT_H),
    }
}

/// Convert a 0..=100 opacity value to a Win32 SetLayeredWindowAttributes
/// alpha byte (0..=255). Values below 30 are clamped so the bar is never
/// fully invisible — that would be a usability footgun.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn opacity_to_alpha(opacity: u8) -> u8 {
    let clamped = opacity.clamp(30, 100);
    ((clamped as u32) * 255 / 100) as u8
}

/// Open the floating-bar window, or focus + reapply attributes if already
/// open. Position is restored from the geometry store keyed by
/// `floatbar`; on first launch the window is centered horizontally near
/// the top of the primary monitor.
pub fn show(
    app: &tauri::AppHandle,
    opacity: u8,
    orientation: &str,
    style: &str,
    click_through: bool,
) -> Result<(), String> {
    // A dead webview under the hidden window paints nothing; drop it so the
    // build path below runs instead of showing an empty strip (#410).
    crate::webview_recovery::reclaim_dead_window(app, FLOATBAR_LABEL)?;

    if let Some(window) = app.get_webview_window(FLOATBAR_LABEL) {
        apply_no_activate(&window);
        apply_opacity(&window, opacity);
        apply_click_through(&window, click_through);
        apply_always_on_top(&window);
        window.show().map_err(|e| e.to_string())?;
        if style == "taskbar" {
            reposition_taskbar(&window, true);
        }
        apply_always_on_top(&window);
        return Ok(());
    }

    let (w, h) = initial_size(orientation);
    let url =
        WebviewUrl::App(format!("index.html?window=floatbar&orientation={orientation}").into());

    let builder = tauri::WebviewWindowBuilder::new(app, FLOATBAR_LABEL, url)
        .title("Ceiling capacity strip")
        .inner_size(w, h)
        .decorations(false)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true);

    // WebView2 only honors an alpha (transparent) background when the native
    // window is itself created transparent. Tauri cfg-gates this builder API
    // off on macOS unless `macos-private-api` is enabled, so keep the Windows
    // fix out of the macOS validation path.
    #[cfg(windows)]
    let builder = builder.transparent(true);

    let win = builder
        .background_color(tauri::utils::config::Color(0, 0, 0, 0))
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;

    // Restore prior geometry if we have one. Otherwise, taskbar style opens
    // near the bottom while the original floating style keeps its top-center
    // placement.
    let stored_geometry = geometry_store::load_entry(FLOATBAR_LABEL);
    let has_stored_geometry = stored_geometry.is_some();
    if let Some(g) = stored_geometry {
        let _ = win.set_position(LogicalPosition::new(g.x as f64, g.y as f64));
        if let (Some(w), Some(h)) = (g.width, g.height) {
            let _ = win.set_size(LogicalSize::new(w as f64, h as f64));
        }
    } else if let Ok(Some(monitor)) = win.primary_monitor() {
        let scale = win.scale_factor().unwrap_or(1.0);
        let mon_x = monitor.position().x as f64 / scale;
        let mon_y = monitor.position().y as f64 / scale;
        let mon_w = monitor.size().width as f64 / scale;
        let mon_h = monitor.size().height as f64 / scale;
        let x = mon_x + (mon_w - w) / 2.0;
        let y = if style == "taskbar" {
            mon_y + mon_h - h - 8.0
        } else {
            mon_y + 8.0
        };
        let _ = win.set_position(LogicalPosition::new(x.max(mon_x), y.max(mon_y)));
    }

    apply_no_activate(&win);
    apply_opacity(&win, opacity);
    apply_click_through(&win, click_through);
    apply_always_on_top(&win);
    if style == "taskbar" {
        reposition_taskbar(&win, has_stored_geometry);
    }
    win.show().map_err(|e| e.to_string())?;
    apply_always_on_top(&win);
    Ok(())
}

/// Hide / destroy the floating bar.
pub fn hide(app: &tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(FLOATBAR_LABEL) {
        // Persist position before closing so it reopens in place.
        remember_geometry(&window);
        window.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Capture current position into the geometry store under the floatbar key.
///
/// Accepts any Tauri window handle (`Window` from event callbacks or
/// `WebviewWindow` from `get_webview_window`), since `WindowEvent`
/// callbacks deliver a `&Window` while imperative call sites have a
/// `&WebviewWindow`.
pub fn remember_geometry<R: tauri::Runtime, M: WindowGeometry<R>>(window: &M) {
    let Ok(pos) = window.outer_position() else {
        return;
    };
    let Ok(size) = window.outer_size() else {
        return;
    };
    let scale = window.scale_factor().unwrap_or(1.0);
    geometry_store::save_entry(
        FLOATBAR_LABEL,
        geometry_store::StoredGeometry {
            x: (pos.x as f64 / scale).round() as i32,
            y: (pos.y as f64 / scale).round() as i32,
            width: Some((size.width as f64 / scale).round() as u32),
            height: Some((size.height as f64 / scale).round() as u32),
        },
    );
}

/// Subset of `tauri::WebviewWindow` / `tauri::Window` used by
/// [`remember_geometry`]. Both types implement the underlying methods, but
/// they don't share a public trait — this private trait bridges them so we
/// can be called from `WindowEvent` (which delivers `&Window`) and from
/// imperative paths (which hold `&WebviewWindow`).
pub trait WindowGeometry<R: tauri::Runtime> {
    fn outer_position(&self) -> tauri::Result<tauri::PhysicalPosition<i32>>;
    fn outer_size(&self) -> tauri::Result<tauri::PhysicalSize<u32>>;
    fn scale_factor(&self) -> tauri::Result<f64>;
}

impl<R: tauri::Runtime> WindowGeometry<R> for tauri::WebviewWindow<R> {
    fn outer_position(&self) -> tauri::Result<tauri::PhysicalPosition<i32>> {
        tauri::WebviewWindow::outer_position(self)
    }
    fn outer_size(&self) -> tauri::Result<tauri::PhysicalSize<u32>> {
        tauri::WebviewWindow::outer_size(self)
    }
    fn scale_factor(&self) -> tauri::Result<f64> {
        tauri::WebviewWindow::scale_factor(self)
    }
}

impl<R: tauri::Runtime> WindowGeometry<R> for tauri::Window<R> {
    fn outer_position(&self) -> tauri::Result<tauri::PhysicalPosition<i32>> {
        tauri::Window::outer_position(self)
    }
    fn outer_size(&self) -> tauri::Result<tauri::PhysicalSize<u32>> {
        tauri::Window::outer_size(self)
    }
    fn scale_factor(&self) -> tauri::Result<f64> {
        tauri::Window::scale_factor(self)
    }
}

/// Resize the floatbar to the given logical dimensions and re-assert the
/// native interaction invariants in the same step.
///
/// A resize goes through `SetWindowPos`/frame changes, which can drop the
/// extended window styles, so the no-activate and click-through flags must be
/// re-applied afterwards. Keeping both halves here gives callers (including the
/// webview) a single canonical "the bar changed size" entry point instead of
/// pairing a JS `setSize` with a separate native repair command.
pub fn resize(
    window: &tauri::WebviewWindow,
    width: f64,
    height: f64,
    click_through: bool,
) -> Result<(), String> {
    window
        .set_size(LogicalSize::new(width, height))
        .map_err(|e| e.to_string())?;
    apply_no_activate(window);
    apply_click_through(window, click_through);
    apply_always_on_top(window);
    Ok(())
}

/// Snap a taskbar-styled floatbar into the nearest complete free gap.
///
/// Discovery and placement use physical pixels end-to-end. When taskbar
/// discovery fails or no gap can fit the complete bar, the current position is
/// deliberately left untouched.
pub fn reposition_taskbar(window: &tauri::WebviewWindow, prefer_current: bool) {
    let layouts = super::taskbar::discover_all();
    reposition_taskbar_with_layouts(window, &layouts, prefer_current);
}

fn reposition_taskbar_with_layouts(
    window: &tauri::WebviewWindow,
    layouts: &[TaskbarLayout],
    prefer_current: bool,
) {
    let Ok(size) = window.outer_size() else {
        return;
    };
    let current = window.outer_position().ok().map(|position| Point {
        x: position.x,
        y: position.y,
    });
    let layout = if prefer_current {
        current
            .and_then(|position| {
                let center = Point {
                    x: position.x.saturating_add((size.width / 2) as i32),
                    y: position.y.saturating_add((size.height / 2) as i32),
                };
                super::taskbar::select_for_point(layouts, center)
            })
            .or_else(|| super::taskbar::primary_layout(layouts))
    } else {
        super::taskbar::primary_layout(layouts)
    };
    let Some(layout) = layout else {
        return;
    };
    let preferred = if prefer_current {
        current.unwrap_or_else(|| layout.preferred_anchor())
    } else {
        layout.preferred_anchor()
    };
    let Some(target) = layout.place(size.width as i32, size.height as i32, preferred) else {
        return;
    };

    if let Ok(current) = window.outer_position()
        && (current.x - target.x).abs() < 2
        && (current.y - target.y).abs() < 2
    {
        return;
    }

    let _ = window.set_position(PhysicalPosition::new(target.x, target.y));
    apply_no_activate(window);
    apply_always_on_top(window);
}

/// Keep the floating bar at the front of Windows' topmost band.
///
/// Explorer owns the taskbar and periodically promotes it back to the front
/// after Start/Search, notifications, display changes, and Explorer restarts.
/// A one-time `always_on_top` flag therefore is not sufficient for a widget
/// intentionally placed inside the taskbar's screen area. The guard runs only
/// while the bar exists and is visible, never activates it, and performs no
/// geometry work.
pub fn install_z_order_guard(app: tauri::AppHandle) {
    #[cfg(windows)]
    {
        install_win_event_hooks();
        tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(Z_ORDER_GUARD_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let mut ticks = 0_u64;
            loop {
                interval.tick().await;
                let Some(window) = app.get_webview_window(FLOATBAR_LABEL) else {
                    let recover_window = repair_due(false, ticks, WINDOW_RECOVERY_TICKS);
                    ticks = ticks.wrapping_add(1);
                    if recover_window {
                        let settings = codexbar::settings::Settings::load();
                        if settings.float_bar_enabled {
                            if let Err(error) = super::apply_state(&app, &settings) {
                                tracing::warn!(%error, "Could not recover floating bar window");
                            }
                            Z_ORDER_DIRTY.store(true, Ordering::Release);
                            TASKBAR_LAYOUT_DIRTY.store(true, Ordering::Release);
                        }
                    }
                    continue;
                };
                let visible = window.is_visible().unwrap_or(false);
                let repair_z_order = repair_due(
                    Z_ORDER_DIRTY.swap(false, Ordering::AcqRel),
                    ticks,
                    Z_ORDER_SAFETY_TICKS,
                );
                if visible && repair_z_order {
                    apply_always_on_top(&window);
                }
                ticks = ticks.wrapping_add(1);
                let repair_layout = repair_due(
                    TASKBAR_LAYOUT_DIRTY.swap(false, Ordering::AcqRel),
                    ticks,
                    TASKBAR_LAYOUT_SAFETY_TICKS,
                );
                if visible && repair_layout {
                    let settings = codexbar::settings::Settings::load();
                    if settings.float_bar_style == "taskbar" {
                        let layouts = super::taskbar::discover_all();
                        reposition_taskbar_with_layouts(&window, &layouts, true);
                    }
                }
            }
        });
    }
    #[cfg(not(windows))]
    let _ = app;
}

#[cfg_attr(not(windows), allow(dead_code))]
fn repair_due(dirty: bool, ticks: u64, safety_ticks: u64) -> bool {
    dirty || (safety_ticks > 0 && ticks.is_multiple_of(safety_ticks))
}

#[cfg(windows)]
fn install_win_event_hooks() {
    unsafe {
        const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
        const EVENT_OBJECT_CREATE: u32 = 0x8000;
        const EVENT_OBJECT_REORDER: u32 = 0x8004;
        const EVENT_OBJECT_LOCATIONCHANGE: u32 = 0x800B;
        const WINEVENT_OUTOFCONTEXT: u32 = 0;
        const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;

        // These out-of-context hooks remain active for the process lifetime.
        // They are installed on Tauri's UI thread, whose message loop delivers
        // callbacks without injecting code into Explorer.
        let _ = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            0,
            Some(floatbar_win_event),
            0,
            0,
            flags,
        );
        let _ = SetWinEventHook(
            EVENT_OBJECT_CREATE,
            EVENT_OBJECT_CREATE,
            0,
            Some(floatbar_win_event),
            0,
            0,
            flags,
        );
        let _ = SetWinEventHook(
            EVENT_OBJECT_REORDER,
            EVENT_OBJECT_REORDER,
            0,
            Some(floatbar_win_event),
            0,
            0,
            flags,
        );
        let _ = SetWinEventHook(
            EVENT_OBJECT_LOCATIONCHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
            0,
            Some(floatbar_win_event),
            0,
            0,
            flags,
        );
    }
}

#[cfg(windows)]
unsafe extern "system" fn floatbar_win_event(
    _hook: isize,
    event: u32,
    hwnd: isize,
    _object_id: i32,
    _child_id: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
    if event == EVENT_SYSTEM_FOREGROUND {
        Z_ORDER_DIRTY.store(true, Ordering::Release);
        return;
    }

    if unsafe { is_taskbar_window(hwnd) } {
        Z_ORDER_DIRTY.store(true, Ordering::Release);
        TASKBAR_LAYOUT_DIRTY.store(true, Ordering::Release);
    }
}

#[cfg(windows)]
unsafe fn is_taskbar_window(hwnd: isize) -> bool {
    if hwnd == 0 {
        return false;
    }
    const GA_ROOT: u32 = 2;
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    let candidate = if root == 0 { hwnd } else { root };
    let mut class_name = [0_u16; 64];
    let length =
        unsafe { GetClassNameW(candidate, class_name.as_mut_ptr(), class_name.len() as i32) };
    if length <= 0 {
        return false;
    }
    matches!(
        String::from_utf16_lossy(&class_name[..length as usize]).as_str(),
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    )
}

/// Re-assert native topmost ordering without activating the window.
///
/// Tauri's `always_on_top(true)` sets the initial intent, but on Windows
/// resize/style changes and competing topmost windows (especially Explorer's
/// taskbar) can still disturb z-order. This Win32 pass promotes the floatbar to
/// the front of the topmost band while preserving the foreground app's focus.
pub fn apply_always_on_top(window: &tauri::WebviewWindow) {
    let _ = window;
    #[cfg(windows)]
    {
        use raw_window_handle::HasWindowHandle;
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() else {
            return;
        };
        unsafe {
            const HWND_TOPMOST: isize = -1;
            const SWP_NOSIZE: u32 = 0x0001;
            const SWP_NOMOVE: u32 = 0x0002;
            const SWP_NOACTIVATE: u32 = 0x0010;
            const SWP_NOOWNERZORDER: u32 = 0x0200;
            const SWP_NOSENDCHANGING: u32 = 0x0400;
            let flags =
                SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOSENDCHANGING;
            SetWindowPos(h.hwnd.get(), HWND_TOPMOST, 0, 0, 0, 0, flags);
        }
    }
}

/// Apply the current opacity setting to an existing floatbar window via
/// `SetLayeredWindowAttributes`. No-op on non-Windows platforms.
pub fn apply_opacity(window: &tauri::WebviewWindow, opacity: u8) {
    let _ = (window, opacity);
    #[cfg(windows)]
    {
        use raw_window_handle::HasWindowHandle;
        let alpha = opacity_to_alpha(opacity);
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() else {
            return;
        };
        unsafe {
            // Ensure WS_EX_LAYERED is set so SetLayeredWindowAttributes works.
            const WS_EX_LAYERED: isize = 0x00080000;
            let ex = GetWindowLongPtrW(h.hwnd.get(), GWL_EXSTYLE);
            if ex & WS_EX_LAYERED == 0 {
                set_extended_style(h.hwnd.get(), ex | WS_EX_LAYERED);
            }
            const LWA_ALPHA: u32 = 0x00000002;
            SetLayeredWindowAttributes(h.hwnd.get(), 0, alpha, LWA_ALPHA);
        }
    }
}

/// Keep the floatbar from activating when it is shown or clicked. This makes
/// it behave like a desktop widget that visually sits above the taskbar without
/// stealing focus from the active app.
pub fn apply_no_activate(window: &tauri::WebviewWindow) {
    let _ = window;
    #[cfg(windows)]
    {
        use raw_window_handle::HasWindowHandle;
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() else {
            return;
        };
        unsafe {
            const WS_EX_NOACTIVATE: isize = 0x08000000;
            let ex = GetWindowLongPtrW(h.hwnd.get(), GWL_EXSTYLE);
            if ex & WS_EX_NOACTIVATE == 0 {
                set_extended_style(h.hwnd.get(), ex | WS_EX_NOACTIVATE);
            }
        }
    }
}

/// Toggle click-through (`WS_EX_TRANSPARENT`). When enabled, mouse events
/// pass through to the window beneath — true overlay mode.
pub fn apply_click_through(window: &tauri::WebviewWindow, click_through: bool) {
    let _ = (window, click_through);
    #[cfg(windows)]
    {
        use raw_window_handle::HasWindowHandle;
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() else {
            return;
        };
        unsafe {
            const WS_EX_LAYERED: isize = 0x00080000;
            const WS_EX_TRANSPARENT: isize = 0x00000020;
            let ex = GetWindowLongPtrW(h.hwnd.get(), GWL_EXSTYLE);
            let mut new_ex = ex | WS_EX_LAYERED;
            if click_through {
                new_ex |= WS_EX_TRANSPARENT;
            } else {
                new_ex &= !WS_EX_TRANSPARENT;
            }
            if new_ex != ex {
                set_extended_style(h.hwnd.get(), new_ex);
            }
        }
    }
}

#[cfg(windows)]
const GWL_EXSTYLE: i32 = -20;

#[cfg(windows)]
unsafe fn set_extended_style(hwnd: isize, ex_style: isize) {
    unsafe {
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style);
        const SWP_NOSIZE: u32 = 0x0001;
        const SWP_NOMOVE: u32 = 0x0002;
        const SWP_NOZORDER: u32 = 0x0004;
        const SWP_NOACTIVATE: u32 = 0x0010;
        const SWP_FRAMECHANGED: u32 = 0x0020;
        let flags = SWP_NOSIZE | SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED;
        SetWindowPos(hwnd, 0, 0, 0, 0, 0, flags);
    }
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
    fn SetWindowLongPtrW(hwnd: isize, index: i32, new: isize) -> isize;
    fn SetLayeredWindowAttributes(hwnd: isize, color_key: u32, alpha: u8, flags: u32) -> i32;
    fn SetWindowPos(
        hwnd: isize,
        hwnd_insert_after: isize,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
    fn SetWinEventHook(
        event_min: u32,
        event_max: u32,
        module: isize,
        callback: Option<unsafe extern "system" fn(isize, u32, isize, i32, i32, u32, u32)>,
        process_id: u32,
        thread_id: u32,
        flags: u32,
    ) -> isize;
    fn GetAncestor(hwnd: isize, flags: u32) -> isize;
    fn GetClassNameW(hwnd: isize, class_name: *mut u16, max_count: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opacity_to_alpha_clamps_low_values() {
        assert_eq!(opacity_to_alpha(0), opacity_to_alpha(30));
        assert_eq!(opacity_to_alpha(10), opacity_to_alpha(30));
    }

    #[test]
    fn opacity_to_alpha_full_is_255() {
        assert_eq!(opacity_to_alpha(100), 255);
    }

    #[test]
    fn opacity_to_alpha_is_monotonic() {
        let a = opacity_to_alpha(30);
        let b = opacity_to_alpha(60);
        let c = opacity_to_alpha(100);
        assert!(a < b);
        assert!(b < c);
    }

    #[test]
    fn opacity_to_alpha_midpoint() {
        // 50% should be roughly half of 255.
        let alpha = opacity_to_alpha(50);
        assert!((125..=130).contains(&alpha), "got {alpha}");
    }

    #[test]
    fn initial_size_picks_orientation() {
        assert_eq!(
            initial_size("horizontal"),
            (FLOATBAR_DEFAULT_WIDTH_H, FLOATBAR_DEFAULT_HEIGHT_H)
        );
        assert_eq!(
            initial_size("vertical"),
            (FLOATBAR_DEFAULT_WIDTH_V, FLOATBAR_DEFAULT_HEIGHT_V)
        );
        // Unknown values fall through to horizontal so a corrupted setting
        // can't yield an unreadable strip.
        assert_eq!(
            initial_size("diagonal"),
            (FLOATBAR_DEFAULT_WIDTH_H, FLOATBAR_DEFAULT_HEIGHT_H)
        );
    }

    #[test]
    fn event_driven_repair_runs_immediately_when_dirty() {
        assert!(repair_due(true, 1, 50));
    }

    #[test]
    fn recovery_watchdog_is_only_a_safety_net() {
        assert!(!repair_due(false, 49, 50));
        assert!(repair_due(false, 50, 50));
    }

    #[test]
    fn lost_window_recovery_uses_the_slow_watchdog_cadence() {
        assert!(repair_due(false, 0, WINDOW_RECOVERY_TICKS));
        assert!(!repair_due(false, 1, WINDOW_RECOVERY_TICKS));
        assert!(repair_due(false, 50, WINDOW_RECOVERY_TICKS));
    }
}
