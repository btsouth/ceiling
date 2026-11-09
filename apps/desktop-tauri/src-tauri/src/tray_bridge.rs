//! System tray icon setup: left-click toggles the dashboard, right-click native menu.

use std::sync::Mutex;

use crate::commands::ProviderCatalogEntry;
use codexbar::core::ProviderId;
use codexbar::settings::{MetricPreference, Settings};
use tauri::image::Image;
use tauri::menu::{CheckMenuItemBuilder, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use codexbar::tray::render_ceiling_tray_icon_rgba;

use crate::shell;
use crate::state::{AppState, TrayAnchor};
use crate::surface::SurfaceMode;
use crate::surface_target::SurfaceTarget;
#[cfg(test)]
use crate::tray_menu::build_tray_menu;
use crate::tray_menu::{TrayMenuEntry, build_tray_menu_with};

#[derive(Debug, Clone, Copy)]
struct MonitorScaleInfo {
    physical_x: i32,
    physical_y: i32,
    physical_width: u32,
    physical_height: u32,
    scale_factor: f64,
}

impl MonitorScaleInfo {
    fn from_monitor(monitor: &tauri::Monitor) -> Self {
        let scale_factor = monitor.scale_factor();
        let safe_scale = if scale_factor.is_finite() && scale_factor > 0.0 {
            scale_factor
        } else {
            1.0
        };
        let position = monitor.position();
        let size = monitor.size();

        Self {
            physical_x: position.x,
            physical_y: position.y,
            physical_width: size.width,
            physical_height: size.height,
            scale_factor: safe_scale,
        }
    }
}

fn scale_factor_for_physical_point(x: f64, y: f64, monitors: &[MonitorScaleInfo]) -> Option<f64> {
    monitors
        .iter()
        .find(|monitor| {
            x >= monitor.physical_x as f64
                && x < (monitor.physical_x + monitor.physical_width as i32) as f64
                && y >= monitor.physical_y as f64
                && y < (monitor.physical_y + monitor.physical_height as i32) as f64
        })
        .map(|monitor| monitor.scale_factor)
}

fn logical_to_physical_anchor(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale_factor: f64,
) -> TrayAnchor {
    let safe_scale = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };

    TrayAnchor {
        x: (x * safe_scale).round() as i32,
        y: (y * safe_scale).round() as i32,
        width: ((width * safe_scale).round().max(1.0)) as u32,
        height: ((height * safe_scale).round().max(1.0)) as u32,
    }
}

fn resolve_tray_anchor(
    rect: &tauri::Rect,
    click_position: tauri::PhysicalPosition<f64>,
    monitors: &[MonitorScaleInfo],
) -> Option<TrayAnchor> {
    let click_scale = scale_factor_for_physical_point(click_position.x, click_position.y, monitors);

    match (rect.position, rect.size) {
        (tauri::Position::Physical(position), tauri::Size::Physical(size)) => Some(TrayAnchor {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        }),
        (tauri::Position::Logical(position), tauri::Size::Logical(size)) => {
            click_scale.map(|scale| {
                logical_to_physical_anchor(position.x, position.y, size.width, size.height, scale)
            })
        }
        (tauri::Position::Physical(position), tauri::Size::Logical(size)) => {
            click_scale.map(|scale| TrayAnchor {
                x: position.x,
                y: position.y,
                width: ((size.width * scale).round().max(1.0)) as u32,
                height: ((size.height * scale).round().max(1.0)) as u32,
            })
        }
        (tauri::Position::Logical(position), tauri::Size::Physical(size)) => {
            click_scale.map(|scale| TrayAnchor {
                x: (position.x * scale).round() as i32,
                y: (position.y * scale).round() as i32,
                width: size.width,
                height: size.height,
            })
        }
    }
}

fn build_native_tray_menu(
    app: &AppHandle,
    providers: &[ProviderCatalogEntry],
    status_labels: &[(String, String)],
) -> tauri::Result<Menu<tauri::Wry>> {
    let settings = Settings::load();
    let enabled = settings.enabled_providers.clone();
    let spec = build_tray_menu_with(
        providers,
        status_labels,
        &enabled,
        settings.taskbar_widget_enabled,
        settings.ui_language,
    );
    let entries = spec
        .iter()
        .map(|entry| build_native_menu_entry(app, entry))
        .collect::<tauri::Result<Vec<_>>>()?;
    let item_refs = entries
        .iter()
        .map(NativeMenuEntry::as_item)
        .collect::<Vec<_>>();

    Menu::with_items(app, &item_refs)
}

fn resolve_menu_target(id: &str) -> Option<shell::ShellTransitionRequest> {
    match id {
        // "Pop Out Dashboard" (and the legacy "Show Window") both open the full
        // dashboard: SurfaceMode::PopOut on `main`. Pop Out Dashboard is now the
        // primary, obviously-named entry point; the compact flyout is retired.
        "pop_out" | "show_panel" => Some(shell::ShellTransitionRequest {
            mode: SurfaceMode::PopOut,
            target: SurfaceTarget::Dashboard,
            position: None,
        }),
        _ if id.starts_with("provider:") => Some(shell::ShellTransitionRequest {
            mode: SurfaceMode::PopOut,
            target: SurfaceTarget::parse(id)?,
            position: None,
        }),
        _ => None,
    }
}

enum MenuAction {
    Transition(shell::ShellTransitionRequest),
    /// Open Settings/About in a detached window.
    OpenSettings(String),
    Refresh,
    CheckForUpdates,
    /// Toggle the enabled/disabled state of the provider with the given CLI name.
    ToggleProvider(String),
    /// Toggle the floating bar window on/off.
    ToggleFloatBar,
    Quit,
}

enum MenuTransitionDispatch {
    Transition(shell::ShellTransitionRequest),
    Reopen(shell::ShellTransitionRequest),
}

fn resolve_menu_action(id: &str) -> Option<MenuAction> {
    match id {
        "refresh" => Some(MenuAction::Refresh),
        "check_for_updates" => Some(MenuAction::CheckForUpdates),
        "quit" => Some(MenuAction::Quit),
        "settings" => Some(MenuAction::OpenSettings("general".into())),
        "more_providers" => Some(MenuAction::OpenSettings("providers".into())),
        "about" => Some(MenuAction::OpenSettings("about".into())),
        "toggle_float_bar" => Some(MenuAction::ToggleFloatBar),
        _ if id.starts_with("toggle_provider:") => {
            let provider_id = id["toggle_provider:".len()..].to_string();
            Some(MenuAction::ToggleProvider(provider_id))
        }
        _ => resolve_menu_target(id).map(MenuAction::Transition),
    }
}

fn resolve_menu_transition_dispatch(
    id: &str,
    request: shell::ShellTransitionRequest,
) -> MenuTransitionDispatch {
    if id == "show_panel" || id == "pop_out" {
        MenuTransitionDispatch::Reopen(shell::ShellTransitionRequest {
            mode: request.mode,
            target: request.target,
            position: None,
        })
    } else {
        MenuTransitionDispatch::Transition(request)
    }
}

/// Store the tray icon bounds from a click event into shared state.
fn store_anchor(app: &AppHandle, rect: &tauri::Rect, click_position: tauri::PhysicalPosition<f64>) {
    let monitors = app
        .get_webview_window("main")
        .and_then(|window| window.available_monitors().ok())
        .unwrap_or_default()
        .into_iter()
        .map(|monitor| MonitorScaleInfo::from_monitor(&monitor))
        .collect::<Vec<_>>();

    let Some(anchor) = resolve_tray_anchor(rect, click_position, &monitors) else {
        return;
    };

    if let Some(st) = app.try_state::<Mutex<AppState>>() {
        let mut guard = st.lock().unwrap();
        guard.tray_anchor = Some(anchor);
    }
}

/// Initialise the system tray icon, context menu, and event handlers.
///
/// - **Left-click** toggles the primary dashboard: show it when hidden, hide it
///   when it is already visible.
/// - **Right-click** opens the native context menu with shell actions.
pub fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let menu = build_native_tray_menu(app.handle(), &crate::commands::get_provider_catalog(), &[])?;

    // Use tray-specific transparent artwork from the first frame. The full
    // square app icon loses its silhouette when Windows downsamples it to 16px.
    let (rgba, width, height) = render_ceiling_tray_icon_rgba(0.0, false);
    let icon = Image::new_owned(rgba, width, height);

    let _tray = TrayIconBuilder::with_id("codexbar-main")
        .icon(icon)
        .tooltip("Ceiling")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state,
                position,
                rect,
                ..
            } = event
            {
                let app = tray.app_handle();
                if button == MouseButton::Left && button_state == MouseButtonState::Up {
                    store_anchor(app, &rect, position);
                    // Left-click toggles the dashboard (SurfaceMode::PopOut on
                    // `main`) — the same surface as "Pop Out Dashboard".
                    // `toggle_dashboard` only reopens via `reopen_to_target` on
                    // the existing `main` window (no WebviewWindowBuilder), so
                    // the sync-IPC build deadlock does not apply on this
                    // native event-loop callback.
                    shell::toggle_dashboard(app);
                }
            }
        })
        .on_menu_event(|app, event| {
            handle_menu_event(app, event.id().as_ref());
        })
        .build(app)?;

    Ok(())
}

/// Route a native menu-item click to the corresponding shell action.
fn handle_menu_event(app: &AppHandle, id: &str) {
    match resolve_menu_action(id) {
        Some(MenuAction::Transition(request)) => {
            match resolve_menu_transition_dispatch(id, request) {
                // Pass None so default_surface_position can use remembered PopOut
                // geometry first, then fall back to tray/current-monitor placement.
                MenuTransitionDispatch::Reopen(request) => {
                    let _ = shell::reopen_to_target(
                        app,
                        request.mode,
                        request.target,
                        request.position,
                    );
                }
                MenuTransitionDispatch::Transition(request) => {
                    let _ = shell::transition_to_target(
                        app,
                        request.mode,
                        request.target,
                        request.position,
                    );
                }
            }
        }
        Some(MenuAction::OpenSettings(tab)) => {
            let _ = shell::settings_window::open_or_focus(app, &tab);
        }
        Some(MenuAction::Refresh) => {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = crate::commands::do_refresh_providers(&handle).await;
            });
        }
        Some(MenuAction::CheckForUpdates) => {
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<Mutex<AppState>>();
                let _ = crate::commands::check_for_updates(handle.clone(), state).await;
            });
        }
        Some(MenuAction::ToggleProvider(provider_id)) => {
            let _ = Settings::update(|settings| {
                if settings.enabled_providers.contains(&provider_id) {
                    settings.enabled_providers.remove(&provider_id);
                } else {
                    settings.enabled_providers.insert(provider_id);
                }
            });
            crate::floatbar::notify_settings_changed(app);
            rebuild_tray_menu(app);
        }
        Some(MenuAction::ToggleFloatBar) => {
            crate::floatbar::toggle(app);
            rebuild_tray_menu(app);
        }
        Some(MenuAction::Quit) => {
            app.exit(0);
        }
        None => {}
    }
}

/// Rebuild the native tray menu from current provider + settings state.
pub(crate) fn rebuild_tray_menu(app: &AppHandle) {
    let catalog = crate::commands::get_provider_catalog();
    let settings = Settings::load();
    let status_labels = if let Some(st) = app.try_state::<Mutex<AppState>>() {
        let guard = st.lock().unwrap();
        let snapshots =
            presentation_snapshots(&guard.provider_cache, settings.codex_spark_usage_visible());
        status_labels_for_settings(&settings, &snapshots, settings.ui_language)
    } else {
        vec![]
    };
    if let Ok(menu) = build_native_tray_menu(app, &catalog, &status_labels)
        && let Some(tray) = app.tray_by_id("codexbar-main")
    {
        let _ = tray.set_menu(Some(menu));
    }
}

/// Rebuild the tray menu with current provider status labels after a refresh cycle.
pub fn update_tray_status_items(
    app: &AppHandle,
    snapshots: &[crate::commands::ProviderUsageSnapshot],
) {
    let catalog = crate::commands::get_provider_catalog();
    let settings = Settings::load();
    let snapshots = presentation_snapshots(snapshots, settings.codex_spark_usage_visible());
    let status_labels = status_labels_for_settings(&settings, &snapshots, settings.ui_language);

    if let Ok(menu) = build_native_tray_menu(app, &catalog, &status_labels)
        && let Some(tray) = app.tray_by_id("codexbar-main")
    {
        let _ = tray.set_menu(Some(menu));
    }
}

/// Refresh every native tray surface that depends on settings and cached provider data.
pub(crate) fn refresh_tray_presentation(app: &AppHandle) {
    let snapshots = app
        .try_state::<Mutex<AppState>>()
        .map(|st| st.lock().unwrap().provider_cache.clone())
        .unwrap_or_default();
    update_tray_status_items(app, &snapshots);
    update_tray_icon_and_tooltip(app, &snapshots);
}

fn presentation_snapshots(
    snapshots: &[crate::commands::ProviderUsageSnapshot],
    spark_usage_visible: bool,
) -> Vec<crate::commands::ProviderUsageSnapshot> {
    let mut snapshots = snapshots.to_vec();
    for snapshot in &mut snapshots {
        crate::commands::filter_hidden_codex_spark_rows(snapshot, spark_usage_visible);
    }
    snapshots
}

/// Update the tray icon pixels and tooltip text to reflect current provider usage.
///
/// One branded tray icon represents Ceiling as a whole. Its severity color
/// follows the most constrained healthy provider. Legacy icon-selection
/// settings (`menu_bar_shows_highest_usage`, `menu_bar_shows_percent`,
/// `menu_bar_display_mode`) are intentionally ignored.
///
/// With zero healthy providers but at least one error, fall back to an
/// error-styled icon so the tray still communicates "something is wrong".
pub fn update_tray_icon_and_tooltip(
    app: &AppHandle,
    snapshots: &[crate::commands::ProviderUsageSnapshot],
) {
    let Some(tray) = app.tray_by_id("codexbar-main") else {
        return;
    };

    // ── Icon ─────────────────────────────────────────────────────────────
    let settings = Settings::load();
    let snapshots = presentation_snapshots(snapshots, settings.codex_spark_usage_visible());
    let ordered_snapshots = ordered_snapshot_refs(&settings, &snapshots);
    let ok_snapshots: Vec<_> = ordered_snapshots
        .iter()
        .copied()
        .filter(|s| s.error.is_none())
        .collect();
    let all_error = ok_snapshots.is_empty() && !ordered_snapshots.is_empty();

    // One branded tray icon represents Ceiling as a whole. Its severity color
    // follows the most constrained healthy provider; legacy icon-selection
    // settings are intentionally ignored.
    let prefer_highest = true;

    let picked = pick_tray_provider(&ok_snapshots, &settings, prefer_highest);

    let (session_pct, weekly_pct) = match picked {
        Some(s) => selected_tray_used_percents(s, &settings),
        None => (
            ok_snapshots
                .iter()
                .map(|s| selected_tray_used_percents(s, &settings).0)
                .fold(0.0_f64, f64::max),
            None,
        ),
    };

    let (rgba, w, h) = render_tray_icon_for_settings(&settings, session_pct, weekly_pct, all_error);
    let icon = Image::new_owned(rgba, w, h);
    let _ = tray.set_icon(Some(icon));

    // ── Tooltip ───────────────────────────────────────────────────────────
    // Use the same dashboard-ordered set as the icon so the tooltip lists
    // providers in the window's order and never omits one to a tail clip.
    let tooltip = build_tooltip(
        &ordered_snapshots,
        settings.ui_language,
        settings.show_as_used,
    );
    let _ = tray.set_tooltip(Some(tooltip));
}

fn status_labels_for_settings(
    settings: &Settings,
    snapshots: &[crate::commands::ProviderUsageSnapshot],
    lang: codexbar::settings::Language,
) -> Vec<(String, String)> {
    let ordered_snapshots = ordered_snapshot_refs(settings, snapshots);
    let healthy: Vec<_> = ordered_snapshots
        .into_iter()
        .filter(|s| s.error.is_none())
        .collect();
    let Some(selected) = pick_tray_provider(&healthy, settings, true) else {
        return vec![];
    };

    let (_, label) = provider_status_label_for_settings(selected, settings, lang);
    vec![("status_summary".to_string(), label)]
}

fn ordered_snapshot_refs<'a>(
    settings: &Settings,
    snapshots: &'a [crate::commands::ProviderUsageSnapshot],
) -> Vec<&'a crate::commands::ProviderUsageSnapshot> {
    let order = settings
        .provider_display_order_names()
        .into_iter()
        .enumerate()
        .map(|(index, provider_id)| (provider_id, index))
        .collect::<std::collections::HashMap<_, _>>();
    // Refreshes upsert provider snapshots, so disabling a provider does not
    // remove its cached entry. Never let that stale cache continue driving the
    // native tray icon, tooltip, or status menu after the user unchecks it.
    let mut ordered = snapshots
        .iter()
        .filter(|snapshot| settings.enabled_providers.contains(&snapshot.provider_id))
        .collect::<Vec<_>>();
    ordered.sort_by(|a, b| {
        let a_order = order.get(&a.provider_id);
        let b_order = order.get(&b.provider_id);
        match (a_order, b_order) {
            (Some(a_order), Some(b_order)) if a_order != b_order => a_order.cmp(b_order),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => a.display_name.cmp(&b.display_name),
        }
    });
    ordered
}

#[cfg(test)]
fn provider_status_label(
    snapshot: &crate::commands::ProviderUsageSnapshot,
    lang: codexbar::settings::Language,
) -> (String, String) {
    let label = crate::commands::compact_tray_status_label(&snapshot.primary, lang);
    (
        snapshot.provider_id.clone(),
        format!("{} {}", snapshot.display_name, label),
    )
}

fn provider_status_label_for_settings(
    snapshot: &crate::commands::ProviderUsageSnapshot,
    settings: &Settings,
    lang: codexbar::settings::Language,
) -> (String, String) {
    if let Some(headline) = recorded_totals_headline(snapshot) {
        return (
            snapshot.provider_id.clone(),
            format!("{} {headline}", snapshot.display_name),
        );
    }
    let provider = ProviderId::from_cli_name(snapshot.provider_id.as_str());
    let preference = provider
        .map(|id| settings.get_provider_metric(id))
        .unwrap_or(MetricPreference::Automatic);
    let percent = selected_metric_percent(snapshot, provider, preference)
        .or_else(|| selected_metric_percent(snapshot, provider, MetricPreference::Automatic))
        .unwrap_or(snapshot.primary.used_percent);
    let status = selected_metric_window(snapshot, provider, preference)
        .map(|window| crate::commands::compact_tray_status_label(window, lang))
        .unwrap_or_else(|| format!("{}%", percent.clamp(0.0, 100.0).round() as u8));
    (
        snapshot.provider_id.clone(),
        format!("{} {status}", snapshot.display_name),
    )
}

/// DeepSeek Harness reports wallet balances and recorded token counts, not a
/// quota, so the tray shows its headline value instead of a percentage.
fn recorded_totals_headline(snapshot: &crate::commands::ProviderUsageSnapshot) -> Option<&str> {
    if snapshot.provider_id != "deepseekharness" {
        return None;
    }
    snapshot
        .primary
        .reset_description
        .as_deref()
        .map(str::trim)
        .filter(|headline| !headline.is_empty())
}

fn render_tray_icon_for_settings(
    settings: &Settings,
    session_pct: f64,
    weekly_pct: Option<f64>,
    all_error: bool,
) -> (Vec<u8>, u32, u32) {
    let _ = (settings, weekly_pct);
    render_ceiling_tray_icon_rgba(session_pct, all_error)
}

/// Pick the provider whose usage the tray icon should render.
///
/// Exposed so that the unit tests can exercise both `highest` and `first`
/// paths without needing a live Tauri app handle.
fn pick_tray_provider<'a>(
    ok_snapshots: &'a [&'a crate::commands::ProviderUsageSnapshot],
    settings: &Settings,
    prefer_highest: bool,
) -> Option<&'a crate::commands::ProviderUsageSnapshot> {
    if ok_snapshots.is_empty() {
        return None;
    }
    if prefer_highest {
        ok_snapshots.iter().copied().max_by(|a, b| {
            selected_tray_used_percents(a, settings)
                .0
                .partial_cmp(&selected_tray_used_percents(b, settings).0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    } else {
        Some(ok_snapshots[0])
    }
}

fn selected_tray_used_percents(
    snapshot: &crate::commands::ProviderUsageSnapshot,
    settings: &Settings,
) -> (f64, Option<f64>) {
    let provider = ProviderId::from_cli_name(snapshot.provider_id.as_str());
    let preference = provider
        .map(|id| settings.get_provider_metric(id))
        .unwrap_or(MetricPreference::Automatic);
    let primary = selected_metric_percent(snapshot, provider, preference)
        .or_else(|| selected_metric_percent(snapshot, provider, MetricPreference::Automatic))
        .unwrap_or(snapshot.primary.used_percent);

    let secondary = snapshot.secondary.as_ref().map(|w| w.used_percent);

    (primary.clamp(0.0, 100.0), secondary)
}

fn display_metric_percent(used_percent: f64, show_as_used: bool) -> f64 {
    let used = used_percent.clamp(0.0, 100.0);
    if show_as_used { used } else { 100.0 - used }
}

fn selected_metric_percent(
    snapshot: &crate::commands::ProviderUsageSnapshot,
    provider: Option<ProviderId>,
    preference: MetricPreference,
) -> Option<f64> {
    match preference {
        MetricPreference::Automatic => automatic_metric_percent(snapshot, provider),
        MetricPreference::Session => Some(snapshot.primary.used_percent),
        MetricPreference::Weekly => snapshot
            .secondary
            .as_ref()
            .map(|w| w.used_percent)
            .or(Some(snapshot.primary.used_percent)),
        MetricPreference::Model => snapshot
            .model_specific
            .as_ref()
            .map(|w| w.used_percent)
            .or(Some(snapshot.primary.used_percent)),
        MetricPreference::Tertiary => snapshot
            .tertiary
            .as_ref()
            .map(|w| w.used_percent)
            .or_else(|| snapshot.secondary.as_ref().map(|w| w.used_percent))
            .or(Some(snapshot.primary.used_percent)),
        MetricPreference::Credits => cost_metric_percent(snapshot),
        MetricPreference::ExtraUsage => {
            extra_rate_window_percent(snapshot).or_else(|| cost_metric_percent(snapshot))
        }
        MetricPreference::Average => average_metric_percent(snapshot),
    }
}

fn selected_metric_window(
    snapshot: &crate::commands::ProviderUsageSnapshot,
    provider: Option<ProviderId>,
    preference: MetricPreference,
) -> Option<&crate::commands::RateWindowSnapshot> {
    match preference {
        MetricPreference::Automatic => automatic_metric_window(snapshot, provider),
        MetricPreference::Session => Some(&snapshot.primary),
        MetricPreference::Weekly => snapshot.secondary.as_ref().or(Some(&snapshot.primary)),
        MetricPreference::Model => snapshot.model_specific.as_ref().or(Some(&snapshot.primary)),
        MetricPreference::Tertiary => snapshot
            .tertiary
            .as_ref()
            .or(snapshot.secondary.as_ref())
            .or(Some(&snapshot.primary)),
        MetricPreference::ExtraUsage => snapshot
            .extra_rate_windows
            .iter()
            .max_by(|a, b| {
                a.window
                    .used_percent
                    .partial_cmp(&b.window.used_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|extra| &extra.window),
        MetricPreference::Credits | MetricPreference::Average => None,
    }
}

fn automatic_metric_window<'a>(
    snapshot: &'a crate::commands::ProviderUsageSnapshot,
    provider: Option<ProviderId>,
) -> Option<&'a crate::commands::RateWindowSnapshot> {
    let highest = |windows: Vec<&'a crate::commands::RateWindowSnapshot>| {
        windows.into_iter().max_by(|a, b| {
            a.used_percent
                .partial_cmp(&b.used_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    };
    match provider {
        Some(ProviderId::Cursor) => {
            let mut windows = vec![&snapshot.primary];
            windows.extend(snapshot.secondary.iter());
            windows.extend(snapshot.tertiary.iter());
            highest(windows)
        }
        Some(ProviderId::Zai) => {
            let mut windows = vec![&snapshot.primary];
            windows.extend(snapshot.tertiary.iter());
            highest(windows)
        }
        Some(ProviderId::Factory) | Some(ProviderId::Kimi) => {
            snapshot.secondary.as_ref().or(Some(&snapshot.primary))
        }
        Some(ProviderId::Copilot) => {
            let mut windows = vec![&snapshot.primary];
            windows.extend(snapshot.secondary.iter());
            windows.extend(
                snapshot
                    .extra_rate_windows
                    .iter()
                    .map(|extra| &extra.window),
            );
            highest(windows)
        }
        // Claude/Codex both expose a short session and a weekly pool. Automatic
        // mode must surface the hotter of the two (SOU-288) so a maxed weekly
        // is not hidden behind a fresh 5-hour session.
        Some(ProviderId::Claude) | Some(ProviderId::Codex) => {
            let mut windows = vec![&snapshot.primary];
            windows.extend(snapshot.secondary.iter());
            highest(windows)
        }
        _ => Some(&snapshot.primary),
    }
}

fn automatic_metric_percent(
    snapshot: &crate::commands::ProviderUsageSnapshot,
    provider: Option<ProviderId>,
) -> Option<f64> {
    match provider {
        Some(ProviderId::Cursor) => max_metric_percent([
            Some(snapshot.primary.used_percent),
            snapshot.secondary.as_ref().map(|w| w.used_percent),
            snapshot.tertiary.as_ref().map(|w| w.used_percent),
        ]),
        Some(ProviderId::Zai) => max_metric_percent([
            Some(snapshot.primary.used_percent),
            snapshot.tertiary.as_ref().map(|w| w.used_percent),
            None,
        ])
        .or_else(|| snapshot.secondary.as_ref().map(|w| w.used_percent)),
        Some(ProviderId::Factory) | Some(ProviderId::Kimi) => snapshot
            .secondary
            .as_ref()
            .map(|w| w.used_percent)
            .or(Some(snapshot.primary.used_percent)),
        Some(ProviderId::Copilot) => max_metric_percent([
            Some(snapshot.primary.used_percent),
            snapshot.secondary.as_ref().map(|w| w.used_percent),
            extra_rate_window_percent(snapshot),
        ]),
        Some(ProviderId::Claude) | Some(ProviderId::Codex) => max_metric_percent([
            Some(snapshot.primary.used_percent),
            snapshot.secondary.as_ref().map(|w| w.used_percent),
            None,
        ]),
        _ => Some(snapshot.primary.used_percent),
    }
}

fn average_metric_percent(snapshot: &crate::commands::ProviderUsageSnapshot) -> Option<f64> {
    let secondary = snapshot.secondary.as_ref()?;
    Some((snapshot.primary.used_percent + secondary.used_percent) / 2.0)
}

fn cost_metric_percent(snapshot: &crate::commands::ProviderUsageSnapshot) -> Option<f64> {
    let cost = snapshot.cost.as_ref()?;
    let limit = cost.limit?;
    if limit <= 0.0 {
        return None;
    }
    Some(((cost.used / limit) * 100.0).clamp(0.0, 100.0))
}

fn extra_rate_window_percent(snapshot: &crate::commands::ProviderUsageSnapshot) -> Option<f64> {
    snapshot
        .extra_rate_windows
        .iter()
        .map(|extra| extra.window.used_percent)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

fn max_metric_percent<const N: usize>(values: [Option<f64>; N]) -> Option<f64> {
    values
        .into_iter()
        .flatten()
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

/// Build a compact multi-line tooltip string from provider snapshots.
///
/// The hard constraint: Windows Shell only reliably *displays* 63 characters
/// of a tray icon's tooltip on the taskbar surface we use (the backing struct
/// is larger, but Explorer clips the visible text). At that budget, "Name P% Label · reset" for
/// three-plus providers overflows and the tail providers silently disappear —
/// which is exactly the "not showing all subscriptions" bug.
///
/// So the rule here is completeness first: EVERY configured provider gets a
/// line, and we spend the remaining budget on the richest per-line detail that
/// still fits — degrading label+reset → reset → percent uniformly rather than
/// dropping anyone. Callers pass snapshots already in dashboard order so the
/// tooltip reads the same top-to-bottom as the window. The configured
/// used/remaining mode is included explicitly so the percentage is never
/// ambiguous on hover.
fn build_tooltip(
    snapshots: &[&crate::commands::ProviderUsageSnapshot],
    lang: codexbar::settings::Language,
    show_as_used: bool,
) -> String {
    use codexbar::locale::{LocaleKey, get_text};

    if snapshots.is_empty() {
        return "Ceiling".to_string();
    }

    let error_label = get_text(lang, LocaleKey::TrayStatusRowError);
    let percent_suffix = get_text(
        lang,
        if show_as_used {
            LocaleKey::PanelUsedSuffix
        } else {
            LocaleKey::PanelLeftSuffix
        },
    );

    // The window label is mandatory: an unlabeled percentage is not useful
    // across providers with radically different reset windows. Reset timing is
    // the only optional detail when Windows' tooltip budget is tight.
    //   0: "Name · Weekly · 85% used · 24d 11h"
    //   1: "Name · Weekly · 85% used"
    //   2: "Name Weekly 85% used"
    //   3: "Name Weekly 85%"
    let render_line = |s: &crate::commands::ProviderUsageSnapshot, detail: u8| -> String {
        if s.error.is_some() {
            return format!("{}  {}", s.display_name, error_label);
        }
        if let Some(headline) = recorded_totals_headline(s) {
            let name = truncate_tooltip_text(&s.display_name, 12);
            return match detail {
                0 | 1 => format!("{name} · {headline}"),
                _ => format!("{name} {headline}"),
            };
        }
        let percent = display_metric_percent(s.primary.used_percent, show_as_used);
        let label = s
            .primary_label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(|label| truncate_tooltip_text(label, 12))
            .or_else(|| {
                s.primary.window_minutes.map(|minutes| {
                    if minutes >= 60 && minutes % 60 == 0 {
                        format!("{}h", minutes / 60)
                    } else {
                        format!("{minutes}m")
                    }
                })
            })
            .unwrap_or_else(|| match s.provider_id.as_str() {
                "claude" => "Session (5h)".to_string(),
                "codex" => "Weekly".to_string(),
                "cursor" => "Plan".to_string(),
                _ => "Limit".to_string(),
            });
        let name = truncate_tooltip_text(&s.display_name, 12);
        let compact_label = compact_tooltip_window_label(
            s.provider_id.as_str(),
            Some(label.as_str()),
            s.primary.window_minutes,
        );
        let mut line = match detail {
            0 | 1 => format!("{name} · {label} · {percent:.0}% {percent_suffix}"),
            2 => format!("{name} {compact_label} {percent:.0}% {percent_suffix}"),
            _ => format!("{name} {compact_label} {percent:.0}%"),
        };
        if detail == 0
            && let Some(reset) = tooltip_short_reset(
                s.primary.resets_at.as_deref(),
                s.primary.reset_description.as_deref(),
            )
        {
            line.push_str(" · ");
            line.push_str(&reset);
        }
        line
    };

    // Windows' visible tooltip budget. Kept conservative so every provider
    // survives the shell's clip even on the strictest builds.
    const BUDGET: usize = 63;
    for detail in 0u8..=3 {
        let body = snapshots
            .iter()
            .map(|s| render_line(s, detail))
            .collect::<Vec<_>>()
            .join("\n");
        if body.chars().count() <= BUDGET {
            return body;
        }
    }
    // More providers can exceed the Windows limit even in compact form. Keep
    // complete lines only and end with an explicit overflow row; never hand
    // Explorer a string that it will cut halfway through a word or provider.
    let compact = snapshots
        .iter()
        .map(|s| render_line(s, 3))
        .collect::<Vec<_>>();
    let mut visible = Vec::new();
    for (index, line) in compact.iter().enumerate() {
        let remaining = compact.len() - index - 1;
        let overflow = (remaining > 0).then(|| format!("+{remaining} more in Ceiling"));
        let mut candidate = visible.clone();
        candidate.push(line.clone());
        if let Some(overflow) = overflow {
            candidate.push(overflow);
        }
        if candidate.join("\n").chars().count() > BUDGET {
            break;
        }
        visible.push(line.clone());
    }
    let hidden = compact.len().saturating_sub(visible.len());
    if hidden > 0 {
        visible.push(format!("+{hidden} more in Ceiling"));
    }
    visible.join("\n")
}

fn compact_tooltip_window_label(
    provider_id: &str,
    label: Option<&str>,
    window_minutes: Option<u32>,
) -> String {
    let label = label.map(str::trim).filter(|label| !label.is_empty());
    let normalized = label.map(str::to_ascii_lowercase);
    if provider_id == "claude"
        && (normalized
            .as_deref()
            .is_some_and(|label| label.contains("session"))
            || window_minutes == Some(300))
    {
        return "5h".to_string();
    }
    label
        .map(|label| truncate_tooltip_text(label, 8))
        .unwrap_or_else(|| match provider_id {
            "codex" => "Weekly".to_string(),
            "cursor" => "Plan".to_string(),
            _ => "Limit".to_string(),
        })
}

/// Compact reset duration for the tooltip ("24d 11h" / "2h 21m" / "12m"), or
/// `None` when there is no known future reset. Deliberately unit-terse and
/// language-neutral to keep the native Windows tooltip within its length cap.
pub(crate) fn tooltip_short_reset(
    resets_at: Option<&str>,
    reset_desc: Option<&str>,
) -> Option<String> {
    tooltip_short_reset_at(resets_at, reset_desc, chrono::Utc::now())
}

/// `tooltip_short_reset` against an explicit clock so the 24h cut and the
/// last-minute clamp can be asserted without racing `Utc::now()`.
pub(crate) fn tooltip_short_reset_at(
    resets_at: Option<&str>,
    reset_desc: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    if let Some(ra) = resets_at
        && let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ra)
    {
        let dt = parsed.with_timezone(&chrono::Utc);
        if dt > now {
            // Same floor-and-clamp / 1440-minute day cut as CLI and the TS
            // hooks. Without the clamp, 1-59s remaining rendered "0m"
            // (SBS-927 / leftover of SBS-621).
            return Some(codexbar::core::format_remaining_countdown(dt - now));
        }
    }
    // Fall back to the provider's own reset description, minus a leading
    // "resets in " and trimmed to keep the native tooltip compact.
    let desc = reset_desc.map(str::trim).filter(|d| !d.is_empty())?;
    let lower = desc.to_ascii_lowercase();
    let body = ["resets in ", "reset in ", "in "]
        .iter()
        .find(|p| lower.starts_with(**p))
        .map(|p| desc[p.len()..].trim_start())
        .unwrap_or(desc);
    Some(truncate_tooltip_text(body, 14))
}

fn truncate_tooltip_text(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

#[allow(dead_code)]
fn menu_contains(menu: &[TrayMenuEntry], id: &str) -> bool {
    menu.iter().any(|entry| {
        entry.id.as_deref() == Some(id)
            || (!entry.children.is_empty() && menu_contains(&entry.children, id))
    })
}

enum NativeMenuEntry {
    Item(MenuItem<tauri::Wry>),
    CheckItem(tauri::menu::CheckMenuItem<tauri::Wry>),
    Submenu(Submenu<tauri::Wry>),
    Separator(PredefinedMenuItem<tauri::Wry>),
}

impl NativeMenuEntry {
    fn as_item(&self) -> &dyn IsMenuItem<tauri::Wry> {
        match self {
            Self::Item(item) => item,
            Self::CheckItem(item) => item,
            Self::Submenu(item) => item,
            Self::Separator(item) => item,
        }
    }
}

fn build_native_menu_entry(
    app: &AppHandle,
    entry: &TrayMenuEntry,
) -> tauri::Result<NativeMenuEntry> {
    if entry.is_separator {
        return Ok(NativeMenuEntry::Separator(PredefinedMenuItem::separator(
            app,
        )?));
    }

    if !entry.children.is_empty() {
        let children = entry
            .children
            .iter()
            .map(|child| build_native_menu_entry(app, child))
            .collect::<tauri::Result<Vec<_>>>()?;
        let child_refs = children
            .iter()
            .map(NativeMenuEntry::as_item)
            .collect::<Vec<_>>();

        return Ok(NativeMenuEntry::Submenu(Submenu::with_items(
            app,
            &entry.label,
            true,
            &child_refs,
        )?));
    }

    // Render as a checkbox item when `checked` is set.
    if let Some(checked) = entry.checked {
        return Ok(NativeMenuEntry::CheckItem(
            CheckMenuItemBuilder::with_id(entry.id.clone().unwrap_or_default(), &entry.label)
                .enabled(!entry.disabled)
                .checked(checked)
                .build(app)?,
        ));
    }

    Ok(NativeMenuEntry::Item(MenuItem::with_id(
        app,
        entry.id.clone().unwrap_or_default(),
        &entry.label,
        !entry.disabled,
        None::<&str>,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_provider_catalog() -> Vec<ProviderCatalogEntry> {
        vec![
            ProviderCatalogEntry {
                id: "codex".into(),
                display_name: "Codex".into(),
                cookie_domain: None,
            },
            ProviderCatalogEntry {
                id: "claude".into(),
                display_name: "Claude".into(),
                cookie_domain: None,
            },
        ]
    }

    #[test]
    fn tray_menu_includes_about_and_provider_entries() {
        let menu = build_tray_menu(
            &sample_provider_catalog(),
            &[],
            &["codex".to_string(), "claude".to_string()]
                .into_iter()
                .collect(),
        );
        assert!(menu_contains(&menu, "about"));
        assert!(menu_contains(&menu, "toggle_provider:codex"));
        assert!(menu_contains(&menu, "quit"));
    }

    #[test]
    fn toggle_float_bar_routes_to_toggle_action() {
        let action = resolve_menu_action("toggle_float_bar").expect("float bar action");
        assert!(matches!(action, MenuAction::ToggleFloatBar));
    }

    #[test]
    fn settings_menu_routes_to_open_settings_action() {
        let action = resolve_menu_action("about").expect("about action");
        match action {
            MenuAction::OpenSettings(tab) => assert_eq!(tab, "about"),
            _ => panic!("expected OpenSettings for 'about'"),
        }

        let action = resolve_menu_action("settings").expect("settings action");
        match action {
            MenuAction::OpenSettings(tab) => assert_eq!(tab, "general"),
            _ => panic!("expected OpenSettings for 'settings'"),
        }

        let action = resolve_menu_action("more_providers").expect("providers action");
        match action {
            MenuAction::OpenSettings(tab) => assert_eq!(tab, "providers"),
            _ => panic!("expected OpenSettings for 'more_providers'"),
        }
    }

    #[test]
    fn provider_menu_routes_to_provider_popout_target() {
        let action = resolve_menu_target("provider:codex").expect("provider target");
        assert_eq!(action.mode, SurfaceMode::PopOut);
        assert_eq!(
            action.target,
            SurfaceTarget::Provider {
                provider_id: "codex".into()
            }
        );
    }

    #[test]
    fn pop_out_menu_routes_to_dashboard_transition() {
        // "Pop Out Dashboard" now opens the full dashboard (SurfaceMode::PopOut
        // on `main`) — the same surface as the legacy "Show Window" — rather
        // than the retired compact flyout window.
        let request = resolve_menu_target("pop_out").expect("pop_out target");
        assert_eq!(request.mode, SurfaceMode::PopOut);
        assert_eq!(request.target, SurfaceTarget::Dashboard);

        let action = resolve_menu_action("pop_out").expect("pop_out action");
        assert!(matches!(action, MenuAction::Transition(_)));
    }

    #[test]
    fn show_panel_menu_reopens_popout_dashboard_with_default_position_chain() {
        let request = resolve_menu_target("show_panel").expect("show_panel target");
        assert_eq!(request.mode, SurfaceMode::PopOut);
        assert_eq!(request.target, SurfaceTarget::Dashboard);

        let dispatch = resolve_menu_transition_dispatch(
            "show_panel",
            shell::ShellTransitionRequest {
                mode: SurfaceMode::PopOut,
                target: SurfaceTarget::Dashboard,
                position: Some((320, 240)),
            },
        );

        match dispatch {
            MenuTransitionDispatch::Reopen(request) => {
                assert_eq!(request.mode, SurfaceMode::PopOut);
                assert_eq!(request.target, SurfaceTarget::Dashboard);
                assert_eq!(request.position, None);
            }
            MenuTransitionDispatch::Transition(_) => {
                panic!("show_panel should reopen via default PopOut positioning")
            }
        }
    }

    #[test]
    fn non_show_panel_menu_keeps_explicit_position() {
        // A provider deep link is a realistic non-reopen caller of this
        // dispatch — unlike "show_panel"/"pop_out", which reopen with the
        // default position chain.
        let dispatch = resolve_menu_transition_dispatch(
            "provider:codex",
            shell::ShellTransitionRequest {
                mode: SurfaceMode::PopOut,
                target: SurfaceTarget::Provider {
                    provider_id: "codex".into(),
                },
                position: Some((320, 240)),
            },
        );

        match dispatch {
            MenuTransitionDispatch::Transition(request) => {
                assert_eq!(request.mode, SurfaceMode::PopOut);
                assert_eq!(
                    request.target,
                    SurfaceTarget::Provider {
                        provider_id: "codex".into()
                    }
                );
                assert_eq!(request.position, Some((320, 240)));
            }
            MenuTransitionDispatch::Reopen(_) => {
                panic!("non-show-panel actions should use direct transitions")
            }
        }
    }

    #[test]
    fn logical_tray_anchor_uses_click_monitor_scale() {
        let monitors = vec![
            MonitorScaleInfo {
                physical_x: 0,
                physical_y: 0,
                physical_width: 1920,
                physical_height: 1080,
                scale_factor: 1.0,
            },
            MonitorScaleInfo {
                physical_x: 1920,
                physical_y: 0,
                physical_width: 2560,
                physical_height: 1440,
                scale_factor: 2.0,
            },
        ];

        let rect = tauri::Rect {
            position: tauri::Position::Logical(tauri::LogicalPosition::new(1500.0, 500.0)),
            size: tauri::Size::Logical(tauri::LogicalSize::new(12.0, 12.0)),
        };
        let anchor = resolve_tray_anchor(
            &rect,
            tauri::PhysicalPosition::new(1510.0, 500.0),
            &monitors,
        )
        .expect("matching click monitor scale");

        assert_eq!(anchor.x, 1500);
        assert_eq!(anchor.y, 500);
        assert_eq!(anchor.width, 12);
        assert_eq!(anchor.height, 12);
    }

    #[test]
    fn logical_tray_anchor_skips_conversion_without_click_monitor() {
        let monitors = vec![MonitorScaleInfo {
            physical_x: 0,
            physical_y: 0,
            physical_width: 1920,
            physical_height: 1080,
            scale_factor: 1.0,
        }];
        let rect = tauri::Rect {
            position: tauri::Position::Logical(tauri::LogicalPosition::new(1500.0, 500.0)),
            size: tauri::Size::Logical(tauri::LogicalSize::new(12.0, 12.0)),
        };

        let anchor = resolve_tray_anchor(
            &rect,
            tauri::PhysicalPosition::new(2500.0, 500.0),
            &monitors,
        );

        assert!(anchor.is_none());
    }

    fn fake_snapshot_with(
        id: &str,
        display: &str,
        used_percent: f64,
        secondary_percent: Option<f64>,
        tertiary_percent: Option<f64>,
        cost: Option<(f64, f64)>,
    ) -> crate::commands::ProviderUsageSnapshot {
        crate::commands::ProviderUsageSnapshot {
            provider_id: id.into(),
            display_name: display.into(),
            primary: crate::commands::RateWindowSnapshot {
                used_percent,
                remaining_percent: 100.0 - used_percent,
                window_minutes: None,
                resets_at: None,
                reset_description: None,
                is_exhausted: false,
                reserve_percent: None,
                reserve_description: None,
                reserve_will_last_to_reset: false,
                reserve_eta_seconds: None,
            },
            primary_label: None,
            secondary: secondary_percent.map(|pct| crate::commands::RateWindowSnapshot {
                used_percent: pct,
                remaining_percent: 100.0 - pct,
                window_minutes: None,
                resets_at: None,
                reset_description: None,
                is_exhausted: false,
                reserve_percent: None,
                reserve_description: None,
                reserve_will_last_to_reset: false,
                reserve_eta_seconds: None,
            }),
            secondary_label: None,
            model_specific: None,
            tertiary: tertiary_percent.map(|pct| crate::commands::RateWindowSnapshot {
                used_percent: pct,
                remaining_percent: 100.0 - pct,
                window_minutes: None,
                resets_at: None,
                reset_description: None,
                is_exhausted: false,
                reserve_percent: None,
                reserve_description: None,
                reserve_will_last_to_reset: false,
                reserve_eta_seconds: None,
            }),
            tertiary_label: None,
            inactive_rate_windows: Vec::new(),
            extra_rate_windows: Vec::new(),
            promo_signals: Vec::new(),
            reset_credits_available: None,
            cost: cost.map(|(used, limit)| crate::commands::CostSnapshotBridge {
                used,
                limit: Some(limit),
                remaining: Some((limit - used).max(0.0)),
                currency_code: "USD".to_string(),
                period: "monthly".to_string(),
                resets_at: None,
                formatted_used: format!("${used:.2}"),
                formatted_limit: Some(format!("${limit:.2}")),
            }),
            plan_name: None,
            account_email: None,
            source_label: String::new(),
            updated_at: "2025-01-01T00:00:00Z".into(),
            error: None,
            pace: None,
            account_organization: None,
            tray_status_label: None,
            account_id: None,
            account_label: None,
            account_tint: None,
            fetch_duration_ms: None,
            wayfinder_usage: None,
        }
    }

    fn fake_snapshot(
        id: &str,
        display: &str,
        used_percent: f64,
    ) -> crate::commands::ProviderUsageSnapshot {
        fake_snapshot_with(id, display, used_percent, None, None, None)
    }

    fn fake_extra_window(percent: f64) -> crate::commands::NamedRateWindowSnapshot {
        crate::commands::NamedRateWindowSnapshot {
            id: "additional_budget".to_string(),
            title: "Additional Budget".to_string(),
            window: crate::commands::RateWindowSnapshot {
                used_percent: percent,
                remaining_percent: 100.0 - percent,
                window_minutes: None,
                resets_at: None,
                reset_description: None,
                is_exhausted: false,
                reserve_percent: None,
                reserve_description: None,
                reserve_will_last_to_reset: false,
                reserve_eta_seconds: None,
            },
            amount: None,
        }
    }

    #[test]
    fn pick_tray_provider_highest_picks_max_primary() {
        let settings = Settings::default();
        let a = fake_snapshot("codex", "Codex", 30.0);
        let b = fake_snapshot("claude", "Claude", 72.5);
        let c = fake_snapshot("gemini", "Gemini", 50.0);
        let refs: Vec<&crate::commands::ProviderUsageSnapshot> = vec![&a, &b, &c];

        let picked = pick_tray_provider(&refs, &settings, /* prefer_highest = */ true)
            .expect("highest mode should pick a provider");
        assert_eq!(picked.provider_id, "claude");
    }

    #[test]
    fn pick_tray_provider_first_preserves_catalog_order() {
        let settings = Settings::default();
        let a = fake_snapshot("codex", "Codex", 30.0);
        let b = fake_snapshot("claude", "Claude", 72.5);
        let refs: Vec<&crate::commands::ProviderUsageSnapshot> = vec![&a, &b];

        let picked = pick_tray_provider(&refs, &settings, /* prefer_highest = */ false)
            .expect("non-highest mode should still pick the first entry");
        assert_eq!(picked.provider_id, "codex");
    }

    #[test]
    fn pick_tray_provider_ranks_the_configured_metric_in_used_units() {
        let mut settings = Settings::default();
        settings.set_provider_metric(ProviderId::Cursor, MetricPreference::ExtraUsage);
        let cursor = fake_snapshot_with("cursor", "Cursor", 95.0, None, None, Some((15.0, 100.0)));
        let claude = fake_snapshot("claude", "Claude", 50.0);
        let refs = vec![&cursor, &claude];

        let picked = pick_tray_provider(&refs, &settings, true).expect("selected provider");

        assert_eq!(picked.provider_id, "claude");
    }

    #[test]
    fn pick_tray_provider_none_when_empty() {
        let settings = Settings::default();
        let refs: Vec<&crate::commands::ProviderUsageSnapshot> = vec![];
        assert!(pick_tray_provider(&refs, &settings, true).is_none());
        assert!(pick_tray_provider(&refs, &settings, false).is_none());
    }

    #[test]
    fn several_providers_collapse_to_one_tray_summary() {
        let settings = Settings {
            provider_order: codexbar::settings::normalize_provider_order(&[
                "claude".to_string(),
                "codex".to_string(),
            ]),
            ..Settings::default()
        };
        let snapshots = vec![
            fake_snapshot("codex", "Codex", 30.0),
            fake_snapshot("claude", "Claude", 72.0),
        ];

        let labels = status_labels_for_settings(
            &settings,
            &snapshots,
            codexbar::settings::Language::English,
        );

        assert_eq!(
            labels,
            vec![("status_summary".to_string(), "Claude 72%".to_string())]
        );
    }

    #[test]
    fn status_labels_single_mode_collapses_to_selected_provider() {
        let settings = Settings {
            menu_bar_shows_highest_usage: true,
            ..Settings::default()
        };
        let snapshots = vec![
            fake_snapshot("codex", "Codex", 30.0),
            fake_snapshot("claude", "Claude", 72.0),
        ];

        let labels = status_labels_for_settings(
            &settings,
            &snapshots,
            codexbar::settings::Language::English,
        );

        assert_eq!(
            labels,
            vec![("status_summary".to_string(), "Claude 72%".to_string())]
        );
    }

    #[test]
    fn tray_presentation_excludes_disabled_cached_providers() {
        let settings = Settings {
            enabled_providers: ["codex".to_string()].into_iter().collect(),
            ..Settings::default()
        };
        let snapshots = vec![
            fake_snapshot("codex", "Codex", 30.0),
            fake_snapshot("claude", "Claude", 92.0),
        ];

        let ordered = ordered_snapshot_refs(&settings, &snapshots);
        assert_eq!(ordered.len(), 1);
        assert_eq!(ordered[0].provider_id, "codex");
        assert_eq!(
            status_labels_for_settings(
                &settings,
                &snapshots,
                codexbar::settings::Language::English,
            ),
            vec![("status_summary".to_string(), "Codex 30%".to_string())]
        );
    }

    #[test]
    fn legacy_percent_mode_does_not_change_the_branded_tray_icon() {
        let bar_settings = Settings {
            menu_bar_shows_percent: false,
            ..Settings::default()
        };
        let percent_settings = Settings {
            menu_bar_shows_percent: true,
            ..Settings::default()
        };

        let (bar, bar_w, bar_h) =
            render_tray_icon_for_settings(&bar_settings, 72.0, Some(40.0), false);
        let (percent, pct_w, pct_h) =
            render_tray_icon_for_settings(&percent_settings, 72.0, Some(40.0), false);

        assert_eq!((bar_w, bar_h), (pct_w, pct_h));
        assert_eq!(bar, percent);
    }

    #[test]
    fn tooltip_uses_compact_status_labels() {
        let mut claude = fake_snapshot("claude", "Claude", 13.0);
        claude.primary.reset_description = Some("2h 05m".to_string());
        let mut codex = fake_snapshot("codex", "Codex", 8.0);
        codex.primary.reset_description = Some("4h 10m".to_string());

        let tooltip = build_tooltip(
            &[&claude, &codex],
            codexbar::settings::Language::English,
            true,
        );

        // Every line names its rate window. Reset detail is dropped uniformly
        // when retaining it would cross Explorer's visible tooltip limit.
        assert_eq!(
            tooltip,
            "Claude · Session (5h) · 13% used\nCodex · Weekly · 8% used"
        );
    }

    #[test]
    fn deepseek_harness_tray_shows_its_headline_not_a_percentage() {
        let mut dsh = fake_snapshot("deepseekharness", "DeepSeek Harness", 0.0);
        dsh.primary.reset_description = Some("¥42.12".to_string());

        let tooltip = build_tooltip(&[&dsh], codexbar::settings::Language::English, false);
        let name = truncate_tooltip_text("DeepSeek Harness", 12);
        assert_eq!(tooltip, format!("{name} · ¥42.12"));
        assert!(!tooltip.contains('%'));

        let (_, label) = provider_status_label_for_settings(
            &dsh,
            &Settings::default(),
            codexbar::settings::Language::English,
        );
        assert_eq!(label, "DeepSeek Harness ¥42.12");
    }

    #[test]
    fn tooltip_keeps_every_provider_within_the_budget() {
        // Window identity is never sacrificed when reset detail gets tight.
        let mut codex = fake_snapshot("codex", "Codex", 80.0);
        codex.primary_label = Some("Weekly".to_string());
        codex.primary.reset_description = Some("6d 13h".to_string());
        let mut claude = fake_snapshot("claude", "Claude", 57.0);
        claude.primary_label = Some("Session (5h)".to_string());
        claude.primary.reset_description = Some("1h 27m".to_string());
        let mut cursor = fake_snapshot("cursor", "Cursor", 85.0);
        cursor.primary_label = Some("Plan".to_string());
        cursor.primary.reset_description = Some("24d 8h".to_string());

        let tooltip = build_tooltip(
            &[&codex, &claude, &cursor],
            codexbar::settings::Language::English,
            true,
        );

        assert!(tooltip.chars().count() <= 63, "over budget: {tooltip}");
        for name in ["Codex", "Claude", "Cursor"] {
            assert!(tooltip.contains(name), "missing {name}: {tooltip}");
        }
        // Each provider keeps its percent.
        for pct in ["80%", "57%", "85%"] {
            assert!(tooltip.contains(pct), "missing {pct}: {tooltip}");
        }
        for label in ["Weekly", "5h", "Plan"] {
            assert!(tooltip.contains(label), "missing {label}: {tooltip}");
        }
        assert_eq!(
            tooltip,
            "Codex Weekly 80% used\nClaude 5h 57% used\nCursor Plan 85% used"
        );
    }

    #[test]
    fn tooltip_truncates_long_provider_lines() {
        let mut claude = fake_snapshot("claude", "Claude", 13.0);
        claude.primary.reset_description =
            Some("resets in Jun 10 at 3:00PM with extra noisy suffix".to_string());

        let tooltip = build_tooltip(&[&claude], codexbar::settings::Language::English, true);

        let line = tooltip.lines().next().expect("provider tooltip line");
        assert!(
            line.starts_with("Claude · Session (5h) · 13% used · Jun 10 at 3:00"),
            "{line}"
        );
        assert!(line.ends_with("..."), "{line}");
        assert!(line.chars().count() <= 65, "{line}");
    }

    #[test]
    fn tooltip_honors_remaining_mode_and_names_the_value() {
        let claude = fake_snapshot("claude", "Claude", 13.0);

        let tooltip = build_tooltip(&[&claude], codexbar::settings::Language::English, false);

        assert!(
            tooltip.contains("Claude · Session (5h) · 87% left"),
            "{tooltip}"
        );
        assert!(!tooltip.contains("13%"), "{tooltip}");
    }

    #[test]
    fn tray_tooltip_is_compact_and_language_neutral() {
        let mut claude = fake_snapshot("claude", "Claude", 13.0);
        claude.primary.resets_at =
            Some((chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339());

        // The native tooltip is intentionally compact and language-neutral
        // (percent + a universal d/h/m reset) so every provider fits.
        let tooltip = build_tooltip(&[&claude], codexbar::settings::Language::English, true);
        assert!(tooltip.contains("Claude · Session (5h) · 13%"), "{tooltip}");
        assert!(
            !tooltip.to_ascii_lowercase().contains("resets in"),
            "{tooltip}"
        );

        // The tray menu status labels carry the reset text.
        let (_, label) = provider_status_label(&claude, codexbar::settings::Language::English);
        assert!(label.contains("Resets in"), "{label}");
    }

    #[test]
    fn claude_automatic_surfaces_hot_weekly_over_fresh_session() {
        // SOU-288: automatic metric for Claude must pick the hotter window.
        let mut snapshot = fake_snapshot_with("claude", "Claude", 0.0, Some(100.0), None, None);
        snapshot.primary_label = Some("Session (5h)".to_string());
        snapshot.secondary_label = Some("Weekly".to_string());
        snapshot.primary.window_minutes = Some(300);
        if let Some(weekly) = snapshot.secondary.as_mut() {
            weekly.window_minutes = Some(10_080);
        }
        let settings = Settings::default();

        let (primary, secondary) = selected_tray_used_percents(&snapshot, &settings);
        assert_eq!(primary, 100.0);
        assert_eq!(secondary, Some(100.0));

        let window = selected_metric_window(
            &snapshot,
            Some(ProviderId::Claude),
            MetricPreference::Automatic,
        )
        .expect("claude automatic window");
        assert_eq!(window.used_percent, 100.0);
        assert_eq!(window.window_minutes, Some(10_080));
    }

    #[test]
    fn codex_automatic_surfaces_hot_weekly_over_session() {
        let snapshot = fake_snapshot_with("codex", "Codex", 10.0, Some(88.0), None, None);
        let settings = Settings::default();
        let (primary, _) = selected_tray_used_percents(&snapshot, &settings);
        assert_eq!(primary, 88.0);
    }

    #[test]
    fn selected_tray_percent_uses_cursor_extra_usage_cost() {
        let mut settings = Settings::default();
        settings.set_provider_metric(ProviderId::Cursor, MetricPreference::ExtraUsage);
        let snapshot = fake_snapshot_with(
            "cursor",
            "Cursor",
            10.0,
            Some(20.0),
            Some(72.0),
            Some((15.0, 100.0)),
        );

        let (primary, secondary) = selected_tray_used_percents(&snapshot, &settings);

        assert_eq!(primary, 15.0);
        assert_eq!(secondary, Some(20.0));
    }

    #[test]
    fn selected_tray_percent_tracks_extra_rate_window() {
        let mut settings = Settings::default();
        settings.set_provider_metric(ProviderId::Copilot, MetricPreference::ExtraUsage);
        let mut snapshot = fake_snapshot("copilot", "Copilot", 20.0);
        snapshot.extra_rate_windows.push(fake_extra_window(42.0));

        let (primary, secondary) = selected_tray_used_percents(&snapshot, &settings);

        assert_eq!(primary, 42.0);
        assert_eq!(secondary, None);
    }

    #[test]
    fn copilot_automatic_tracks_highest_extra_rate_window() {
        let settings = Settings::default();
        let mut snapshot = fake_snapshot("copilot", "Copilot", 20.0);
        snapshot.extra_rate_windows.push(fake_extra_window(42.0));

        let (primary, _) = selected_tray_used_percents(&snapshot, &settings);

        assert_eq!(primary, 42.0);
    }

    #[test]
    fn tray_icon_pressure_stays_in_used_units_when_display_shows_remaining() {
        let mut settings = Settings {
            show_as_used: false,
            ..Settings::default()
        };
        settings.set_provider_metric(ProviderId::Cursor, MetricPreference::ExtraUsage);
        let snapshot = fake_snapshot_with(
            "cursor",
            "Cursor",
            10.0,
            Some(20.0),
            Some(72.0),
            Some((15.0, 100.0)),
        );

        let (primary, secondary) = selected_tray_used_percents(&snapshot, &settings);

        assert_eq!(primary, 15.0);
        assert_eq!(secondary, Some(20.0));
    }

    #[test]
    fn selected_tray_percent_falls_back_when_extra_usage_missing() {
        let mut settings = Settings::default();
        settings.set_provider_metric(ProviderId::Cursor, MetricPreference::ExtraUsage);
        let snapshot = fake_snapshot_with("cursor", "Cursor", 10.0, Some(72.0), None, None);

        let (primary, _) = selected_tray_used_percents(&snapshot, &settings);

        assert_eq!(primary, 72.0);
    }

    /// SBS-927: `num_minutes()` floored a still-future 30s remainder to 0,
    /// so the taskbar strip and tray tooltip showed "0m" for up to a minute.
    #[test]
    fn tooltip_short_reset_does_not_show_zero_minutes_while_time_remains() {
        let now = "2026-04-02T12:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let resets_at = (now + chrono::Duration::seconds(30)).to_rfc3339();
        assert_eq!(
            tooltip_short_reset_at(Some(&resets_at), None, now).as_deref(),
            Some("1m")
        );
    }

    /// A reset 200ms out is still a reset in the future. Passing the gap as
    /// seconds truncated it to zero and put "0m" in the tooltip, while the
    /// TypeScript hooks read the same instant as "1m".
    #[test]
    fn tooltip_short_reset_keeps_a_sub_second_remainder_at_one_minute() {
        let now = "2026-04-02T12:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let resets_at = (now + chrono::Duration::milliseconds(200)).to_rfc3339();
        assert_eq!(
            tooltip_short_reset_at(Some(&resets_at), None, now).as_deref(),
            Some("1m")
        );
    }

    /// SBS-927: 24h 1s and 24h 0m must be "1d 0h", the same class of string
    /// the CLI and the TypeScript hooks already emit.
    #[test]
    fn tooltip_short_reset_cuts_a_day_at_twenty_four_hours() {
        let now = "2026-04-02T12:00:00Z"
            .parse::<chrono::DateTime<chrono::Utc>>()
            .unwrap();
        let at = |seconds: i64| (now + chrono::Duration::seconds(seconds)).to_rfc3339();
        assert_eq!(
            tooltip_short_reset_at(Some(&at(24 * 3600)), None, now).as_deref(),
            Some("1d 0h")
        );
        assert_eq!(
            tooltip_short_reset_at(Some(&at(24 * 3600 + 1)), None, now).as_deref(),
            Some("1d 0h")
        );
        assert_eq!(
            tooltip_short_reset_at(Some(&at(24 * 3600 + 30 * 60)), None, now).as_deref(),
            Some("1d 0h")
        );
        assert_eq!(
            tooltip_short_reset_at(Some(&at(25 * 3600)), None, now).as_deref(),
            Some("1d 1h")
        );
    }
}
