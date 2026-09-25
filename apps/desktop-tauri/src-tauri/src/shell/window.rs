//! Window-property application and the hide-to-tray flow.

use std::sync::Mutex;

use tauri::{AppHandle, Manager, WebviewWindow};

use crate::state::AppState;
use crate::surface::{SurfaceMode, SurfaceTransition, WindowProperties};
use crate::surface_target::SurfaceTarget;

use super::SHELL_TRANSITION_SERIAL;
use super::transition::{SurfaceSnapshot, apply_transition, current_surface_snapshot};

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    fn GetAncestor(hwnd: isize, flags: u32) -> isize;
    fn ShowWindow(hwnd: isize, command: i32) -> i32;
    fn BringWindowToTop(hwnd: isize) -> i32;
    fn SetForegroundWindow(hwnd: isize) -> i32;
}

pub(super) struct HideToTrayPlan {
    pub previous: SurfaceSnapshot,
    pub transition: Option<SurfaceTransition>,
    pub target: SurfaceTarget,
}

/// Apply the window properties dictated by a surface mode.
pub fn apply_window_properties(
    window: &WebviewWindow,
    mode: SurfaceMode,
    props: &WindowProperties,
) -> Result<(), String> {
    let needs_show = apply_window_layout(window, mode, props)?;
    if needs_show {
        show_window(window)?;
    }
    Ok(())
}

/// Apply layout properties (decorations, size, always-on-top) WITHOUT making
/// the window visible.  Returns `true` when the caller should subsequently
/// call [`show_window`] to make it visible, or `false` when the mode hides
/// the window (already handled internally).
pub fn apply_window_layout(
    window: &WebviewWindow,
    mode: SurfaceMode,
    props: &WindowProperties,
) -> Result<bool, String> {
    let map_err = |e: tauri::Error| e.to_string();

    window.set_decorations(props.decorations).map_err(map_err)?;
    window.set_resizable(props.resizable).map_err(map_err)?;
    window
        .set_always_on_top(props.always_on_top)
        .map_err(map_err)?;
    window
        .set_skip_taskbar(props.skip_taskbar)
        .map_err(map_err)?;
    // Borderless surfaces draw their own chrome via a DWM subclass that zeros
    // the native non-client area. Apply it AFTER `set_resizable` so the
    // resizable variant sees WS_THICKFRAME present and preserves it (keeping
    // the native resize affordance). Native decorations are incompatible with
    // this subclass, so decorated surfaces skip it.
    if !props.decorations {
        if props.resizable {
            super::dwm::force_dark_caption_resizable(window);
        } else {
            super::dwm::force_dark_caption(window);
        }
    }

    if props.visible {
        // The flyout ("Pop Out Dashboard") window is sized entirely by the
        // frontend (content auto-fit, or the user's remembered size applied
        // on open via `flyout_window::open_or_focus` + `set_flyout_size`) —
        // it is its own dedicated window now, never `main`, so this function
        // (which only ever runs against `main`'s window; see callers in
        // `shell/transition.rs`) needs no special-case for it. `main`'s
        // surface machine only ever holds Hidden/PopOut/Settings, none of
        // which race a frontend-owned size the way the old shared-window
        // TrayPanel mode did — so the backend is the sole size-setter here,
        // unconditionally, for every mode this function can be called with.
        //
        // The canonical surface mode drives geometry restore — no brittle
        // shape-matching of WindowProperties. logical_size_from_geometry
        // falls back to the mode's default size for non-remembered modes.
        let (width, height) =
            logical_size_from_geometry(mode, props, crate::geometry_store::load(mode));
        let (width, height) =
            capped_logical_size(window, width, height, props.min_width, props.min_height);
        let size = tauri::LogicalSize::new(width, height);

        // Arm geometry-capture suppression immediately before the programmatic
        // set_size (and the transition's subsequent position apply) so their OS
        // resize/move events are not persisted as if the user had dragged the
        // window (SOU-222). Armed here — after the geometry load/cap above — so
        // the 750 ms window starts at the actual mutation, not before slow prep,
        // and set_size can't run after the guard has already expired.
        if let Some(st) = window.app_handle().try_state::<Mutex<AppState>>()
            && let Ok(mut guard) = st.lock()
        {
            guard.arm_geometry_capture_suppression(
                std::time::Instant::now() + std::time::Duration::from_millis(750),
            );
        }

        window.set_size(size).map_err(map_err)?;

        if let (Some(min_w), Some(min_h)) = (props.min_width, props.min_height) {
            window
                .set_min_size(Some(tauri::LogicalSize::new(min_w, min_h)))
                .map_err(map_err)?;
        } else {
            window
                .set_min_size::<tauri::LogicalSize<f64>>(None)
                .map_err(map_err)?;
        }

        Ok(true) // caller should show
    } else {
        window.hide().map_err(map_err)?;
        Ok(false)
    }
}

pub(super) fn logical_size_from_geometry(
    mode: SurfaceMode,
    props: &WindowProperties,
    stored: Option<crate::geometry_store::StoredGeometry>,
) -> (f64, f64) {
    if !crate::geometry_store::should_remember(mode) {
        return (props.width, props.height);
    }

    let width = stored
        .and_then(|geometry| geometry.width)
        .map(|width| width.max(1) as f64)
        .unwrap_or(props.width);
    let height = stored
        .and_then(|geometry| geometry.height)
        .map(|height| height.max(1) as f64)
        .unwrap_or(props.height);

    (
        props.min_width.map_or(width, |min| width.max(min)),
        props.min_height.map_or(height, |min| height.max(min)),
    )
}

fn capped_logical_size(
    window: &WebviewWindow,
    width: f64,
    height: f64,
    min_width: Option<f64>,
    min_height: Option<f64>,
) -> (f64, f64) {
    const MARGIN: f64 = 16.0;

    let Some(monitor) = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
    else {
        return (width, height);
    };

    let scale = monitor.scale_factor();
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let work_area = monitor.work_area();
    cap_to_available(
        width,
        height,
        work_area.size.width as f64 / scale - MARGIN,
        work_area.size.height as f64 / scale - MARGIN,
        min_width,
        min_height,
    )
}

/// Cap a logical size to the available work area, but never below the window's
/// own minimum. Capping below the minimum makes `set_size` request a
/// sub-minimum size that the OS clamps back up to the minimum — and that clamp
/// used to be persisted as the remembered size, collapsing the window
/// permanently (SOU-222). Modes without a minimum keep a small 320x240 floor.
fn cap_to_available(
    width: f64,
    height: f64,
    available_width: f64,
    available_height: f64,
    min_width: Option<f64>,
    min_height: Option<f64>,
) -> (f64, f64) {
    let max_width = available_width.max(min_width.unwrap_or(320.0));
    let max_height = available_height.max(min_height.unwrap_or(240.0));
    (width.min(max_width), height.min(max_height))
}

/// Make the window visible and give it input focus.
pub fn show_window(window: &WebviewWindow) -> Result<(), String> {
    let map_err = |e: tauri::Error| e.to_string();
    let was_minimized = window.is_minimized().unwrap_or(false);
    if was_minimized {
        window.unminimize().map_err(map_err)?;
    }
    window.show().map_err(map_err)?;
    window.set_focus().map_err(map_err)?;
    request_native_foreground(window, was_minimized);
    Ok(())
}

/// Tauri's cross-platform `set_focus` can return success while Windows leaves
/// an already-visible window behind the current foreground application. A tray
/// icon click is a direct user gesture, so ask Win32 to activate the root HWND
/// as the final reveal step. Failures remain best-effort because Windows may
/// still enforce foreground-lock policy in unusual desktop/session states.
#[cfg(windows)]
fn request_native_foreground(window: &WebviewWindow, was_minimized: bool) {
    use raw_window_handle::HasWindowHandle;

    let Ok(handle) = window.window_handle() else {
        tracing::debug!("shell: no native window handle available for foreground activation");
        return;
    };
    let raw_window_handle::RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };

    const GA_ROOT: u32 = 2;
    const SW_RESTORE: i32 = 9;
    let inner = handle.hwnd.get();
    let root = unsafe { GetAncestor(inner, GA_ROOT) };
    let hwnd = if root != 0 { root } else { inner };

    unsafe {
        if was_minimized {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let raised = BringWindowToTop(hwnd) != 0;
        let foreground = SetForegroundWindow(hwnd) != 0;
        if !raised || !foreground {
            tracing::debug!(
                raised,
                foreground,
                "shell: Windows did not fully grant foreground activation"
            );
        }
    }
}

#[cfg(not(windows))]
fn request_native_foreground(_window: &WebviewWindow, _was_minimized: bool) {}

pub fn hide_to_tray(app: &AppHandle) -> Result<SurfaceMode, String> {
    hide_to_tray_if_current(app, |_| true).map(|mode| mode.unwrap_or(SurfaceMode::Hidden))
}

pub fn hide_to_tray_if_current<P>(
    app: &AppHandle,
    is_eligible: P,
) -> Result<Option<SurfaceMode>, String>
where
    P: FnOnce(SurfaceMode) -> bool,
{
    let _transition_guard = SHELL_TRANSITION_SERIAL.lock().unwrap();
    let st = app
        .try_state::<Mutex<AppState>>()
        .ok_or_else(|| "app state unavailable".to_string())?;
    let plan = {
        let mut guard = st.lock().unwrap();
        prepare_hide_to_tray_if_current(&mut guard, is_eligible)
    };

    let Some(plan) = plan else {
        return Ok(None);
    };

    // The state now reads Hidden. A dead `main` is rebuilt in the background
    // and comes back hidden, which is exactly what this path wanted, so
    // there is nothing to replay; an open queued by another caller during
    // the rebuild is kept (#410). Probed after the eligibility check so an
    // event this path ignores never starts a rebuild on its own.
    let Some(window) = super::window_recovery::resolve_live_main(app, None) else {
        return Ok(Some(SurfaceMode::Hidden));
    };

    if let Some(transition) = plan.transition {
        apply_transition(app, &window, &transition, &plan.previous, plan.target, None).map(Some)
    } else {
        let _ = window.hide();
        Ok(Some(SurfaceMode::Hidden))
    }
}

#[allow(dead_code)]
pub fn hide_to_tray_state(state: &mut AppState) {
    let _ = prepare_hide_to_tray_if_current(state, |_| true);
}

pub(super) fn prepare_hide_to_tray_if_current<P>(
    state: &mut AppState,
    is_eligible: P,
) -> Option<HideToTrayPlan>
where
    P: FnOnce(SurfaceMode) -> bool,
{
    let current = state.surface_machine.current();
    if !is_eligible(current) {
        return None;
    }

    let previous = current_surface_snapshot(state);
    let transition = state.hide_surface();
    Some(HideToTrayPlan {
        previous,
        transition,
        target: state.current_target.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::cap_to_available;

    #[test]
    fn cap_never_shrinks_below_minimum() {
        // A transient tiny work-area read must not cap a 720x560 window below
        // its 520x400 minimum (SOU-222). The cap floors to the minimum, so
        // set_size never requests a sub-minimum size the OS clamps + persists.
        assert_eq!(
            cap_to_available(720.0, 560.0, 100.0, 80.0, Some(520.0), Some(400.0)),
            (520.0, 400.0)
        );
    }

    #[test]
    fn cap_applies_available_when_between_min_and_requested() {
        assert_eq!(
            cap_to_available(720.0, 560.0, 640.0, 500.0, Some(520.0), Some(400.0)),
            (640.0, 500.0)
        );
    }

    #[test]
    fn cap_keeps_requested_size_when_it_fits() {
        assert_eq!(
            cap_to_available(720.0, 560.0, 1904.0, 1040.0, Some(520.0), Some(400.0)),
            (720.0, 560.0)
        );
    }

    #[test]
    fn cap_uses_default_floor_for_min_less_modes() {
        // Modes without an explicit minimum keep the 320x240 baseline floor.
        assert_eq!(
            cap_to_available(1000.0, 800.0, 50.0, 50.0, None, None),
            (320.0, 240.0)
        );
    }
}
