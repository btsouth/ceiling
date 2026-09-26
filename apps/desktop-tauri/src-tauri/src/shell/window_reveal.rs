//! One-shot first-layout reveals for the main and detached Settings windows.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, WebviewWindow};

use crate::state::{AppState, RevealWindow};

/// A second open can recover a window whose frontend never reported ready.
/// The first open waits for layout; the next open after this grace shows the
/// window so its loading or recovery UI remains reachable.
const REVEAL_GRACE: Duration = Duration::from_secs(3);

fn app_state(app: &AppHandle) -> Result<tauri::State<'_, Mutex<AppState>>, String> {
    app.try_state::<Mutex<AppState>>()
        .ok_or_else(|| "app state unavailable".to_string())
}

pub(super) fn arm(app: &AppHandle, target: RevealWindow, native_ready: bool) -> Result<(), String> {
    app_state(app)?
        .lock()
        .map_err(|error| error.to_string())?
        .arm_window_reveal(target, Instant::now(), native_ready);
    Ok(())
}

pub(super) fn cancel(app: &AppHandle, target: RevealWindow) {
    if let Ok(state) = app_state(app) {
        let guard = state.lock();
        if let Ok(mut guard) = guard {
            guard.take_window_reveal(target);
        }
    }
}

/// Whether an open must wait for the frontend. A late repeat open can still
/// reveal a usable window if the frontend did not acknowledge the first one.
pub(super) fn should_defer(app: &AppHandle, target: RevealWindow) -> bool {
    let Ok(state) = app_state(app) else {
        return false;
    };
    let Ok(mut guard) = state.lock() else {
        return false;
    };
    let Some(waited) = guard.window_reveal_pending_for(target, Instant::now()) else {
        return false;
    };
    if !guard.window_reveal_is_native_ready(target) || waited < REVEAL_GRACE {
        return true;
    }
    tracing::warn!(
        ?target,
        waited_ms = waited.as_millis(),
        "window first reveal never arrived; showing on repeat open"
    );
    guard.take_window_reveal(target);
    false
}

/// Called after a Settings build has its caption and position applied. The
/// frontend can reach the command during `build()`, so retain that signal
/// until this native setup is complete.
pub(super) fn native_ready(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    let should_show = app_state(app)?
        .lock()
        .map_err(|error| error.to_string())?
        .mark_window_native_ready(RevealWindow::Settings);
    if should_show {
        show(app, window, RevealWindow::Settings)?;
    }
    Ok(())
}

/// The invoking window identifies the target. Other surfaces have their own
/// reveal path, and a stale or repeated call has no pending token to consume.
pub(crate) fn frontend_ready(app: &AppHandle, window: &WebviewWindow) -> Result<(), String> {
    let target = match window.label() {
        super::window_recovery::MAIN_LABEL => RevealWindow::Main,
        super::settings_window::SETTINGS_LABEL => RevealWindow::Settings,
        _ => return Ok(()),
    };
    let should_show = app_state(app)?
        .lock()
        .map_err(|error| error.to_string())?
        .mark_window_frontend_ready(target);
    if should_show {
        show(app, window, target)?;
    }
    Ok(())
}

fn show(app: &AppHandle, window: &WebviewWindow, target: RevealWindow) -> Result<(), String> {
    match target {
        RevealWindow::Main => {
            super::window::show_window(window)?;
            let state = app_state(app)?;
            let mut guard = state.lock().map_err(|error| error.to_string())?;
            if guard.surface_machine.current() == crate::surface::SurfaceMode::TrayPanel {
                guard.mark_tray_panel_shown(Instant::now());
            }
        }
        RevealWindow::Settings => {
            window.show().map_err(|error| error.to_string())?;
            window.set_focus().map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
