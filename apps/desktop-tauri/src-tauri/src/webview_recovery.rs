//! Recovery for a window whose WebView2 process has exited.
//!
//! Windows keeps a Tauri window's native frame alive after the WebView2
//! browser and render processes die, but the client area then paints nothing:
//! a transparent, empty frame showing whatever is behind it. The flyout,
//! Settings, and FloatBar windows are hidden rather than closed, so that dead
//! frame was reused on every later open and the whole UI stayed blank until the
//! app was restarted (GitHub #410).
//!
//! A dead WebView2 controller cannot be revived, so the only recovery is to
//! drop the dead window and let its normal open path build a fresh one.
//! Detection walks the window's child HWNDs: a live webview always hosts a
//! `Chrome_*` render host, and a dead one has none.
//!
//! The `main` window is deliberately not handled here yet: its open path runs
//! from synchronous Tauri commands, where building a window deadlocks on
//! Windows, so rebuilding it needs an async path first.

use tauri::{Manager, WebviewWindow};

/// Class-name prefixes WebView2 gives the windows it creates under a Tauri
/// window. The render host paints the actual content.
fn class_is_webview(class_name: &str) -> bool {
    class_name.starts_with("Chrome_")
}

/// Whether `window` still has live WebView2 content.
///
/// Returns `true` when the check cannot be performed, so an unreadable window
/// handle never causes a spurious rebuild.
#[cfg(windows)]
pub fn is_webview_alive(window: &WebviewWindow) -> bool {
    use raw_window_handle::HasWindowHandle;

    let Ok(handle) = window.window_handle() else {
        return true;
    };
    let raw_window_handle::RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return true;
    };
    win32::has_webview_child(handle.hwnd.get())
}

#[cfg(not(windows))]
pub fn is_webview_alive(_window: &WebviewWindow) -> bool {
    true
}

/// Drop `label`'s window when its webview is dead, so the caller's normal
/// "window is missing -> build it" path runs.
///
/// Only safe to call from the same context that would build the window (these
/// windows are opened from async commands or spawned tasks); building from a
/// synchronous Tauri command deadlocks on Windows.
///
/// Returns `Ok(true)` when a dead window was reclaimed. A failure to destroy is
/// returned so the caller can surface it rather than showing the dead frame.
pub fn reclaim_dead_window(app: &tauri::AppHandle, label: &str) -> Result<bool, String> {
    let Some(window) = app.get_webview_window(label) else {
        return Ok(false);
    };
    if is_webview_alive(&window) {
        return Ok(false);
    }
    tracing::warn!(
        label,
        "webview exited under an existing window; dropping it so the next open rebuilds"
    );
    window.destroy().map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(windows)]
mod win32 {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumChildWindows(
            hwnd: isize,
            callback: Option<unsafe extern "system" fn(isize, isize) -> i32>,
            lparam: isize,
        ) -> i32;
        fn GetClassNameW(hwnd: isize, buffer: *mut u16, max_count: i32) -> i32;
    }

    struct Search {
        found: bool,
    }

    unsafe extern "system" fn visit(hwnd: isize, lparam: isize) -> i32 {
        let search = unsafe { &mut *(lparam as *mut Search) };
        let mut buffer = [0u16; 128];
        let len = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
        if len > 0 {
            let class_name = String::from_utf16_lossy(&buffer[..len as usize]);
            if super::class_is_webview(&class_name) {
                search.found = true;
                return 0; // stop enumeration
            }
        }
        1 // keep going
    }

    /// `EnumChildWindows` walks the entire descendant tree, so a single call
    /// covers the `WRY_WEBVIEW > Chrome_WidgetWin_* >
    /// Chrome_RenderWidgetHostHWND` chain.
    pub fn has_webview_child(hwnd: isize) -> bool {
        let mut search = Search { found: false };
        unsafe {
            EnumChildWindows(hwnd, Some(visit), &mut search as *mut Search as isize);
        }
        search.found
    }
}

#[cfg(test)]
mod tests {
    use super::class_is_webview;

    #[test]
    fn webview_classes_are_recognized_as_live_content() {
        assert!(class_is_webview("Chrome_WidgetWin_1"));
        assert!(class_is_webview("Chrome_RenderWidgetHostHWND"));
        assert!(class_is_webview("Chrome_WidgetWin_0"));
    }

    #[test]
    fn the_orphaned_frame_is_not_mistaken_for_content() {
        // After the browser process exits, the Tauri frame and the WRY webview
        // container both survive with no Chrome_* children. Treating either as
        // content would skip the rebuild forever, which is the #410 failure.
        assert!(!class_is_webview("WRY_WEBVIEW"));
        assert!(!class_is_webview("TAURI_DRAG_RESIZE_BORDERS"));
        assert!(!class_is_webview("Tauri Window"));
        assert!(!class_is_webview(""));
    }
}
