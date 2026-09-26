//! Detached "Pop Out Dashboard" flyout window: a resizable, always-on-top,
//! tray-anchored panel that auto-hides on click-outside (blur-dismiss).
//!
//! Runs as an auxiliary Tauri window labeled `flyout`, independent of the
//! `main` window's surface state machine — it coexists with "Show Window"
//! (`SurfaceMode::PopOut`, which stays on `main`) instead of being a
//! mutually-exclusive state of the same window.
//!
//! Structurally modeled on `crate::floatbar` (self-contained module owning
//! its window + a `handle_window_event` hook dispatched from `main.rs`
//! before the `main`-window-only handling); the window itself is built with
//! `settings_window.rs`'s builder recipe (async open, manual DWM dark-caption
//! pass, `WebviewUrl::App` with a `?window=` query marker).

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, PhysicalPosition, WebviewUrl};

use crate::geometry_store::{self, StoredSize};
use crate::state::AppState;
use crate::surface::SurfaceMode;

pub const FLYOUT_LABEL: &str = "flyout";
const FLYOUT_WIDTH: f64 = 344.0;
const FLYOUT_INITIAL_HEIGHT: f64 = 174.0;

/// Geometry-store key for the flyout's remembered SIZE (position is never
/// stored — the flyout always re-anchors above the tray on open). Kept as
/// its own key (distinct from the legacy `SurfaceMode::TrayPanel::as_str()`
/// `"trayPanel"` key) — `geometry_store::load_size` migrates a pre-existing
/// `"trayPanel"` entry into this key on first read, so upgrading users keep
/// their remembered flyout size.
const FLYOUT_SIZE_KEY: &str = "flyout";

/// Grace period after showing the flyout during which a spurious Windows
/// blur (tray click focus race) is ignored — mirrors `main.rs`'s 500ms
/// `was_tray_panel_recently_shown` guard for the old shared window.
const RECENTLY_SHOWN_GRACE: Duration = Duration::from_millis(500);

/// Serializes `open_or_focus`. The taskbar widget spawns one async open per
/// hover-dwell and another per click, so a click landing while a hover-open
/// is still building the window used to run a second, concurrent open —
/// see `existing_window_action` for what that second open must not do.
/// Same pattern as `shell::SHELL_TRANSITION_SERIAL`.
static OPEN_SERIAL: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// How long an open leaves a freshly built window to the frontend's reveal
/// before showing it anyway. The reveal normally lands within a few hundred
/// milliseconds of `build()`; past this the frontend is assumed to have
/// failed (render error, rejected command), and a blank-but-visible window
/// the user can dismiss beats an invisible one nothing can open.
const REVEAL_GRACE: Duration = Duration::from_millis(1500);

/// What `open_or_focus` does with a flyout window that already exists (and
/// is live — `reclaim_dead_window` has already dropped a dead one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExistingWindowAction {
    /// Already revealed at least once (or the reveal is overdue):
    /// reposition, show, focus.
    Show,
    /// Built but not yet revealed by the frontend's first layout pass.
    /// Showing it now would paint the pre-measure blank frame the
    /// `.visible(false)` + `reveal_tray_panel_window` handshake exists to
    /// prevent, so leave it to the pending reveal.
    AwaitReveal,
}

/// `reveal_pending_for` is how long the window's first reveal has been
/// outstanding, or `None` once it has happened (or was never armed).
fn existing_window_action(reveal_pending_for: Option<Duration>) -> ExistingWindowAction {
    if reveal_pending_for.is_some_and(|waited| waited < REVEAL_GRACE) {
        ExistingWindowAction::AwaitReveal
    } else {
        ExistingWindowAction::Show
    }
}

/// Read the remembered flyout size, if any (migrating a legacy
/// `"trayPanel"`-keyed size on first read — see `geometry_store::load_size`).
pub fn stored_size() -> Option<(u32, u32)> {
    geometry_store::load_size(FLYOUT_SIZE_KEY).map(|size| (size.width, size.height))
}

/// Persist a user-chosen flyout size. Size-only — no fabricated position.
pub fn save_stored_size(width: u32, height: u32) {
    geometry_store::save_size(FLYOUT_SIZE_KEY, StoredSize { width, height });
}

/// Build (first open) or show + focus (subsequent opens) the flyout window at
/// `position`, if given, else the default tray-anchored position.
///
/// `WebviewWindowBuilder::build` deadlocks when called synchronously from a
/// Tauri command on Windows (see `commands/surface.rs::open_settings_window`
/// precedent) — callers must invoke this from an async context (an `async`
/// command, or `tauri::async_runtime::spawn`), never a sync command handler.
pub fn open_or_focus(app: &AppHandle, position: Option<(i32, i32)>) -> Result<(), String> {
    // Held across the whole open, including the blocking `build()`, so a
    // second caller waits for the first build to finish and then sees the
    // window with its reveal still pending instead of racing it. Taken
    // before the rebuild wait below: the rebuild thread releases its
    // in-flight flag before it calls back into this function, so a holder
    // waiting on that flag is never waiting on a thread that wants this lock.
    let _open_guard = OPEN_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    // A ProcessFailed teardown of this very window may still be waiting for
    // its label; opening into that would show the dying frame or build a
    // second window under the label (#410).
    super::window_recovery::wait_for_flyout_rebuild();

    // A WebView2 process exit leaves the hidden frame with no content; drop it
    // so the build path below runs instead of showing a blank window (#410).
    crate::webview_recovery::reclaim_dead_window(app, FLYOUT_LABEL)?;

    if let Some(window) = app.get_webview_window(FLYOUT_LABEL) {
        let reveal_pending_for = reveal_pending_for(app);
        match existing_window_action(reveal_pending_for) {
            ExistingWindowAction::Show => {
                if let Some(waited) = reveal_pending_for {
                    // The frontend never revealed the window it was handed.
                    // Show it anyway so the user is not locked out; a blank
                    // panel they can dismiss beats an invisible one.
                    tracing::warn!(
                        waited_ms = waited.as_millis(),
                        "flyout_window: first reveal never arrived; showing the window anyway"
                    );
                    clear_reveal(app);
                }
                if let Some((x, y)) = position {
                    let _ = window.set_position(PhysicalPosition::new(x, y));
                } else {
                    reanchor(app)?;
                }
                window.show().map_err(|e| e.to_string())?;
                window.set_focus().map_err(|e| e.to_string())?;
                if show_grace_starts_now(false) {
                    mark_shown(app);
                }
                return Ok(());
            }
            ExistingWindowAction::AwaitReveal => {
                tracing::debug!(
                    "flyout_window: open requested while the first reveal is pending; leaving it to the frontend"
                );
                return Ok(());
            }
        }
    }

    let url = WebviewUrl::App("index.html?window=flyout".into());

    let builder = tauri::WebviewWindowBuilder::new(app, FLYOUT_LABEL, url)
        .title("Ceiling")
        .inner_size(FLYOUT_WIDTH, FLYOUT_INITIAL_HEIGHT)
        .decorations(false)
        .shadow(true)
        .transparent(true)
        // This is a glance surface, not a second dashboard. Its React surface
        // sizes the window to live provider content; users should never see a
        // resize cursor or a remembered legacy tray-panel dimension.
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .theme(Some(tauri::Theme::Dark))
        // CRITICAL: dynamically-built windows default to drag-drop ENABLED,
        // which intercepts the HTML5 draggable events the provider grid's
        // drag-reorder (ProviderGrid.tsx) relies on — see `main`'s
        // `dragDropEnabled: false` in tauri.conf.json for why this must be
        // disabled explicitly on every window that hosts that grid.
        .disable_drag_drop_handler()
        .visible(false);

    // Armed BEFORE `build()`: the page starts loading on the browser process
    // the moment the controller exists, and its reveal can reach
    // `reveal_tray_panel_window` before `build()` has even returned to this
    // thread. Arming afterwards would drop that reveal and leave the flag set
    // with nothing left to consume it.
    arm_reveal(app)?;
    let win = match builder.build() {
        Ok(win) => win,
        Err(error) => {
            clear_reveal(app);
            return Err(error.to_string());
        }
    };
    // ...but `REVEAL_GRACE` must not count the build. An open that waited on
    // `OPEN_SERIAL` for a slow first build would otherwise find the reveal
    // "overdue" the moment it gets the lock and show the pre-layout frame.
    // A no-op when the reveal already landed during the build.
    restamp_reveal(app);
    super::webview_lifecycle::watch(app, &win);

    super::dwm::force_dark_caption(&win);

    let target_position =
        position.or_else(|| super::position::default_surface_position(app, SurfaceMode::TrayPanel));
    if let Some((x, y)) = target_position {
        let _ = win.set_position(PhysicalPosition::new(x, y));
    }

    // Left `.visible(false)` above — the frontend reveals the window itself
    // after its first layout pass, then `reveal_tray_panel_window` starts the
    // recently-shown blur grace at the real show/focus boundary.
    if show_grace_starts_now(true) {
        mark_shown(app);
    }
    Ok(())
}

fn show_grace_starts_now(first_build_hidden: bool) -> bool {
    !first_build_hidden
}

/// How long the window's first reveal has been outstanding, or `None` when
/// none is pending. Missing or poisoned state reads as "not pending", which
/// routes the open to `Show` — the pre-existing behaviour — rather than
/// leaving the window invisible.
fn reveal_pending_for(app: &AppHandle) -> Option<Duration> {
    app.try_state::<Mutex<AppState>>()
        .and_then(|state| state.lock().ok()?.flyout_reveal_pending_for(Instant::now()))
}

fn clear_reveal(app: &AppHandle) {
    if let Some(st) = app.try_state::<Mutex<AppState>>()
        && let Ok(mut guard) = st.lock()
    {
        guard.clear_flyout_reveal();
    }
}

fn restamp_reveal(app: &AppHandle) {
    if let Some(st) = app.try_state::<Mutex<AppState>>()
        && let Ok(mut guard) = st.lock()
    {
        guard.restamp_flyout_reveal(Instant::now());
    }
}

fn arm_reveal(app: &AppHandle) -> Result<(), String> {
    let state = app
        .try_state::<Mutex<AppState>>()
        .ok_or_else(|| "app state unavailable".to_string())?;
    state
        .lock()
        .map_err(|e| e.to_string())?
        .arm_flyout_reveal(Instant::now());
    Ok(())
}

/// Hide (never close) the flyout window. Hiding — rather than closing —
/// keeps the window's WebView2 instance alive across opens, matching
/// `settings_window::dismiss`'s rationale: closing risks Tauri's
/// process/window lifecycle treating it as an app-relevant close.
pub fn hide(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(FLYOUT_LABEL) {
        let state = app
            .try_state::<Mutex<AppState>>()
            .ok_or_else(|| "app state unavailable".to_string())?;
        state
            .lock()
            .map_err(|e| e.to_string())?
            .clear_flyout_reveal();
        window.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn mark_shown(app: &AppHandle) {
    if let Some(st) = app.try_state::<Mutex<AppState>>()
        && let Ok(mut guard) = st.lock()
    {
        guard.mark_tray_panel_shown(Instant::now());
    }
}

/// Handle a `WindowEvent` targeting the flyout window. Returns `true` when
/// the event was for the flyout (and was handled), `false` otherwise so the
/// caller (`main.rs`'s single `on_window_event` dispatcher) can fall through
/// to its own `main`-window-only handling.
///
/// Ports the same four-guard chain `main.rs` applies to the old shared
/// window's `Focused(false)` (proof-mode suppression / startup grace /
/// recently-shown 500ms grace / gesture blur guard) so every previously
/// shipped anti-flicker behavior survives the window split.
pub fn handle_window_event(window: &tauri::Window, event: &tauri::WindowEvent) -> bool {
    if window.label() != FLYOUT_LABEL {
        return false;
    }

    let app = window.app_handle();

    match event {
        tauri::WindowEvent::Focused(false) => {
            if crate::proof_harness::is_proof_mode(app) {
                return true;
            }
            // A window that has never been shown cannot lose focus in any
            // sense the user meant. Windows still reports one when a hidden
            // window is activated without foreground rights (e.g. right after
            // build, before the frontend's reveal); treating that as a
            // dismiss would clear the pending reveal and leave the flyout
            // invisible. A failed query (the window is mid-teardown) is
            // treated the same way: swallowing one blur is cheaper than
            // dismissing a flyout that is about to be rebuilt.
            if !window.is_visible().unwrap_or(false) {
                return true;
            }
            let Some(st) = app.try_state::<Mutex<AppState>>() else {
                return true;
            };
            {
                let mut guard = st.lock().unwrap();
                if guard.take_startup_tray_blur_grace(Instant::now()) {
                    return true;
                }
                if guard.was_tray_panel_recently_shown(Instant::now(), RECENTLY_SHOWN_GRACE) {
                    return true;
                }
                if guard.is_gesture_blur_guard_active(Instant::now()) {
                    return true;
                }
            }
            let _ = hide(app);
            true
        }
        tauri::WindowEvent::Focused(true) => {
            if let Some(st) = app.try_state::<Mutex<AppState>>() {
                st.lock()
                    .unwrap()
                    .clear_gesture_guard_on_refocus(Instant::now());
            }
            true
        }
        // Size persistence is entirely frontend-driven (genuine user
        // drag-resizes call `set_flyout_size`, auto-fit resizes never do) —
        // mirrors `shell::position::remember_current_geometry_if_eligible`
        // skipping TrayPanel for the same reason on the old shared window.
        // Position is never persisted (always re-anchored above the tray).
        tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => true,
        tauri::WindowEvent::CloseRequested { api, .. } => {
            // Hide-not-close, matching Settings/FloatBar lifecycle handling —
            // the flyout must survive a native close (Alt+F4-equivalent from
            // a screen reader, etc.) so it can be reopened without rebuilding
            // the WebView2 instance.
            api.prevent_close();
            let _ = hide(app);
            true
        }
        _ => true,
    }
}

/// Reposition the flyout so its bottom-right corner stays anchored to the
/// system-tray area, using the window's CURRENT logical size (after a
/// frontend-driven resize). Canonical anchor-math implementation for the
/// flyout window; the `reanchor_tray_panel` Tauri command
/// (`commands/system.rs`) is a thin retarget onto this function.
pub fn reanchor(app: &AppHandle) -> Result<(), String> {
    use crate::window_positioner::{PanelSize, Rect};

    let window = app
        .get_webview_window(FLYOUT_LABEL)
        .ok_or_else(|| "flyout window unavailable".to_string())?;
    let scale = window.scale_factor().unwrap_or(1.0).max(1.0);

    let outer = window.outer_size().map_err(|e| e.to_string())?;
    let panel_size = PanelSize {
        width: (outer.width as f64 / scale).round() as u32,
        height: (outer.height as f64 / scale).round() as u32,
    };

    let anchor = app
        .try_state::<Mutex<AppState>>()
        .and_then(|state| state.lock().ok()?.tray_anchor);
    let monitors = window.available_monitors().unwrap_or_default();
    let monitor = anchor
        .and_then(|anchor| crate::shell::geometry::monitor_for_anchor(&monitors, anchor))
        .cloned()
        .or_else(|| window.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| "no monitor".to_string())?;

    let work_area = crate::shell::geometry::monitor_work_area_rect(&monitor);
    let monitor_bounds = Rect {
        x: monitor.position().x,
        y: monitor.position().y,
        width: monitor.size().width,
        height: monitor.size().height,
    };

    let (x, y) = {
        if let Some(a) = anchor {
            crate::window_positioner::calculate_panel_position(
                &Rect {
                    x: a.x,
                    y: a.y,
                    width: a.width,
                    height: a.height,
                },
                &monitor_bounds,
                &work_area,
                &panel_size,
                scale,
            )
        } else {
            // No real click anchor yet: infer one from the taskbar side
            // (handles left/right/top-docked taskbars, not just bottom-right).
            crate::shell::inferred_tray_panel_position_for_monitor_size(&monitor, &panel_size)
        }
    };

    // Pass physical coordinates directly — tao converts PhysicalPosition to
    // OS logical internally by dividing by the window's scale factor.
    let pos = tauri::PhysicalPosition::new(x, y);
    tracing::debug!(
        "flyout_window::reanchor: panel={}x{} => ({},{})",
        panel_size.width,
        panel_size.height,
        pos.x,
        pos.y
    );
    let _ = window.set_position(pos);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flyout_label_is_stable() {
        assert_eq!(FLYOUT_LABEL, "flyout");
    }

    #[test]
    fn flyout_size_key_is_distinct_from_legacy_tray_panel_key() {
        // The whole point of the migration in geometry_store::load_size is
        // that this key differs from the legacy SurfaceMode::TrayPanel key
        // ("trayPanel") — otherwise there'd be nothing to migrate FROM.
        assert_ne!(FLYOUT_SIZE_KEY, SurfaceMode::TrayPanel.as_str());
    }

    #[test]
    fn taskbar_flyout_has_a_small_dedicated_initial_shape() {
        assert_eq!(FLYOUT_WIDTH, 344.0);
        assert_eq!(FLYOUT_INITIAL_HEIGHT, 174.0);
        assert_ne!(
            FLYOUT_WIDTH,
            SurfaceMode::TrayPanel.window_properties().width
        );
    }

    #[test]
    fn first_hidden_build_does_not_start_show_grace() {
        assert!(show_grace_starts_now(false));
        assert!(!show_grace_starts_now(true));
    }

    #[test]
    fn a_revealed_healthy_window_is_simply_shown() {
        assert_eq!(existing_window_action(None), ExistingWindowAction::Show);
    }

    #[test]
    fn an_open_during_a_pending_reveal_waits_for_the_frontend() {
        // The hover/click race from #410 (finding A): the window exists but
        // is still `.visible(false)` awaiting `reveal_tray_panel_window`.
        // Showing it here would bypass the handshake and flash a blank frame.
        assert_eq!(
            existing_window_action(Some(Duration::from_millis(200))),
            ExistingWindowAction::AwaitReveal
        );
    }

    #[test]
    fn an_overdue_reveal_stops_blocking_opens() {
        // If the frontend never reveals (render error, rejected command), a
        // no-op open forever would leave the flyout unopenable until restart —
        // the very symptom #410 is about. Past the grace, show it anyway.
        assert_eq!(
            existing_window_action(Some(REVEAL_GRACE)),
            ExistingWindowAction::Show
        );
        assert_eq!(
            existing_window_action(Some(Duration::from_secs(30))),
            ExistingWindowAction::Show
        );
    }
}
