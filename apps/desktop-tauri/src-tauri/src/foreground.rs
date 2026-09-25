//! Local foreground-app → provider mapping for floatbar selection modes.
//!
//! Detection stays on this machine. It never calls a provider API. A missing
//! match keeps the last known provider so an unrelated app does not blank
//! the bar.

mod matching;

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub use matching::match_foreground_provider;

pub const FOREGROUND_PROVIDER_CHANGED_EVENT: &str = "foreground-provider-changed";

const POLL_INTERVAL: Duration = Duration::from_millis(750);

static WATCH_GENERATION: AtomicU64 = AtomicU64::new(0);
static LAST_ACTIVE: Mutex<Option<String>> = Mutex::new(None);
static LAST_EMITTED: Mutex<Option<ForegroundProviderSnapshot>> = Mutex::new(None);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForegroundProviderSnapshot {
    pub provider_id: Option<String>,
    pub last_active_provider_id: Option<String>,
}

pub fn last_active_provider() -> Option<String> {
    LAST_ACTIVE.lock().ok().and_then(|guard| guard.clone())
}

pub fn should_watch_foreground(settings: &codexbar::settings::Settings) -> bool {
    settings.float_bar_enabled
        && settings.float_bar_foreground_detection
        && matches!(
            settings.float_bar_selection_mode.as_str(),
            "active" | "activePlusCritical"
        )
}

pub fn apply_watch(app: &AppHandle, settings: &codexbar::settings::Settings) {
    let generation = WATCH_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    if !should_watch_foreground(settings) {
        return;
    }
    // A fresh watcher starts with no last-emitted snapshot, so its first tick
    // always reaches the webview even when focus has not moved since the
    // previous watcher stopped (hide, then show again).
    if let Ok(mut guard) = LAST_EMITTED.lock() {
        *guard = None;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        // `Burst` (the default) replays every deadline missed while the
        // machine was suspended, so a resume after hours of modern standby
        // would run thousands of Win32 reads back to back. `Delay` resets the
        // cadence from the moment the loop actually runs again.
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if WATCH_GENERATION.load(Ordering::SeqCst) != generation {
                break;
            }
            let snapshot = tokio::task::spawn_blocking(snapshot_now)
                .await
                .unwrap_or_else(|_| snapshot_now());
            emit_if_changed(&app, &snapshot);
        }
    });
}

/// Emit `foreground-provider-changed` only when the snapshot differs from the
/// last one sent.
///
/// The loop runs 1.33 times a second; emitting unconditionally floods the
/// Tauri bridge and wakes the floating-bar webview forever on a surface whose
/// whole job is to sit idle showing a static number. The frontend listener
/// already discards identical payloads, so this only removes overhead.
fn emit_if_changed(app: &AppHandle, snapshot: &ForegroundProviderSnapshot) {
    match LAST_EMITTED.lock() {
        Ok(mut guard) => {
            if record_emit(&mut guard, snapshot) {
                emit_provider_changed(app, snapshot);
            }
        }
        // On poisoning, fall back to the old always-emit behavior rather than
        // going silent.
        Err(_) => emit_provider_changed(app, snapshot),
    }
}

/// Update the last-emitted slot and report whether this snapshot should be
/// sent. Pure so the dedup rule is unit-tested without a Tauri app handle.
fn record_emit(
    slot: &mut Option<ForegroundProviderSnapshot>,
    snapshot: &ForegroundProviderSnapshot,
) -> bool {
    if slot.as_ref() == Some(snapshot) {
        return false;
    }
    *slot = Some(snapshot.clone());
    true
}

#[tauri::command]
pub async fn get_foreground_provider() -> ForegroundProviderSnapshot {
    tokio::task::spawn_blocking(snapshot_now)
        .await
        .unwrap_or_else(|_| snapshot_now())
}

fn emit_provider_changed(app: &AppHandle, snapshot: &ForegroundProviderSnapshot) {
    let _ = app.emit(FOREGROUND_PROVIDER_CHANGED_EVENT, snapshot);
}

fn snapshot_now() -> ForegroundProviderSnapshot {
    let observed = read_foreground();
    let matched = observed
        .as_ref()
        .and_then(|(exe, title)| match_foreground_provider(exe, title).map(str::to_string));
    if let Some(provider_id) = matched.as_ref()
        && let Ok(mut guard) = LAST_ACTIVE.lock()
    {
        *guard = Some(provider_id.clone());
    }
    let last_active = last_active_provider();
    ForegroundProviderSnapshot {
        provider_id: matched,
        last_active_provider_id: last_active,
    }
}

#[cfg(windows)]
fn read_foreground() -> Option<(String, String)> {
    use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };
    use windows::core::PWSTR;

    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return None;
    }

    let mut pid = 0_u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return None;
    }

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let exe = (|| {
        let mut buf = [0u16; MAX_PATH as usize];
        let mut size = buf.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
        }
        .ok()?;
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    })();
    unsafe {
        let _ = CloseHandle(process);
    }
    let exe = exe?;

    let mut title_buf = [0u16; 512];
    let title_len = unsafe { GetWindowTextW(hwnd, &mut title_buf) };
    let title = if title_len > 0 {
        String::from_utf16_lossy(&title_buf[..title_len as usize])
    } else {
        String::new()
    };

    Some((exe, title))
}

#[cfg(not(windows))]
fn read_foreground() -> Option<(String, String)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_watch_only_when_floatbar_uses_an_active_mode() {
        let pinned = codexbar::settings::Settings {
            float_bar_enabled: true,
            float_bar_foreground_detection: true,
            float_bar_selection_mode: "pinned".into(),
            ..codexbar::settings::Settings::default()
        };
        assert!(!should_watch_foreground(&pinned));

        let active = codexbar::settings::Settings {
            float_bar_selection_mode: "active".into(),
            ..pinned.clone()
        };
        assert!(should_watch_foreground(&active));
        assert!(!should_watch_foreground(&codexbar::settings::Settings {
            float_bar_foreground_detection: false,
            ..active.clone()
        }));
        assert!(!should_watch_foreground(&codexbar::settings::Settings {
            float_bar_enabled: false,
            ..active
        }));
    }

    fn snapshot(provider: Option<&str>, last_active: Option<&str>) -> ForegroundProviderSnapshot {
        ForegroundProviderSnapshot {
            provider_id: provider.map(str::to_string),
            last_active_provider_id: last_active.map(str::to_string),
        }
    }

    #[test]
    fn record_emit_suppresses_repeats_and_lets_changes_through() {
        let mut slot = None;

        assert!(record_emit(
            &mut slot,
            &snapshot(Some("codex"), Some("codex"))
        ));
        assert!(!record_emit(
            &mut slot,
            &snapshot(Some("codex"), Some("codex"))
        ));
        assert!(record_emit(
            &mut slot,
            &snapshot(Some("claude"), Some("claude"))
        ));
        // A change in the last-active field alone is still a change.
        assert!(record_emit(&mut slot, &snapshot(None, Some("claude"))));
        assert!(!record_emit(&mut slot, &snapshot(None, Some("claude"))));
    }

    #[test]
    fn record_emit_treats_none_as_a_real_snapshot() {
        let mut slot = None;

        assert!(record_emit(&mut slot, &snapshot(None, None)));
        assert!(!record_emit(&mut slot, &snapshot(None, None)));
    }
}
