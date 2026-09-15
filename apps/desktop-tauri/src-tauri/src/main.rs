#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::time::Duration;

mod auto_refresh;
mod capacity_events;
mod commands;
mod enforcement;
mod events;
mod floatbar;
mod foreground;
mod geometry_store;
mod powertoys;
mod proof_harness;
mod provider_incidents;
mod quota_run_history;
mod shell;
mod shortcut_bridge;
mod state;
mod surface;
mod surface_target;
mod taskbar_widget;
mod tray_bridge;
mod tray_menu;
mod usage_history;
mod webview_recovery;
mod window_positioner;

use std::sync::Mutex;

use state::AppState;
use surface::SurfaceMode;
use surface_target::SurfaceTarget;
use tauri::Manager;

const PROOF_ACTIVATION_DELAY: Duration = Duration::from_millis(0);
const VISIBLE_START_ACTIVATION_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LaunchBehavior {
    open_primary_window_at_start: bool,
    suppress_blur_dismiss: bool,
}

fn should_hide_close_request(mode: SurfaceMode) -> bool {
    matches!(
        mode,
        SurfaceMode::TrayPanel | SurfaceMode::PopOut | SurfaceMode::Settings
    )
}

fn primary_window_request() -> shell::ShellTransitionRequest {
    shell::ShellTransitionRequest {
        mode: SurfaceMode::PopOut,
        target: SurfaceTarget::Dashboard,
        position: None,
    }
}

fn should_open_primary_window_from_args<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|arg| {
        let normalized = arg
            .as_ref()
            .trim()
            .trim_start_matches(['-', '/'])
            .replace(['-', '_'], "")
            .to_ascii_lowercase();
        matches!(normalized.as_str(), "menubar" | "traypanel" | "tray")
    })
}

fn nonblank_launch_args<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .map(|arg| arg.as_ref().trim().to_string())
        .filter(|arg| !arg.is_empty())
        .collect()
}

fn should_reopen_primary_window_from_instance_args<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args = nonblank_launch_args(args);
    args.is_empty() || should_open_primary_window_from_args(&args)
}

fn launch_behavior<I, S>(force_visible: bool, start_minimized: bool, args: I) -> LaunchBehavior
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args = nonblank_launch_args(args);
    let explicit_primary_launch = should_open_primary_window_from_args(&args);
    let plain_desktop_launch = args.is_empty();

    LaunchBehavior {
        open_primary_window_at_start: force_visible
            || explicit_primary_launch
            || (plain_desktop_launch && !start_minimized),
        suppress_blur_dismiss: force_visible,
    }
}

fn should_suppress_blur_dismiss(launch: LaunchBehavior, proof_mode: bool) -> bool {
    launch.suppress_blur_dismiss || proof_mode
}

fn main() {
    codexbar::logging::init(false, false).expect("failed to initialize logging");

    let proof_config = proof_harness::ProofConfig::from_env();
    let is_proof_mode = proof_config.is_some();
    let force_start_visible = std::env::var_os("CODEXBAR_START_VISIBLE").is_some();
    let settings = codexbar::settings::Settings::load();
    let launch = launch_behavior(
        force_start_visible,
        settings.start_minimized,
        std::env::args().skip(1),
    );

    let mut initial_state = AppState::new();
    initial_state.proof_config = proof_config;

    tauri::Builder::default()
        .manage(Mutex::new(initial_state))
        .plugin(shortcut_bridge::plugin())
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if should_reopen_primary_window_from_instance_args(args.iter().skip(1)) {
                let request = primary_window_request();
                let _ =
                    shell::reopen_to_target(app, request.mode, request.target, request.position);
            }
        }))
        .invoke_handler(tauri::generate_handler![
            commands::get_bootstrap_state,
            commands::get_provider_catalog,
            commands::get_settings_snapshot,
            commands::list_agent_sessions,
            commands::focus_agent_session,
            commands::update_settings,
            commands::set_surface_mode,
            commands::dismiss_tray_panel,
            taskbar_widget::get_taskbar_surface_color,
            taskbar_widget::get_taskbar_widget_status,
            commands::hide_dashboard_to_tray,
            commands::begin_flyout_gesture,
            commands::end_flyout_gesture,
            commands::reveal_tray_panel_window,
            commands::open_settings_window,
            commands::open_flyout_window,
            commands::close_settings_window,
            commands::set_flyout_size,
            commands::flyout_stored_size,
            commands::get_current_surface_mode,
            commands::get_current_surface_state,
            commands::get_proof_state,
            commands::run_proof_command,
            commands::refresh_providers,
            commands::refresh_providers_if_stale,
            commands::get_cached_providers,
            commands::get_safe_diagnostics,
            commands::get_credential_storage_status,
            commands::get_update_state,
            commands::check_for_updates,
            commands::download_update,
            commands::apply_update,
            commands::dismiss_update,
            commands::open_release_page,
            commands::get_api_keys,
            commands::get_api_key_providers,
            commands::set_api_key,
            commands::remove_api_key,
            commands::get_manual_cookies,
            commands::set_manual_cookie,
            commands::remove_manual_cookie,
            commands::get_token_account_providers,
            commands::get_token_accounts,
            commands::get_directory_accounts,
            commands::probe_account_directory,
            commands::add_directory_account,
            commands::remove_directory_account,
            commands::set_active_directory_account,
            commands::update_directory_account,
            commands::add_token_account,
            commands::remove_token_account,
            commands::set_active_token_account,
            commands::get_app_info,
            commands::get_provider_chart_data,
            commands::get_quota_run_history,
            commands::get_quota_run_efficiency,
            commands::get_provider_local_usage_summary,
            commands::get_local_api_value_totals,
            commands::get_local_activity_heatmap,
            commands::get_cursor_model_activity,
            commands::get_provider_incidents,
            commands::export_cost_csv,
            commands::reorder_providers,
            commands::set_provider_region,
            commands::get_provider_region,
            commands::get_provider_region_options,
            commands::set_provider_workspace_id,
            commands::get_provider_gateway_url,
            commands::set_provider_gateway_url,
            commands::get_provider_workspace_id,
            commands::get_gemini_cli_signed_in,
            commands::get_detected_provider_accounts,
            commands::get_vertexai_status,
            commands::list_jetbrains_detected_ides,
            commands::set_jetbrains_ide_path,
            commands::get_kiro_status,
            commands::validate_global_shortcut,
            commands::is_remote_session,
            commands::get_launch_block_reason,
            commands::get_work_area_rect,
            commands::play_notification_sound,
            commands::send_test_notification,
            commands::open_external_url,
            commands::reanchor_tray_panel,
            commands::quit_app,
            commands::open_provider_dashboard,
            commands::open_provider_status_page,
            commands::get_provider_detail,
            commands::trigger_provider_login,
            commands::revoke_provider_credentials,
            commands::get_available_languages,
            commands::get_locale_strings,
            commands::set_ui_language,
            commands::open_path,
            foreground::get_foreground_provider,
            floatbar::show_float_bar,
            floatbar::hide_float_bar,
            floatbar::set_float_bar_opacity,
            floatbar::set_float_bar_click_through,
            floatbar::resize_float_bar,
            floatbar::set_float_bar_orientation,
        ])
        .setup(move |app| {
            match codexbar::browser::remove_legacy_cookie_caches() {
                Ok(removed) if removed > 0 => {
                    tracing::info!(removed, "Removed legacy plaintext cookie caches");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "Failed to remove legacy plaintext cookie caches");
                }
            }
            if let Some(window) = app.get_webview_window("main") {
                shell::dwm::force_dark_caption(&window);
                window.hide()?;
            }
            tray_bridge::setup(app)?;
            shortcut_bridge::register(app.handle());
            floatbar::install(app.handle());
            taskbar_widget::install(app.handle());
            auto_refresh::install(app.handle().clone());
            commands::prewarm_local_scan_caches(app.handle().clone());
            if settings.powertoys_status_pipe_enabled {
                powertoys::install(app.handle().clone());
            }

            // Give the WebView/event loop one turn to finish startup before
            // routing shortcut launches into the tray panel. Without this, the
            // Windows shell can leave only Tauri's tiny internal window visible.
            if is_proof_mode {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(PROOF_ACTIVATION_DELAY).await;
                    proof_harness::activate(&app_handle);
                });
            } else if launch.open_primary_window_at_start {
                let app = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(VISIBLE_START_ACTIVATION_DELAY).await;
                    let request = primary_window_request();
                    let _ = shell::reopen_to_target(
                        &app,
                        request.mode,
                        request.target,
                        request.position,
                    );
                });
            }

            Ok(())
        })
        .on_window_event(move |window, event| {
            if floatbar::handle_window_event(window, event) {
                return;
            }
            if shell::flyout_window::handle_window_event(window, event) {
                return;
            }
            if shell::settings_window::handle_window_event(window, event) {
                return;
            }
            // Only the main window participates in blur-dismiss and close-to-hide.
            // The detached settings window uses normal OS close behavior.
            if window.label() != "main" {
                return;
            }
            match event {
                tauri::WindowEvent::Focused(false) => {
                    // Suppress blur-dismiss in proof mode so the window stays
                    // visible for automated screenshot capture.
                    if should_suppress_blur_dismiss(
                        launch,
                        proof_harness::is_proof_mode(window.app_handle()),
                    ) {
                        return;
                    }
                    if let Some(st) = window.app_handle().try_state::<Mutex<AppState>>()
                        && st
                            .lock()
                            .unwrap()
                            .take_startup_tray_blur_grace(std::time::Instant::now())
                    {
                        return;
                    }
                    // Grace period: ignore blur within 500ms of showing the panel.
                    // On Windows, the tray click can cause a spurious blur before
                    // the window fully acquires focus.
                    if let Some(st) = window.app_handle().try_state::<Mutex<AppState>>()
                        && st.lock().unwrap().was_tray_panel_recently_shown(
                            std::time::Instant::now(),
                            Duration::from_millis(500),
                        )
                    {
                        return;
                    }
                    // Gesture guard: ignore blur while a resize-grip drag or
                    // HTML5 drag-reorder is running its Win32/OLE modal loop.
                    // Windows produces a spurious Focused(false) the instant
                    // such a loop starts even though the user never left the
                    // window; see AppState::begin_gesture_blur_guard.
                    if let Some(st) = window.app_handle().try_state::<Mutex<AppState>>()
                        && st
                            .lock()
                            .unwrap()
                            .is_gesture_blur_guard_active(std::time::Instant::now())
                    {
                        return;
                    }
                    // Blur in TrayPanel mode auto-hides the panel. Tray
                    // left-click now opens the dashboard, so no same-click
                    // reopen marker is needed.
                    let _ = shell::hide_to_tray_if_current(window.app_handle(), |mode| {
                        mode == SurfaceMode::TrayPanel
                    });
                }
                tauri::WindowEvent::Focused(true) => {
                    // A genuine refocus (after the gesture's own focus flicker
                    // has settled) re-arms the gesture guard so a later
                    // outside-click blur dismisses immediately again.
                    if let Some(st) = window.app_handle().try_state::<Mutex<AppState>>() {
                        st.lock()
                            .unwrap()
                            .clear_gesture_guard_on_refocus(std::time::Instant::now());
                    }
                }
                tauri::WindowEvent::Resized(_) if window.is_minimized().unwrap_or(false) => {
                    // Native taskbar minimization must behave like the custom
                    // minimize control: remove the dashboard from the taskbar
                    // and keep Ceiling alive in the system tray.
                    let _ = shell::hide_to_tray(window.app_handle());
                }
                tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                    // Capture geometry for surfaces eligible for persistence.
                    // The helper is a no-op when the current surface is not eligible.
                    shell::remember_current_geometry_if_eligible(window);
                }
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    // Never destroy the main window — Ceiling is a tray app.
                    // Prefer a proper hide transition; fall back to hide() if needed.
                    match shell::hide_to_tray_if_current(
                        window.app_handle(),
                        should_hide_close_request,
                    ) {
                        Ok(Some(_)) => api.prevent_close(),
                        Ok(None) | Err(_) => {
                            api.prevent_close();
                            let _ = window.hide();
                        }
                    }
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to build Ceiling desktop shell")
        .run(|_app, event| {
            // Tray apps must survive accidental last-window close. `app.exit(code)`
            // (Quit menu / quit_app) sets Some(code) and is allowed through.
            if let tauri::RunEvent::ExitRequested {
                api, code: None, ..
            } = event
            {
                tracing::debug!("Keeping Ceiling alive after window close (tray mode)");
                api.prevent_exit();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command in `generate_handler!` must also be allowed in
    /// `permissions/commands.toml`.
    ///
    /// Registering a command without allowlisting it compiles, passes every
    /// other test, and then fails at runtime with "not allowed by ACL" the
    /// moment a user opens the surface that calls it. That shipped once: all six
    /// config-directory account commands were registered but not allowed, so the
    /// entire Accounts tab rendered an error in 1.5.0.
    #[test]
    fn every_registered_command_is_allowed_by_the_acl() {
        const MAIN_RS: &str = include_str!("main.rs");
        const COMMANDS_TOML: &str = include_str!("../permissions/commands.toml");

        let handler = MAIN_RS
            .split_once("tauri::generate_handler![")
            .expect("invoke handler")
            .1
            .split_once("])")
            .expect("end of handler")
            .0;

        let registered: Vec<&str> = handler
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .filter_map(|line| line.trim_end_matches(',').rsplit("::").next())
            .filter(|name| !name.is_empty())
            .collect();

        assert!(
            registered.len() > 30,
            "parsed too few commands ({}); the handler format probably changed              and this test is no longer checking anything",
            registered.len()
        );

        let missing: Vec<&str> = registered
            .iter()
            .copied()
            .filter(|name| !COMMANDS_TOML.contains(&format!("\"{name}\"")))
            .collect();

        assert!(
            missing.is_empty(),
            "these commands are registered but not allowed in              permissions/commands.toml, so calling them fails at runtime: {missing:?}"
        );
    }

    #[test]
    fn close_request_hides_tray_first_surfaces() {
        assert!(should_hide_close_request(SurfaceMode::TrayPanel));
        assert!(should_hide_close_request(SurfaceMode::PopOut));
        assert!(should_hide_close_request(SurfaceMode::Settings));
    }

    #[test]
    fn close_request_leaves_hidden_surface_alone() {
        // Hidden is not a "hide transition" target; main CloseRequested still
        // prevents destruction and just hides the window (see main.rs).
        assert!(!should_hide_close_request(SurfaceMode::Hidden));
    }

    #[test]
    fn primary_window_request_targets_popout_dashboard() {
        let request = primary_window_request();
        assert_eq!(request.mode, SurfaceMode::PopOut);
        assert_eq!(request.target, SurfaceTarget::Dashboard);
        assert_eq!(request.position, None);
    }

    #[test]
    fn menubar_launch_arg_opens_primary_window() {
        assert!(should_open_primary_window_from_args(["menubar"]));
        assert!(should_open_primary_window_from_args(["--tray-panel"]));
        assert!(should_open_primary_window_from_args(["/tray_panel"]));
    }

    #[test]
    fn unrelated_launch_args_do_not_open_primary_window() {
        assert!(!should_open_primary_window_from_args([
            "usage", "-p", "claude"
        ]));
        assert!(!should_reopen_primary_window_from_instance_args([
            "usage", "-p", "claude"
        ]));
        assert_eq!(
            launch_behavior(false, false, ["usage", "-p", "claude"]),
            LaunchBehavior {
                open_primary_window_at_start: false,
                suppress_blur_dismiss: false,
            }
        );
    }

    #[test]
    fn plain_desktop_launch_opens_unless_start_minimized() {
        assert_eq!(
            launch_behavior(false, false, std::iter::empty::<&str>()),
            LaunchBehavior {
                open_primary_window_at_start: true,
                suppress_blur_dismiss: false,
            }
        );
        assert_eq!(
            launch_behavior(false, false, [""]),
            LaunchBehavior {
                open_primary_window_at_start: true,
                suppress_blur_dismiss: false,
            }
        );
        assert_eq!(
            launch_behavior(false, false, ["  "]),
            LaunchBehavior {
                open_primary_window_at_start: true,
                suppress_blur_dismiss: false,
            }
        );
        assert_eq!(
            launch_behavior(false, true, std::iter::empty::<&str>()),
            LaunchBehavior {
                open_primary_window_at_start: false,
                suppress_blur_dismiss: false,
            }
        );
    }

    #[test]
    fn single_instance_plain_launch_reopens_primary_window() {
        assert!(should_reopen_primary_window_from_instance_args(
            std::iter::empty::<&str>()
        ));
        assert!(should_reopen_primary_window_from_instance_args([""]));
        assert!(should_reopen_primary_window_from_instance_args(["  "]));
        assert!(should_reopen_primary_window_from_instance_args(["menubar"]));
    }

    #[test]
    fn menubar_launch_does_not_suppress_blur_dismiss() {
        assert_eq!(
            launch_behavior(false, true, ["menubar"]),
            LaunchBehavior {
                open_primary_window_at_start: true,
                suppress_blur_dismiss: false,
            }
        );
    }

    #[test]
    fn automation_launch_opens_and_suppresses_blur_dismiss() {
        let launch = launch_behavior(true, true, std::iter::empty::<&str>());
        assert_eq!(
            launch,
            LaunchBehavior {
                open_primary_window_at_start: true,
                suppress_blur_dismiss: true,
            }
        );
        assert!(should_suppress_blur_dismiss(launch, false));
    }

    #[test]
    fn proof_mode_suppresses_blur_dismiss() {
        let launch = launch_behavior(false, true, std::iter::empty::<&str>());
        assert!(should_suppress_blur_dismiss(launch, true));
    }

    #[test]
    fn visible_start_delays_stay_short() {
        assert_eq!(PROOF_ACTIVATION_DELAY, Duration::ZERO);
        assert!(VISIBLE_START_ACTIVATION_DELAY <= Duration::from_millis(500));
    }
}
