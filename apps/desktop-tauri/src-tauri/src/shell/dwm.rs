//! Windows DWM helpers for eliminating the non-client caption area.
//!
//! Even with `decorations(false)`, Windows keeps a thin caption strip
//! that DWM renders. We install a window subclass that intercepts
//! WM_NCCALCSIZE to zero the non-client area and WM_NCPAINT/WM_NCACTIVATE
//! to suppress DWM painting, making the window truly borderless.
//!
//! Opaque borderless windows also get a native background that follows the
//! resolved light/dark theme (see [`Backdrop`]), so the area behind the
//! WebView matches the page while the window opens or resizes.

#[cfg(windows)]
use std::ffi::c_void;

#[cfg(windows)]
#[link(name = "dwmapi")]
unsafe extern "system" {
    fn DwmSetWindowAttribute(hwnd: isize, attr: u32, data: *const c_void, size: u32) -> i32;
    fn DwmExtendFrameIntoClientArea(hwnd: isize, margins: *const Margins) -> i32;
}

#[cfg(windows)]
#[repr(C)]
struct Margins {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WinPoint {
    x: i32,
    y: i32,
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WinRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// Win32 `MINMAXINFO`. `lparam` of `WM_GETMINMAXINFO` points at one of these.
#[cfg(windows)]
#[repr(C)]
struct MinMaxInfo {
    reserved: WinPoint,
    max_size: WinPoint,
    max_position: WinPoint,
    min_track_size: WinPoint,
    max_track_size: WinPoint,
}

/// Win32 `MONITORINFO` (40 bytes). `cb_size` must be set before the call.
#[cfg(windows)]
#[repr(C)]
struct MonitorInfo {
    cb_size: u32,
    rc_monitor: WinRect,
    rc_work: WinRect,
    dw_flags: u32,
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetAncestor(hwnd: isize, flags: u32) -> isize;
    fn SetWindowLongPtrW(hwnd: isize, index: i32, new: isize) -> isize;
    fn GetWindowLongPtrW(hwnd: isize, index: i32) -> isize;
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, w: i32, h: i32, flags: u32) -> i32;
    fn DefSubclassProc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn MonitorFromWindow(hwnd: isize, flags: u32) -> isize;
    fn GetMonitorInfoW(hmonitor: isize, info: *mut MonitorInfo) -> i32;
}

/// Return the physical Win32 monitor rectangle containing `hwnd`.
///
/// Keeping this query beside the existing DWM declarations avoids duplicate
/// extern signatures in other native window features.
#[cfg(windows)]
pub(crate) fn monitor_bounds_for_window(hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    const MONITOR_DEFAULTTONEAREST: u32 = 2;
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if monitor == 0 {
            return None;
        }
        let mut info = MonitorInfo {
            cb_size: std::mem::size_of::<MonitorInfo>() as u32,
            rc_monitor: WinRect::default(),
            rc_work: WinRect::default(),
            dw_flags: 0,
        };
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        Some((
            info.rc_monitor.left,
            info.rc_monitor.top,
            info.rc_monitor.right,
            info.rc_monitor.bottom,
        ))
    }
}

#[cfg(windows)]
#[link(name = "comctl32")]
unsafe extern "system" {
    fn SetWindowSubclass(
        hwnd: isize,
        pfn: unsafe extern "system" fn(isize, u32, usize, isize, usize, usize) -> isize,
        id: usize,
        data: usize,
    ) -> i32;
}

/// `--app-bg` of the light theme in `styles.css`.
const LIGHT_BACKDROP: (u8, u8, u8) = (0xF5, 0xF7, 0xFA);
/// `--app-bg` of the dark theme in `styles.css`.
const DARK_BACKDROP: (u8, u8, u8) = (0x1C, 0x20, 0x26);

/// What the native window shows behind its WebView.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backdrop {
    /// See-through windows (the dashboard and the taskbar flyout) keep the
    /// default transparent background. Their DWM frame follows the theme
    /// through `setTheme`.
    Transparent,
    /// Opaque windows (Settings) are filled with the app background of the
    /// given theme, so a light window does not flash dark while it opens or
    /// resizes. Apply it again when the window theme changes.
    Opaque(tauri::Theme),
}

/// Background colour painted behind the WebView of an opaque window.
pub fn backdrop_color(theme: tauri::Theme) -> tauri::window::Color {
    let (r, g, b) = match theme {
        tauri::Theme::Light => LIGHT_BACKDROP,
        _ => DARK_BACKDROP,
    };
    tauri::window::Color(r, g, b, 255)
}

/// The window's current theme, or dark when it can't be read.
pub fn current_theme(win: &tauri::WebviewWindow) -> tauri::Theme {
    win.theme().unwrap_or(tauri::Theme::Dark)
}

/// Apply `backdrop` to the native window and to the WebView's default
/// background, which is what shows before the page paints and in the strip a
/// resize exposes. Tauri keeps both per window, unlike the window class
/// brush, which every Tauri window shares.
pub fn apply_backdrop(win: &tauri::WebviewWindow, backdrop: Backdrop) {
    let Backdrop::Opaque(theme) = backdrop else {
        return;
    };
    if let Err(error) = win.set_background_color(Some(backdrop_color(theme))) {
        tracing::warn!("dwm: couldn't set the {theme:?} background: {error}");
    }
}

#[cfg(windows)]
const WM_NCCALCSIZE: u32 = 0x0083;
#[cfg(windows)]
const WM_NCPAINT: u32 = 0x0085;
#[cfg(windows)]
const WM_NCACTIVATE: u32 = 0x0086;
#[cfg(windows)]
const WM_GETMINMAXINFO: u32 = 0x0024;
#[cfg(windows)]
const BORDERLESS_SUBCLASS_ID: usize = 0xC0DE_BA12;

#[cfg(windows)]
unsafe extern "system" fn borderless_subclass_proc(
    hwnd: isize,
    msg: u32,
    wparam: usize,
    lparam: isize,
    _id: usize,
    _data: usize,
) -> isize {
    match msg {
        WM_NCCALCSIZE => {
            if wparam != 0 {
                // Returning 0 when wparam is TRUE tells Windows the
                // client area == the window area (no non-client area).
                return 0;
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        WM_NCPAINT => {
            // Suppress DWM non-client painting entirely.
            0
        }
        WM_NCACTIVATE => {
            // Return TRUE to accept activation but skip DWM painting.
            1
        }
        WM_GETMINMAXINFO => {
            // A borderless window whose non-client area is zeroed maximizes to
            // cover the entire monitor, including the taskbar. Constrain the
            // maximized position/size to the monitor work area instead.
            const MONITOR_DEFAULTTONEAREST: u32 = 2;
            unsafe {
                let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                if hmon != 0 && lparam != 0 {
                    let mut mi = MonitorInfo {
                        cb_size: std::mem::size_of::<MonitorInfo>() as u32,
                        rc_monitor: WinRect::default(),
                        rc_work: WinRect::default(),
                        dw_flags: 0,
                    };
                    if GetMonitorInfoW(hmon, &mut mi) != 0 {
                        let mmi = lparam as *mut MinMaxInfo;
                        (*mmi).max_position = WinPoint {
                            x: mi.rc_work.left - mi.rc_monitor.left,
                            y: mi.rc_work.top - mi.rc_monitor.top,
                        };
                        (*mmi).max_size = WinPoint {
                            x: mi.rc_work.right - mi.rc_work.left,
                            y: mi.rc_work.bottom - mi.rc_work.top,
                        };
                        (*mmi).max_track_size = (*mmi).max_size;
                    }
                }
            }
            0
        }
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

/// Eliminate the DWM caption bar by subclassing the window to zero the
/// non-client area, and apply `backdrop`.  Safe to call on multiple
/// windows — each gets its own subclass via `SetWindowSubclass`.
#[cfg(windows)]
pub fn force_borderless_caption(win: &tauri::WebviewWindow, backdrop: Backdrop) {
    force_borderless_caption_inner(win, false, backdrop);
}

/// Same as [`force_borderless_caption`] but keeps `WS_THICKFRAME`, so the
/// native resize affordance still works.
#[cfg(windows)]
pub fn force_borderless_caption_resizable(win: &tauri::WebviewWindow, backdrop: Backdrop) {
    force_borderless_caption_inner(win, true, backdrop);
}

#[cfg(windows)]
fn force_borderless_caption_inner(
    win: &tauri::WebviewWindow,
    keep_resize: bool,
    backdrop: Backdrop,
) {
    use raw_window_handle::HasWindowHandle;

    let Ok(handle) = win.window_handle() else {
        tracing::warn!("dwm: couldn't get window handle");
        return;
    };
    let raw_window_handle::RawWindowHandle::Win32(h) = handle.as_raw() else {
        tracing::warn!("dwm: not a Win32 handle");
        return;
    };

    const GA_ROOT: u32 = 2;
    let inner = h.hwnd.get();
    let hwnd = unsafe { GetAncestor(inner, GA_ROOT) };
    let hwnd = if hwnd != 0 { hwnd } else { inner };
    tracing::info!("dwm: inner={inner:#x} root={hwnd:#x}");

    unsafe {
        // Windows 11 rounded corners for our borderless windows (so the top
        // doesn't read as a hard, cut-off rectangle).
        const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
        const DWMWCP_ROUND: u32 = 2;
        let corner: u32 = DWMWCP_ROUND;
        let r_corner = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &raw const corner as *const c_void,
            4,
        );
        tracing::info!("dwm: corner_round={r_corner:#x}");

        // Extend DWM frame fully into client area
        let margins = Margins {
            left: -1,
            right: -1,
            top: -1,
            bottom: -1,
        };
        let r3 = DwmExtendFrameIntoClientArea(hwnd, &margins);
        tracing::info!("dwm: extend_frame={r3:#x}");

        // Install subclass proc (safe for multiple windows)
        let ok = SetWindowSubclass(hwnd, borderless_subclass_proc, BORDERLESS_SUBCLASS_ID, 0);
        tracing::info!("dwm: subclass installed={ok}");

        // Remove WS_CAPTION; only strip WS_THICKFRAME for non-resizable windows
        const GWL_STYLE: i32 = -16;
        const WS_CAPTION: isize = 0x00C00000;
        const WS_THICKFRAME: isize = 0x00040000;
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let new_style = if keep_resize {
            style & !WS_CAPTION
        } else {
            style & !WS_CAPTION & !WS_THICKFRAME
        };
        if new_style != style {
            SetWindowLongPtrW(hwnd, GWL_STYLE, new_style);
            if keep_resize {
                tracing::info!("dwm: stripped WS_CAPTION (kept WS_THICKFRAME for resize)");
            } else {
                tracing::info!("dwm: stripped WS_CAPTION/WS_THICKFRAME");
            }
        }

        // Force frame recalculation
        const SWP_FRAMECHANGED: u32 = 0x0020;
        const SWP_NOMOVE: u32 = 0x0002;
        const SWP_NOSIZE: u32 = 0x0001;
        const SWP_NOZORDER: u32 = 0x0004;
        SetWindowPos(
            hwnd,
            0,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
        );
    }

    apply_backdrop(win, backdrop);
}

#[cfg(not(windows))]
pub fn force_borderless_caption(win: &tauri::WebviewWindow, backdrop: Backdrop) {
    apply_backdrop(win, backdrop);
}

#[cfg(not(windows))]
pub fn force_borderless_caption_resizable(win: &tauri::WebviewWindow, backdrop: Backdrop) {
    apply_backdrop(win, backdrop);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--app-bg` (`#rrggbb`) inside the `[data-theme="..."]` block of `styles.css`.
    fn css_app_background(theme: &str) -> tauri::window::Color {
        let css = include_str!("../../../src/styles.css");
        let selector = format!("[data-theme=\"{theme}\"] {{");
        let start = css.find(&selector).expect("theme block") + selector.len();
        let block = &css[start..start + css[start..].find('}').expect("block end")];
        let value = block
            .split("--app-bg:")
            .nth(1)
            .expect("--app-bg in theme block")
            .trim_start();
        let hex = value.strip_prefix('#').expect("hex colour");
        let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex digit");
        tauri::window::Color(channel(0), channel(2), channel(4), 255)
    }

    #[test]
    fn backdrop_color_matches_app_background_per_theme() {
        assert_eq!(
            backdrop_color(tauri::Theme::Light),
            tauri::window::Color(0xF5, 0xF7, 0xFA, 255)
        );
        assert_eq!(
            backdrop_color(tauri::Theme::Dark),
            tauri::window::Color(0x1C, 0x20, 0x26, 255)
        );
    }

    #[test]
    fn backdrop_color_tracks_the_css_app_background() {
        assert_eq!(
            backdrop_color(tauri::Theme::Light),
            css_app_background("light")
        );
        assert_eq!(
            backdrop_color(tauri::Theme::Dark),
            css_app_background("dark")
        );
    }
}
