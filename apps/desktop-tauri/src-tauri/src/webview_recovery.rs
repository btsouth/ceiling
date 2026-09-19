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
//! The `main` window cannot use [`reclaim_dead_window`]: its open path runs
//! from synchronous Tauri commands, where building a window deadlocks on
//! Windows. `shell::window_recovery` rebuilds it on a background thread
//! instead, using [`is_webview_alive`] and [`destroy_and_release`] from here;
//! `shell::webview_lifecycle` drives the same rebuilds from WebView2's own
//! `ProcessFailed` event so a window that is on screen when its browser
//! process dies does not have to wait for the next open.

use std::time::Duration;

use tauri::{Manager, WebviewWindow};

/// How many times `destroy_and_release` re-checks the label before giving up
/// on the rebuild, and the pause between checks. Counted in polls rather than
/// wall-clock time on purpose: each check may block on the event loop, and
/// while another window is being built on the main thread (a `ProcessFailed`
/// burst rebuilds several) that can be longer than any sane timeout. Time
/// spent blocked is not time the label was overdue.
const LABEL_RELEASE_POLLS: usize = 200;
const LABEL_RELEASE_POLL: Duration = Duration::from_millis(10);

/// Class-name prefixes WebView2 gives the windows it creates under a Tauri
/// window. The render host paints the actual content.
fn class_is_webview(class_name: &str) -> bool {
    class_name.starts_with("Chrome_")
}

/// The window's Win32 handle, or `None` when it no longer exposes one. A
/// destroyed window keeps its label until the event loop processes
/// `Destroyed`, but its native handle is already gone by then.
///
/// Off the main thread this is a marshalled getter that waits on the event
/// loop; callers holding a lock the main thread may want must not use it.
#[cfg(windows)]
pub(crate) fn native_hwnd(window: &WebviewWindow) -> Option<isize> {
    use raw_window_handle::HasWindowHandle;

    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

#[cfg(not(windows))]
pub(crate) fn native_hwnd(_window: &WebviewWindow) -> Option<isize> {
    None
}

/// Whether the window behind `hwnd` still has live WebView2 content. Pure
/// Win32; safe from any thread and under any lock. A handle that is no
/// longer a window has no children and reads as dead.
#[cfg(windows)]
pub(crate) fn hwnd_has_webview_child(hwnd: isize) -> bool {
    win32::has_webview_child(hwnd)
}

#[cfg(not(windows))]
pub(crate) fn hwnd_has_webview_child(_hwnd: isize) -> bool {
    true
}

/// Whether `window` still has live WebView2 content.
///
/// Returns `true` when the check cannot be performed, so an unreadable window
/// handle never causes a spurious rebuild.
pub fn is_webview_alive(window: &WebviewWindow) -> bool {
    match native_hwnd(window) {
        Some(hwnd) => hwnd_has_webview_child(hwnd),
        None => true,
    }
}

/// Whether the label a destroyed window held (native handle `destroyed`) has
/// been given up, judged from what currently sits under it: `None` when no
/// window does, `Some(handle)` for the window that does.
///
/// Nothing under the label means it is free. A window with a *different*
/// handle means someone else already rebuilt it, so the label was free in
/// between and there is nothing left to wait for. The same handle, or a
/// window with no handle at all, is still the destroyed one on its way out.
fn label_released(destroyed: Option<isize>, current: Option<Option<isize>>) -> bool {
    match current {
        None => true,
        Some(Some(hwnd)) => Some(hwnd) != destroyed,
        Some(None) => false,
    }
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

/// Destroy `window` and block until Tauri has released its label, so the
/// caller can rebuild a window under the same label.
///
/// Tauri only unregisters the label when the event loop processes the
/// window's `Destroyed` event, which happens after `destroy()` has already
/// returned — rebuilding straight away fails with "a window with label `…`
/// already exists". A `destroy()` error is logged rather than propagated:
/// the window may already be half-destroyed (native handle gone, label still
/// registered), in which case the label is on its way out and waiting is
/// still the right move. Blocks the calling thread while it waits; like
/// `WebviewWindowBuilder::build`, it must never run on the main thread.
///
/// Returns as soon as the label is free *or* another window already occupies
/// it: an open racing this teardown (a widget click during a `ProcessFailed`
/// burst) can legitimately rebuild the window first, and that is not a
/// failure. See `label_released`.
pub fn destroy_and_release(app: &tauri::AppHandle, window: &WebviewWindow) -> Result<(), String> {
    let label = window.label().to_string();
    let destroyed = native_hwnd(window);
    if let Err(error) = window.destroy() {
        tracing::warn!(
            %error,
            label,
            "destroy() failed; waiting for the label to be released anyway"
        );
    }

    for _ in 0..LABEL_RELEASE_POLLS {
        let current = app
            .get_webview_window(&label)
            .map(|window| native_hwnd(&window));
        if label_released(destroyed, current) {
            return Ok(());
        }
        std::thread::sleep(LABEL_RELEASE_POLL);
    }
    Err(format!(
        "window `{label}` did not release its label after destroy"
    ))
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
    use super::{class_is_webview, label_released};

    #[test]
    fn a_label_with_nothing_under_it_is_released() {
        assert!(label_released(Some(0x10), None));
        assert!(label_released(None, None));
    }

    #[test]
    fn the_destroyed_window_still_under_its_label_is_not_released() {
        // Same handle: Tauri has not processed `Destroyed` yet.
        assert!(!label_released(Some(0x10), Some(Some(0x10))));
        // No handle at all: half-destroyed, label still registered.
        assert!(!label_released(Some(0x10), Some(None)));
        assert!(!label_released(None, Some(None)));
    }

    #[test]
    fn a_different_window_under_the_label_means_it_was_released_meanwhile() {
        // Observed live: a widget click rebuilt the flyout while the
        // ProcessFailed teardown was still waiting, and the wait timed out
        // against the *new* window. That rebuild is the desired outcome,
        // not a failure.
        assert!(label_released(Some(0x10), Some(Some(0x20))));
        assert!(label_released(None, Some(Some(0x20))));
    }

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
