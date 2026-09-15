//! Detached Settings window: opens Settings/About in a separate window
//! so the tray panel stays open.

use tauri::{Emitter, Manager, PhysicalPosition, WebviewUrl};

use crate::surface::SurfaceMode;

const SETTINGS_LABEL: &str = "settings";
const SETTINGS_WIDTH: f64 = 720.0;
const SETTINGS_HEIGHT: f64 = 580.0;

/// Open the detached Settings window, or focus it if already open.
///
/// When the window already exists, emits `settings-change-tab` so the
/// frontend can switch to the requested tab without a full reload.
pub fn open_or_focus(app: &tauri::AppHandle, tab: &str) -> Result<(), String> {
    // See `webview_recovery`: a dead webview under a hidden frame must be
    // rebuilt, never shown empty (#410).
    crate::webview_recovery::reclaim_dead_window(app, SETTINGS_LABEL)?;

    if let Some(window) = app.get_webview_window(SETTINGS_LABEL) {
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
        app.emit_to(SETTINGS_LABEL, "settings-change-tab", tab)
            .map_err(|e| e.to_string())?;
        return Ok(());
    }

    let url = WebviewUrl::App(format!("index.html?window=settings&tab={tab}").into());

    let stored = crate::geometry_store::load(SurfaceMode::Settings);
    let width = stored
        .and_then(|geometry| geometry.width)
        .map_or(SETTINGS_WIDTH, f64::from);
    let height = stored
        .and_then(|geometry| geometry.height)
        .map_or(SETTINGS_HEIGHT, f64::from);

    let win = tauri::WebviewWindowBuilder::new(app, SETTINGS_LABEL, url)
        .title("Ceiling Settings")
        .inner_size(width, height)
        .decorations(false)
        .shadow(false)
        .theme(Some(tauri::Theme::Dark))
        .resizable(true)
        .build()
        .map_err(|e| e.to_string())?;

    // Force DWM caption to dark; keep WS_THICKFRAME since window is resizable
    super::dwm::force_dark_caption_resizable(&win);

    if let Some((x, y)) = super::position::default_surface_position(app, SurfaceMode::Settings) {
        let _ = win.set_position(PhysicalPosition::new(x, y));
    }

    Ok(())
}

/// Persist move and resize events for the detached Settings window.
pub fn handle_window_event(window: &tauri::Window, event: &tauri::WindowEvent) -> bool {
    if window.label() != SETTINGS_LABEL {
        return false;
    }

    if matches!(
        event,
        tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_)
    ) && !window.is_maximized().unwrap_or(false)
        && !window.is_minimized().unwrap_or(false)
        && let Ok(position) = window.outer_position()
    {
        let scale = window.scale_factor().unwrap_or(1.0).max(1.0);
        let logical_size = window.outer_size().ok().map(|size| {
            (
                (size.width as f64 / scale).round().max(1.0) as u32,
                (size.height as f64 / scale).round().max(1.0) as u32,
            )
        });
        crate::geometry_store::save(
            SurfaceMode::Settings,
            crate::geometry_store::StoredGeometry {
                x: position.x,
                y: position.y,
                width: logical_size.map(|size| size.0),
                height: logical_size.map(|size| size.1),
            },
        );
    }

    true
}

/// Dismiss Settings without exiting CodexBar.
///
/// The detached Settings window is hidden instead of closed so Tauri's
/// process/window lifecycle cannot interpret this as an app quit. If Settings
/// is rendered in the main shell surface, hide that surface back to tray.
pub fn dismiss(app: &tauri::AppHandle, window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.label() == SETTINGS_LABEL {
        return window.hide().map_err(|e| e.to_string());
    }

    crate::shell::hide_to_tray_if_current(app, |mode| {
        mode == crate::surface::SurfaceMode::Settings
    })?;
    Ok(())
}
