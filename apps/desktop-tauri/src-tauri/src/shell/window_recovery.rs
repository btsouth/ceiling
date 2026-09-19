//! Off-thread window rebuilds (#410).
//!
//! [`crate::webview_recovery`] can tell whether a window still hosts a live
//! WebView2 instance and, for the windows that are opened from an async
//! context, drops a dead one inline so the caller's first-build path runs.
//! `main` cannot be handled that way: `tray_bridge::handle_menu_event` calls
//! `shell::transition_to_target` synchronously on the menu-event thread and
//! `commands::set_surface_mode` is a *sync* Tauri command, so its guard is
//! reached on the main thread, where waiting for the destroyed window's label
//! to be released would block the very event loop that has to process
//! `Destroyed` — a guaranteed stall followed by a failed rebuild.
//!
//! So the `main` guard hands the destroy + rebuild to a background thread and
//! tells its caller to abandon this attempt. The request is kept as the
//! rebuild's pending replay and re-issued on the main thread once the window
//! is healthy again, so the click that hit the dead window still ends up
//! doing what the user asked — just a beat later. Every path that can notice
//! a dead `main` (the transition guards, the hide-to-tray guard and the
//! `ProcessFailed` handler) merges into the same pending replay, so it does
//! not matter which of them wins the race to start the rebuild.
//!
//! The guard itself never asks the event loop for anything: it probes the
//! native handle captured when the window was built. Transitions hold
//! `SHELL_TRANSITION_SERIAL` while they run, some of them from spawned tasks,
//! and a marshalled window getter under that lock can wait on a main thread
//! that is itself waiting for the lock.
//!
//! The same background rebuilds serve [`super::webview_lifecycle`], which
//! learns about a dead webview from WebView2 itself (inside a COM callback
//! on the UI thread, so it is under the same must-not-block rule) rather
//! than from a probe at show time. The `*_after_loss` entry points are its
//! side of the contract, and they exist for every window because the shared
//! browser process takes all of them down at once.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager, WebviewWindow};

use crate::surface::SurfaceMode;
use crate::surface_target::SurfaceTarget;
use crate::webview_recovery;

pub(crate) const MAIN_LABEL: &str = "main";

/// How long an open may wait for a lifecycle-driven rebuild of the same
/// window to release its label (see `wait_for_flyout_rebuild`).
const REBUILD_WAIT_POLLS: usize = 200;
const REBUILD_WAIT_POLL: Duration = Duration::from_millis(10);

/// `main`'s native handle, captured right after each build so the liveness
/// probe never has to ask the event loop for it. Zero until the first build
/// is registered, which the probe treats as "alive" (never rebuild on a
/// guess).
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

/// The one `main` rebuild that may be in flight, and what to replay once it
/// lands. A later request replaces an earlier one; a hide (`None`) never
/// clears a pending replay. See `merge_replay`.
struct MainRebuild {
    in_flight: bool,
    replay: Option<MainRequest>,
}

impl MainRebuild {
    /// Queue `request` as the replay and claim the in-flight slot if it is
    /// free. Returns whether the caller now owns the slot and must start
    /// the rebuild thread.
    fn enqueue(&mut self, request: Option<MainRequest>) -> bool {
        self.replay = merge_replay(self.replay.take(), request);
        if self.in_flight {
            return false;
        }
        self.in_flight = true;
        true
    }

    /// Release the in-flight slot and claim the queued replay in one step.
    ///
    /// Both must happen under the same lock: a request that lands between
    /// them would be merged into a queue nobody reads (`enqueue` sees the
    /// slot taken and does not spawn; this thread has already taken its
    /// replay) and stay there until the next crash. Done together, it is
    /// either claimed here or finds the slot free and starts its own
    /// rebuild.
    fn finish(&mut self) -> Option<MainRequest> {
        self.in_flight = false;
        self.replay.take()
    }
}

static MAIN_REBUILD: Mutex<MainRebuild> = Mutex::new(MainRebuild {
    in_flight: false,
    replay: None,
});

/// Set while a rebuild thread is between `destroy()` and a released label,
/// so a burst of `ProcessFailed` events queues one recovery per window.
static SETTINGS_REBUILD_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static FLYOUT_REBUILD_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static FLOATBAR_REBUILD_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// An in-flight flag that clears itself when dropped, so a rebuild thread
/// that panics cannot leave its window unrecoverable for the session.
struct InFlight(&'static AtomicBool);

impl InFlight {
    fn claim(flag: &'static AtomicBool) -> Option<Self> {
        (!flag.swap(true, Ordering::SeqCst)).then_some(Self(flag))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn main_rebuild() -> std::sync::MutexGuard<'static, MainRebuild> {
    lock_rebuild(&MAIN_REBUILD)
}

fn lock_rebuild(slot: &Mutex<MainRebuild>) -> std::sync::MutexGuard<'_, MainRebuild> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The rebuild thread's hold on the `main` in-flight slot.
///
/// Clears the slot if the thread panics before `finish`, so a panic cannot
/// leave `main` unrecoverable for the session; a pending replay survives
/// the panic and is picked up by the next dispatch. Once `finish` has run
/// the drop is a no-op: `finish` already released the slot under the lock,
/// and a caller queued on that lock may have claimed it in the meantime.
/// Clearing again would wipe that claim and let the next caller start a
/// second rebuild of `main` while the first is still running.
struct Ticket<'a> {
    slot: &'a Mutex<MainRebuild>,
    armed: bool,
}

impl<'a> Ticket<'a> {
    fn new(slot: &'a Mutex<MainRebuild>) -> Self {
        Self { slot, armed: true }
    }

    /// Release the slot and take the replay together (`MainRebuild::finish`),
    /// then disarm so the drop leaves the slot alone.
    fn finish(&mut self) -> Option<MainRequest> {
        self.armed = false;
        lock_rebuild(self.slot).finish()
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        if self.armed {
            lock_rebuild(self.slot).in_flight = false;
        }
    }
}

/// What a caller should do with a window it is about to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuardAction {
    /// The window is there and its webview is live — carry on.
    Proceed,
    /// The window needs rebuilding and this caller should start it.
    Rebuild,
    /// A rebuild is already running; abandon this attempt without starting
    /// a second one.
    Waiting,
}

/// Decide what to do about a window, from facts a caller can cheaply gather.
///
/// Split out from the Win32 probe and the thread spawning so the policy is
/// testable on its own: a missing window is as recoverable as a dead one
/// (Tauri drops a window from its label map once `Destroyed` is processed,
/// so an interrupted earlier recovery leaves exactly that shape), and an
/// in-flight rebuild always wins so concurrent callers cannot stack up
/// destroy/build cycles on the same label.
pub(crate) fn guard_action(exists: bool, alive: bool, rebuild_in_flight: bool) -> GuardAction {
    if rebuild_in_flight {
        return GuardAction::Waiting;
    }
    if exists && alive {
        return GuardAction::Proceed;
    }
    GuardAction::Rebuild
}

/// The replay to keep when `incoming` arrives while `pending` is queued.
///
/// The newest request is what the user asked for most recently, so it wins;
/// a hide (`None`) has nothing to replay and must not erase a queued open,
/// or a `Focused(false)` that reaches the dying window a few milliseconds
/// before the `ProcessFailed` callback would silently cancel the reopen.
fn merge_replay(
    pending: Option<MainRequest>,
    incoming: Option<MainRequest>,
) -> Option<MainRequest> {
    incoming.or(pending)
}

/// The transition to replay once `main` has been rebuilt.
///
/// The rebuilt window is hidden and the surface state is reset to `Hidden`
/// with it (see `rebuild_main`), so a plain `transition_to_target` always
/// resolves as a mode change and shows the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MainRequest {
    pub mode: SurfaceMode,
    pub target: SurfaceTarget,
    pub position: Option<(i32, i32)>,
}

/// Remember a freshly built `main` window: capture its native handle for
/// the liveness probe and subscribe it to `ProcessFailed`. Call right after
/// `build()`, at startup and on every rebuild.
pub(crate) fn register_main(app: &AppHandle, window: &WebviewWindow) {
    MAIN_HWND.store(
        webview_recovery::native_hwnd(window).unwrap_or(0),
        Ordering::SeqCst,
    );
    super::webview_lifecycle::watch(app, window);
}

/// Whether the registered `main` window still hosts a live webview. Pure
/// Win32 on the captured handle: nothing here waits on the event loop.
fn main_webview_is_alive() -> bool {
    match MAIN_HWND.load(Ordering::SeqCst) {
        0 => true,
        hwnd => webview_recovery::hwnd_has_webview_child(hwnd),
    }
}

/// `main`'s webview was reported dead by WebView2 itself: rebuild it now,
/// hidden, exactly as at startup. When it was on screen, replay the current
/// surface at `position` so it comes back where the user had it.
pub(crate) fn recover_main_after_loss(app: &AppHandle, replay: bool, position: Option<(i32, i32)>) {
    let request = replay
        .then(|| {
            let state = app.try_state::<Mutex<crate::state::AppState>>()?;
            let snapshot = {
                let guard = state.lock().unwrap_or_else(|error| error.into_inner());
                super::transition::current_surface_snapshot(&guard)
            };
            tracing::debug!(
                mode = ?snapshot.mode,
                target = ?snapshot.target,
                "window_recovery: surface to replay after the main rebuild"
            );
            (snapshot.mode != SurfaceMode::Hidden).then_some(MainRequest {
                mode: snapshot.mode,
                target: snapshot.target,
                position,
            })
        })
        .flatten();
    dispatch_main_rebuild(app, request);
}

/// Resolve the `main` window, or start recovering it.
///
/// `Some(window)` means the window is live and the caller may proceed with
/// its transition. `None` means the caller must abandon this attempt: a
/// rebuild is either already running or has just been dispatched, and
/// `request` (when given) is queued as its replay.
pub(crate) fn resolve_live_main(
    app: &AppHandle,
    request: Option<MainRequest>,
) -> Option<WebviewWindow> {
    let window = app.get_webview_window(MAIN_LABEL);
    let alive = window.is_some() && main_webview_is_alive();
    let in_flight = main_rebuild().in_flight;

    match guard_action(window.is_some(), alive, in_flight) {
        GuardAction::Proceed => return window,
        GuardAction::Waiting => {
            tracing::debug!(
                queued = request.is_some(),
                "window_recovery: main rebuild already in flight; deferring this open"
            );
        }
        GuardAction::Rebuild => {
            tracing::warn!(
                label = MAIN_LABEL,
                "window_recovery: main window is not usable; rebuilding it (#410)"
            );
        }
    }
    dispatch_main_rebuild(app, request);
    None
}

/// Queue `request` as the pending replay and, unless one is already
/// running, destroy and rebuild `main` on a background thread.
///
/// Spawns rather than blocking because the caller may be the main thread;
/// `std::thread::spawn` (not the async runtime) matches the existing
/// precedent in `transition.rs`'s startup reveal fallback and keeps the
/// blocking label wait off a tokio worker.
fn dispatch_main_rebuild(app: &AppHandle, request: Option<MainRequest>) {
    if !main_rebuild().enqueue(request) {
        return;
    }

    let app = app.clone();
    let _ = std::thread::spawn(move || {
        let mut ticket = Ticket::new(&MAIN_REBUILD);
        let result = rebuild_main(&app);
        let replay = ticket.finish();

        match result {
            Ok(()) => {
                tracing::info!(label = MAIN_LABEL, "window_recovery: main window rebuilt");
                if let Some(request) = replay {
                    replay_on_main_thread(&app, request);
                }
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    label = MAIN_LABEL,
                    replay_dropped = replay.is_some(),
                    "window_recovery: could not rebuild the main window"
                );
            }
        }
    });
}

/// Replay `request` against the rebuilt `main` — on the main thread, never
/// on the rebuild thread.
///
/// A transition holds `SHELL_TRANSITION_SERIAL` while it calls window
/// getters, and the main thread's window-event handler takes that same lock
/// (`hide_to_tray_if_current` on `Focused(false)`). Run from a background
/// thread, the replay can therefore deadlock: it holds the lock and waits
/// for the main thread to answer a getter, while the main thread sits in an
/// event handler waiting for the lock. That is not hypothetical — a freshly
/// built window receives a `Focused(true)`/`Focused(false)` pair right after
/// `build()`, so the race was hit on every rebuild of a visible `main`
/// (observed 2026-09-11). On the main thread every window call takes the
/// runtime's direct path and the event handler cannot run concurrently, which
/// is exactly why the tray and menu paths run their transitions there.
///
/// The window is healthy by now, so this pass takes the normal path —
/// `resolve_live_main` returns it and no further rebuild is dispatched.
fn replay_on_main_thread(app: &AppHandle, request: MainRequest) {
    let handle = app.clone();
    let dispatched = app.run_on_main_thread(move || {
        if let Err(error) =
            super::transition_to_target(&handle, request.mode, request.target, request.position)
        {
            tracing::warn!(
                %error,
                "window_recovery: replaying the transition after rebuild failed"
            );
        }
    });
    if let Err(error) = dispatched {
        tracing::warn!(
            %error,
            "window_recovery: could not dispatch the post-rebuild replay to the main thread"
        );
    }
}

/// Rebuild `main` from the same `tauri.conf.json` entry the first build uses.
///
/// `main` is declared in the config rather than built in code, so the only
/// faithful recipe is `WebviewWindowBuilder::from_config` over that entry —
/// which also means the rebuilt window inherits `"visible": false` and the
/// rest of its declared properties, exactly as at startup. What `setup` does
/// to it afterwards (register it, `force_dark_caption`, `hide()`) is repeated
/// here so a recovered window is indistinguishable from a freshly-launched
/// one, and in particular can never flash a frame of its own before the
/// surface machinery decides to show it.
///
/// The surface state is reset to `Hidden` to match: it still reads the mode
/// the dead window was showing, and a transition back to that mode would
/// otherwise resolve as a no-op and leave the rebuilt window hidden forever.
fn rebuild_main(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        webview_recovery::destroy_and_release(app, &window)?;
    }

    let config = app
        .config()
        .app
        .windows
        .first()
        .ok_or_else(|| "no window is declared in tauri.conf.json".to_string())?;

    let window = tauri::WebviewWindowBuilder::from_config(app, config)
        .map_err(|error| error.to_string())?
        .build()
        .map_err(|error| error.to_string())?;
    register_main(app, &window);

    super::dwm::force_dark_caption(&window);
    window.hide().map_err(|error| error.to_string())?;
    super::transition::commit_surface_snapshot(app, &super::transition::hidden_surface_snapshot())
}

/// `settings` lost its webview: tear it down now. When it was on screen,
/// reopen it on the default tab — the tab it was showing died with the
/// webview, and the default is where the user lands from the tray too.
///
/// `settings_window::open_or_focus` may run on the main thread (tray menu),
/// so unlike the flyout it cannot wait for this teardown; an open that lands
/// in the few hundred milliseconds between `destroy()` and the released
/// label is handled by `webview_recovery::reclaim_dead_window` as best it
/// can.
pub(crate) fn recover_settings_after_loss(app: &AppHandle, reopen: bool) {
    let Some(ticket) = InFlight::claim(&SETTINGS_REBUILD_IN_FLIGHT) else {
        return;
    };
    let tab = reopen.then(
        || match SurfaceTarget::default_for_mode(SurfaceMode::Settings) {
            SurfaceTarget::Settings { tab } => tab,
            _ => "general".to_string(),
        },
    );
    let app = app.clone();
    let _ = std::thread::spawn(move || {
        let destroyed = match app.get_webview_window(super::settings_window::SETTINGS_LABEL) {
            Some(window) => webview_recovery::destroy_and_release(&app, &window),
            None => Ok(()),
        };
        drop(ticket);

        let tab = match (destroyed, tab) {
            (Err(error), _) => {
                tracing::error!(
                    %error,
                    "window_recovery: could not release the settings window for rebuild"
                );
                return;
            }
            (Ok(()), None) => {
                tracing::info!(
                    label = super::settings_window::SETTINGS_LABEL,
                    "window_recovery: settings window torn down; it will be rebuilt on next open"
                );
                return;
            }
            (Ok(()), Some(tab)) => tab,
        };
        // Re-entering `open_or_focus` rather than duplicating its builder
        // keeps the window's size/geometry/theme recipe in one place; with
        // the label released it takes the first-build branch, which is
        // precisely "the same path the first build uses".
        match super::settings_window::open_or_focus(&app, &tab) {
            Ok(()) => tracing::info!(
                label = super::settings_window::SETTINGS_LABEL,
                "window_recovery: settings window rebuilt"
            ),
            Err(error) => tracing::warn!(
                %error,
                "window_recovery: reopening Settings after rebuild failed"
            ),
        }
    });
}

/// Block, bounded, while a lifecycle-driven flyout teardown is between
/// `destroy()` and the released label, so an open that races it neither
/// shows the dying window nor builds a second one under the same label.
///
/// Like `flyout_window::open_or_focus` itself, this must only run from an
/// async context: on the main thread it would wait on the very event loop
/// the teardown needs.
pub(crate) fn wait_for_flyout_rebuild() {
    for _ in 0..REBUILD_WAIT_POLLS {
        if !FLYOUT_REBUILD_IN_FLIGHT.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(REBUILD_WAIT_POLL);
    }
    tracing::warn!(
        "window_recovery: flyout rebuild still in flight after the wait; opening anyway"
    );
}

/// The flyout lost its webview: tear it down now and, when it was on
/// screen, reopen it.
///
/// `flyout_window::open_or_focus` is the first-build path, so the reopened
/// window is built `visible(false)` and revealed by the frontend after its
/// first layout pass — the same handshake as any other open, which is what
/// keeps the recovery from flashing a blank frame. It re-anchors above the
/// tray on its own, so no position is carried over. `open_or_focus` may
/// block on `build()`, hence the background thread.
pub(crate) fn recover_flyout_after_loss(app: &AppHandle, reopen: bool) {
    let Some(ticket) = InFlight::claim(&FLYOUT_REBUILD_IN_FLIGHT) else {
        return;
    };
    let app = app.clone();
    let _ = std::thread::spawn(move || {
        let destroyed = match app.get_webview_window(super::flyout_window::FLYOUT_LABEL) {
            Some(window) => webview_recovery::destroy_and_release(&app, &window),
            None => Ok(()),
        };
        // Released before `open_or_focus`, which waits on this very flag.
        drop(ticket);

        match destroyed {
            Err(error) => tracing::error!(
                %error,
                "window_recovery: could not release the flyout window for rebuild"
            ),
            Ok(()) if !reopen => tracing::info!(
                label = super::flyout_window::FLYOUT_LABEL,
                "window_recovery: flyout torn down; it will be rebuilt on next open"
            ),
            Ok(()) => match super::flyout_window::open_or_focus(&app, None) {
                Ok(()) => tracing::info!(
                    label = super::flyout_window::FLYOUT_LABEL,
                    "window_recovery: flyout rebuilt"
                ),
                Err(error) => tracing::warn!(
                    %error,
                    "window_recovery: reopening the flyout after rebuild failed"
                ),
            },
        }
    });
}

/// The float bar lost its webview: tear it down and let
/// `floatbar::apply_state` build it again from the persisted settings.
///
/// The bar has no "next open" to fall back on — it is on screen for as long
/// as it is enabled, and neither its z-order guard (which only rebuilds a
/// *missing* window) nor a settings save would touch a frame that is still
/// there — so the dead frame would stay blank until the user toggled the
/// feature. `apply_state` is the same recipe startup uses, and a disabled
/// bar simply stays torn down.
pub(crate) fn recover_floatbar_after_loss(app: &AppHandle) {
    let Some(ticket) = InFlight::claim(&FLOATBAR_REBUILD_IN_FLIGHT) else {
        return;
    };
    let app = app.clone();
    let _ = std::thread::spawn(move || {
        let _ticket = ticket;
        let destroyed = match app.get_webview_window(crate::floatbar::FLOATBAR_LABEL) {
            Some(window) => webview_recovery::destroy_and_release(&app, &window),
            None => Ok(()),
        };
        if let Err(error) = destroyed {
            tracing::error!(
                %error,
                "window_recovery: could not release the float bar window for rebuild"
            );
            return;
        }
        let settings = codexbar::settings::Settings::load();
        match crate::floatbar::apply_state(&app, &settings) {
            Ok(()) => tracing::info!(
                label = crate::floatbar::FLOATBAR_LABEL,
                enabled = settings.float_bar_enabled,
                "window_recovery: float bar torn down and re-applied from settings"
            ),
            Err(error) => tracing::warn!(
                %error,
                "window_recovery: re-applying the float bar after rebuild failed"
            ),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_window_is_shown_as_is() {
        assert_eq!(guard_action(true, true, false), GuardAction::Proceed);
    }

    #[test]
    fn a_dead_window_is_rebuilt() {
        assert_eq!(guard_action(true, false, false), GuardAction::Rebuild);
    }

    #[test]
    fn a_missing_window_is_rebuilt_too() {
        // An earlier recovery that was interrupted after `destroy()` leaves
        // no window under the label at all; that is recoverable, not fatal.
        assert_eq!(guard_action(false, false, false), GuardAction::Rebuild);
    }

    #[test]
    fn an_in_flight_rebuild_wins_over_every_other_shape() {
        // Concurrent tray clicks must not stack destroy/build cycles on one
        // label — including the case where the half-rebuilt window already
        // probes as alive.
        assert_eq!(guard_action(true, false, true), GuardAction::Waiting);
        assert_eq!(guard_action(false, false, true), GuardAction::Waiting);
        assert_eq!(guard_action(true, true, true), GuardAction::Waiting);
    }

    fn open(mode: SurfaceMode) -> MainRequest {
        MainRequest {
            mode,
            target: SurfaceTarget::default_for_mode(mode),
            position: None,
        }
    }

    #[test]
    fn the_newest_open_request_is_the_one_replayed() {
        let pending = open(SurfaceMode::PopOut);
        let incoming = open(SurfaceMode::TrayPanel);
        assert_eq!(
            merge_replay(Some(pending), Some(incoming.clone())),
            Some(incoming)
        );
    }

    #[test]
    fn a_hide_never_cancels_a_queued_open() {
        // A `Focused(false)` reaching the dying window just before the
        // ProcessFailed callback (or a tray hide during the rebuild) has
        // nothing to replay; it must not erase the reopen already queued.
        let pending = open(SurfaceMode::PopOut);
        assert_eq!(merge_replay(Some(pending.clone()), None), Some(pending));
        assert_eq!(merge_replay(None, None), None);
    }

    #[test]
    fn only_the_first_enqueue_owns_the_rebuild() {
        let mut rebuild = MainRebuild {
            in_flight: false,
            replay: None,
        };
        assert!(rebuild.enqueue(Some(open(SurfaceMode::PopOut))));
        assert!(!rebuild.enqueue(Some(open(SurfaceMode::TrayPanel))));
        assert!(rebuild.in_flight);
        assert_eq!(rebuild.replay, Some(open(SurfaceMode::TrayPanel)));
    }

    #[test]
    fn finishing_releases_the_slot_and_claims_the_replay_together() {
        let mut rebuild = MainRebuild {
            in_flight: true,
            replay: Some(open(SurfaceMode::PopOut)),
        };
        assert_eq!(rebuild.finish(), Some(open(SurfaceMode::PopOut)));
        assert!(!rebuild.in_flight);
        assert_eq!(rebuild.replay, None);
    }

    #[test]
    fn a_finished_ticket_leaves_a_later_claim_alone() {
        // `finish` releases the slot; a caller queued on the lock claims it
        // right after; the ticket then goes out of scope. That drop used to
        // clear the slot unconditionally, wiping the new claim, so the
        // caller after that would start a second rebuild of `main`.
        let slot = Mutex::new(MainRebuild {
            in_flight: false,
            replay: None,
        });
        assert!(lock_rebuild(&slot).enqueue(Some(open(SurfaceMode::PopOut))));
        let mut ticket = Ticket::new(&slot);

        assert_eq!(ticket.finish(), Some(open(SurfaceMode::PopOut)));
        assert!(lock_rebuild(&slot).enqueue(Some(open(SurfaceMode::TrayPanel))));
        drop(ticket);

        let state = lock_rebuild(&slot);
        assert!(state.in_flight, "the later claim must survive the ticket");
        assert_eq!(state.replay, Some(open(SurfaceMode::TrayPanel)));
    }

    #[test]
    fn a_ticket_dropped_before_finish_releases_the_slot() {
        // The panic path: the thread unwinds before `finish`, the slot is
        // freed, and the replay stays queued for the next dispatch.
        let slot = Mutex::new(MainRebuild {
            in_flight: false,
            replay: None,
        });
        assert!(lock_rebuild(&slot).enqueue(Some(open(SurfaceMode::PopOut))));
        drop(Ticket::new(&slot));

        let state = lock_rebuild(&slot);
        assert!(!state.in_flight);
        assert_eq!(state.replay, Some(open(SurfaceMode::PopOut)));
    }

    #[test]
    fn a_request_racing_the_completion_is_never_stranded() {
        // The lost-request shape is `in_flight == false` with a replay
        // still queued: the worker took its replay, the request was merged
        // behind it, and the worker then released the slot without looking
        // again. Race `enqueue` against `finish` on a shared state and
        // require that every interleaving hands the request to exactly one
        // side — either the finishing worker replays it or the enqueuer is
        // told to start a rebuild of its own.
        use std::sync::{Arc, Barrier};

        for _ in 0..500 {
            let rebuild = Arc::new(Mutex::new(MainRebuild {
                in_flight: true,
                replay: None,
            }));
            let gate = Arc::new(Barrier::new(2));

            let worker = {
                let rebuild = Arc::clone(&rebuild);
                let gate = Arc::clone(&gate);
                std::thread::spawn(move || {
                    gate.wait();
                    rebuild.lock().unwrap().finish()
                })
            };
            let caller = {
                let rebuild = Arc::clone(&rebuild);
                let gate = Arc::clone(&gate);
                std::thread::spawn(move || {
                    gate.wait();
                    rebuild
                        .lock()
                        .unwrap()
                        .enqueue(Some(open(SurfaceMode::TrayPanel)))
                })
            };

            let replayed = worker.join().unwrap().is_some();
            let spawned = caller.join().unwrap();
            assert!(
                replayed ^ spawned,
                "the request must be claimed by exactly one side (replayed={replayed}, spawned={spawned})"
            );
            let state = rebuild.lock().unwrap();
            assert!(
                state.in_flight || state.replay.is_none(),
                "a replay must never be queued with no rebuild in flight"
            );
        }
    }
}
