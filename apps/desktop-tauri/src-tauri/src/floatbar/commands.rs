//! Tauri commands that drive the floating-bar window.
//!
//! These are thin wrappers around the local `window` module. User-initiated
//! changes keep persisted settings in sync, while the resize command applies a
//! new size and the native interaction state together.

use codexbar::settings::{Settings, clamp_float_bar_opacity, normalize_float_bar_orientation};
use tauri::{AppHandle, Manager};

use super::window as floatbar_window;

#[tauri::command]
pub async fn show_float_bar(app: AppHandle) -> Result<(), String> {
    let settings = Settings::update(|settings| settings.float_bar_enabled = true)
        .map_err(|e| e.to_string())?;

    // Showing the bar can be the first time foreground watching becomes
    // eligible (the setting may be enabled while the bar was hidden), so the
    // watcher has to be started here and not only from `after_settings_saved`.
    crate::foreground::apply_watch(&app, &settings);
    super::apply_state(&app, &settings)
}

#[tauri::command]
pub fn hide_float_bar(app: AppHandle) -> Result<(), String> {
    let settings = Settings::update(|settings| settings.float_bar_enabled = false)
        .map_err(|e| e.to_string())?;
    // Hiding disables the bar, which makes the watcher ineligible; without
    // this it would keep polling Win32 and emitting to a closed webview for
    // the rest of the process lifetime.
    crate::foreground::apply_watch(&app, &settings);
    floatbar_window::hide(&app)
}

#[tauri::command]
pub fn set_float_bar_opacity(app: AppHandle, opacity: u8) -> Result<(), String> {
    let opacity = clamp_float_bar_opacity(opacity);
    Settings::update(|settings| settings.float_bar_opacity = opacity).map_err(|e| e.to_string())?;

    if let Some(window) = app.get_webview_window(floatbar_window::FLOATBAR_LABEL) {
        floatbar_window::apply_no_activate(&window);
        floatbar_window::apply_opacity(&window, opacity);
        floatbar_window::apply_always_on_top(&window);
    }
    Ok(())
}

#[tauri::command]
pub fn set_float_bar_click_through(app: AppHandle, enabled: bool) -> Result<(), String> {
    Settings::update(|settings| settings.float_bar_click_through = enabled)
        .map_err(|e| e.to_string())?;

    if let Some(window) = app.get_webview_window(floatbar_window::FLOATBAR_LABEL) {
        floatbar_window::apply_no_activate(&window);
        floatbar_window::apply_click_through(&window, enabled);
        floatbar_window::apply_always_on_top(&window);
    }
    Ok(())
}

#[tauri::command]
pub fn resize_float_bar(app: AppHandle, width: f64, height: f64) -> Result<(), String> {
    let settings = Settings::load();
    if let Some(window) = app.get_webview_window(floatbar_window::FLOATBAR_LABEL) {
        // One native operation owns the resize + interaction-state invariant,
        // so the webview never has to repair Win32 window styles itself.
        floatbar_window::resize(&window, width, height, settings.float_bar_click_through)?;
    }
    Ok(())
}

#[tauri::command]
pub fn set_float_bar_orientation(app: AppHandle, orientation: String) -> Result<(), String> {
    let orientation = normalize_float_bar_orientation(&orientation);
    Settings::update(|settings| settings.float_bar_orientation = orientation.clone())
        .map_err(|e| e.to_string())?;

    // The webview re-reads orientation from settings on every config
    // change — emit the live-update event so it re-lays-out without us
    // needing to destroy/recreate the window (which races with Tauri's
    // async window lifecycle and can crash on Windows).
    use tauri::Emitter;
    let _ = app.emit(super::FLOAT_BAR_CONFIG_CHANGED_EVENT, ());
    Ok(())
}
