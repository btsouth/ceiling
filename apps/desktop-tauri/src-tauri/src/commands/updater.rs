//! Updater lifecycle commands: check, download, apply, dismiss, and release-page linking.
//!
//! State transitions are mirrored through [`events::emit_update_state_changed`]
//! so the frontend can react without polling.

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::Manager;

use super::open_url_in_browser;
use crate::events;
use crate::state::{AppState, UpdateState, UpdateStatePayload};
use codexbar::updater::{UpdateCheckError, UpdateInfo};

#[tauri::command]
pub fn get_update_state(state: tauri::State<'_, Mutex<AppState>>) -> UpdateStatePayload {
    state
        .lock()
        .map(|guard| guard.update_payload())
        .unwrap_or_else(|_| UpdateState::default().to_payload())
}

#[tauri::command]
pub async fn check_for_updates(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<UpdateStatePayload, String> {
    // Skip only in-flight work. Ready still re-checks so About/tray are not a
    // silent no-op; a "still latest" result restores Ready and the installer.
    {
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        if should_skip_update_check(&guard.update_state) {
            return Ok(guard.update_payload());
        }
        let staged = matches!(guard.update_state, UpdateState::Ready)
            .then(|| (guard.update_info.clone(), guard.installer_path.clone()));
        guard.update_state = UpdateState::Checking;
        if staged.is_none() {
            guard.update_info = None;
            guard.installer_path = None;
        }
    }

    let checking_payload = {
        let guard = state.lock().map_err(|e| e.to_string())?;
        guard.update_payload()
    };
    events::emit_update_state_changed(&app, &checking_payload);

    let settings = codexbar::settings::Settings::load();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        codexbar::updater::check_for_updates_with_channel(settings.update_channel),
    )
    .await;

    let payload = {
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        // SBS-962: a download that claimed the slot while we were on the
        // network must keep its own UpdateInfo. Applying here would pair a
        // different release's digest with the file still being written.
        if commit_check_result(&mut guard, result) {
            guard.last_update_check_ms = Some(chrono::Utc::now().timestamp_millis());
        }
        guard.update_payload()
    };
    events::emit_update_state_changed(&app, &payload);

    Ok(payload)
}

fn should_skip_update_check(state: &UpdateState) -> bool {
    matches!(state, UpdateState::Checking | UpdateState::Downloading(_))
}

struct AppliedCheck {
    state: UpdateState,
    info: Option<UpdateInfo>,
    installer_path: Option<PathBuf>,
}

fn apply_check_result(
    result: Result<Result<Option<UpdateInfo>, UpdateCheckError>, tokio::time::error::Elapsed>,
    staged: (Option<UpdateInfo>, Option<PathBuf>),
) -> AppliedCheck {
    let (staged_info, staged_path) = staged;
    let keep_ready = staged_path.is_some();
    match &result {
        Ok(Ok(None)) if keep_ready => AppliedCheck {
            state: UpdateState::Ready,
            info: staged_info,
            installer_path: staged_path,
        },
        Ok(Ok(Some(info)))
            if keep_ready
                && staged_info
                    .as_ref()
                    .is_some_and(|old| old.version == info.version) =>
        {
            AppliedCheck {
                state: UpdateState::Ready,
                info: staged_info,
                installer_path: staged_path,
            }
        }
        Ok(Err(_)) | Err(_) if keep_ready => AppliedCheck {
            state: UpdateState::Ready,
            info: staged_info,
            installer_path: staged_path,
        },
        _ => {
            let (state, info) = state_from_check_result(result);
            AppliedCheck {
                state,
                info,
                installer_path: None,
            }
        }
    }
}

/// Apply a finished check only while this check still owns the slot.
///
/// Returns whether the check result was committed. A concurrent download
/// (SBS-962) or dismiss leaves `update_state` as something other than
/// Checking; overwriting that would pair a different release's digest with
/// an in-flight or staged installer.
fn commit_check_result(
    guard: &mut AppState,
    result: Result<Result<Option<UpdateInfo>, UpdateCheckError>, tokio::time::error::Elapsed>,
) -> bool {
    if !matches!(guard.update_state, UpdateState::Checking) {
        return false;
    }
    let staged = (guard.update_info.clone(), guard.installer_path.clone());
    let applied = apply_check_result(result, staged);
    guard.update_state = applied.state;
    guard.update_info = applied.info;
    // Ready -> Available (a newer release superseded the staged one): the old
    // installer will never be applied, so do not leave it in the staging dir.
    if guard.installer_path != applied.installer_path {
        discard_staged_installer(guard.installer_path.take());
    }
    guard.installer_path = applied.installer_path;
    true
}

/// Delete a staged installer nothing will apply any more.
fn discard_staged_installer(path: Option<PathBuf>) {
    if let Some(path) = path
        && let Err(error) = std::fs::remove_file(&path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "could not remove an abandoned staged installer");
    }
}

/// Map a completed check onto Idle / Available / Error.
///
/// Idle is only a successful "no newer release". Timeout, transport, HTTP,
/// and parse failures are Error so About cannot say the user is current.
fn state_from_check_result(
    result: Result<Result<Option<UpdateInfo>, UpdateCheckError>, tokio::time::error::Elapsed>,
) -> (UpdateState, Option<UpdateInfo>) {
    match result {
        Ok(Ok(Some(info))) => (UpdateState::Available(info.version.clone()), Some(info)),
        Ok(Ok(None)) => (UpdateState::Idle, None),
        Ok(Err(error)) => (UpdateState::Error(error.user_message()), None),
        Err(_) => (
            UpdateState::Error(codexbar::locale::get_text(
                codexbar::locale::current_language(),
                codexbar::locale::LocaleKey::UpdateErrorTimedOut,
            )),
            None,
        ),
    }
}

#[tauri::command]
pub async fn download_update(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<UpdateStatePayload, String> {
    let (info, initial_payload) = match begin_download(&state)? {
        DownloadStart::Ready { info, payload } => (info, payload),
        DownloadStart::AlreadyDownloading(payload) => return Ok(payload),
    };
    events::emit_update_state_changed(&app, &initial_payload);
    spawn_download_task(app.clone(), info);

    Ok(initial_payload)
}

#[derive(Debug)]
enum DownloadStart {
    Ready {
        info: UpdateInfo,
        payload: UpdateStatePayload,
    },
    AlreadyDownloading(UpdateStatePayload),
}

/// Read `update_info` and enter Downloading under one lock. SBS-962: the
/// previous split let a check land in the gap, clear `update_info`, then
/// get stamped over by Downloading and finish by pairing a different
/// release's digest with this download.
fn begin_download(state: &Mutex<AppState>) -> Result<DownloadStart, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    if matches!(
        guard.update_state,
        UpdateState::Available(_) | UpdateState::Error(_)
    ) && guard
        .update_info
        .as_ref()
        .is_some_and(|info| !info.supports_auto_download())
    {
        return Err(
            "This update does not support automatic download. Open the release page instead."
                .to_string(),
        );
    }
    claim_download_locked(&mut guard)
}

fn claim_download_locked(guard: &mut AppState) -> Result<DownloadStart, String> {
    match &guard.update_state {
        UpdateState::Available(_) | UpdateState::Error(_) => {}
        UpdateState::Downloading(_) => {
            return Ok(DownloadStart::AlreadyDownloading(guard.update_payload()));
        }
        _ => return Err("No update available to download".to_string()),
    }
    let info = guard
        .update_info
        .clone()
        .ok_or_else(|| "No update information available".to_string())?;
    guard.update_state = UpdateState::Downloading(0.0);
    Ok(DownloadStart::Ready {
        payload: guard.update_payload(),
        info,
    })
}

fn spawn_download_task(app_handle: tauri::AppHandle, info: UpdateInfo) {
    tokio::spawn(async move {
        let (tx, rx) = tokio::sync::watch::channel(codexbar::updater::UpdateState::Available);
        let progress_handle = spawn_download_progress_task(app_handle.clone(), rx);
        let final_payload = run_download_task(app_handle.clone(), info, tx).await;

        events::emit_update_state_changed(&app_handle, &final_payload);
        progress_handle.abort();
    });
}

fn spawn_download_progress_task(
    app: tauri::AppHandle,
    mut rx: tokio::sync::watch::Receiver<codexbar::updater::UpdateState>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let backend_state = rx.borrow().clone();
            if let codexbar::updater::UpdateState::Downloading(progress) = backend_state {
                emit_download_progress(&app, progress);
            }
        }
    })
}

fn emit_download_progress(app: &tauri::AppHandle, progress: f32) {
    let st = app.state::<Mutex<AppState>>();
    let payload = {
        let mut guard = st.lock().unwrap_or_else(|e| e.into_inner());
        if !record_download_progress(&mut guard, progress) {
            return;
        }
        guard.update_payload()
    };
    events::emit_update_state_changed(app, &payload);
}

/// Report progress only while this download still owns the slot.
///
/// The Available banner offers Download and Dismiss together, so both can be
/// clicked. Dismiss sets Idle, but every later chunk used to write Downloading
/// back unconditionally, which re-armed the slot and let `record_download_finish`
/// pass its own guard and resurrect Install & Restart after the user had
/// dismissed the update (SBS-962).
fn record_download_progress(guard: &mut AppState, progress: f32) -> bool {
    if !matches!(guard.update_state, UpdateState::Downloading(_)) {
        return false;
    }
    guard.update_state = UpdateState::Downloading(progress);
    true
}

async fn run_download_task(
    app: tauri::AppHandle,
    info: UpdateInfo,
    tx: tokio::sync::watch::Sender<codexbar::updater::UpdateState>,
) -> UpdateStatePayload {
    let staged_info = info.clone();
    let download_handle =
        tokio::spawn(async move { codexbar::updater::download_update(&info, tx).await });

    match download_handle.await {
        Ok(Ok(path)) => finish_download(&app, UpdateState::Ready, Some(path), Some(staged_info)),
        Ok(Err(error)) => finish_download(&app, UpdateState::Error(error), None, None),
        Err(join_err) => finish_download(
            &app,
            UpdateState::Error(format!("Download task failed: {join_err}")),
            None,
            None,
        ),
    }
}

fn finish_download(
    app: &tauri::AppHandle,
    update_state: UpdateState,
    installer_path: Option<std::path::PathBuf>,
    staged_info: Option<UpdateInfo>,
) -> UpdateStatePayload {
    let st = app.state::<Mutex<AppState>>();
    let mut guard = st.lock().unwrap_or_else(|e| e.into_inner());
    record_download_finish(&mut guard, update_state, installer_path, staged_info);
    guard.update_payload()
}

/// Pair the installer with the UpdateInfo the download started from so
/// apply-time SHA256 is the digest of the file on disk, not of a later check.
///
/// Apply Ready/Error only while this download still owns the slot. A dismiss
/// (or any later writer) that ran while spawn_download_task was finishing
/// must not be replaced by Ready plus the start digest.
fn record_download_finish(
    guard: &mut AppState,
    update_state: UpdateState,
    installer_path: Option<PathBuf>,
    staged_info: Option<UpdateInfo>,
) {
    if !matches!(guard.update_state, UpdateState::Downloading(_)) {
        // Somebody else owns the slot now - a dismiss, most likely. Nothing
        // will ever apply this file, and the staging directory would keep one
        // installer per dismissed download, so remove it on the way out.
        discard_staged_installer(installer_path);
        return;
    }
    guard.update_state = update_state;
    guard.installer_path = installer_path;
    if let Some(info) = staged_info {
        guard.update_info = Some(info);
    }
}

#[tauri::command]
pub fn apply_update(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<(), String> {
    apply_ready_update(&app, &state)
}

pub(crate) fn apply_ready_update(
    app: &tauri::AppHandle,
    state: &Mutex<AppState>,
) -> Result<(), String> {
    apply_ready_installer(state).inspect_err(|_| {
        if let Ok(guard) = state.lock() {
            events::emit_update_state_changed(app, &guard.update_payload());
        }
    })
}

fn apply_ready_installer(state: &Mutex<AppState>) -> Result<(), String> {
    let result = (|| {
        let (path, expected_sha256) = {
            let guard = state.lock().map_err(|e| e.to_string())?;
            staged_apply_target(&guard)?
        };
        codexbar::updater::verify_installer_hash(&path, &expected_sha256)?;
        codexbar::updater::apply_update(&path)
    })();

    if let Err(error) = &result {
        let _ = record_apply_failure(state, error);
    }
    result
}

fn staged_apply_target(guard: &AppState) -> Result<(PathBuf, String), String> {
    let path = guard
        .installer_path
        .clone()
        .ok_or_else(|| "No downloaded update available to apply".to_string())?;
    let expected_sha256 = guard
        .update_info
        .as_ref()
        .and_then(|info| info.expected_sha256.clone())
        .ok_or_else(|| "Missing SHA256 digest for downloaded update".to_string())?;
    Ok((path, expected_sha256))
}

fn record_apply_failure(
    state: &Mutex<AppState>,
    error: &str,
) -> Result<UpdateStatePayload, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.update_state = UpdateState::Error(error.to_string());
    guard.installer_path = None;
    Ok(guard.update_payload())
}

#[tauri::command]
pub fn dismiss_update(
    app: tauri::AppHandle,
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<UpdateStatePayload, String> {
    let payload = {
        let mut guard = state.lock().map_err(|e| e.to_string())?;
        guard.update_state = UpdateState::Idle;
        guard.update_info = None;
        discard_staged_installer(guard.installer_path.take());
        guard.update_payload()
    };
    events::emit_update_state_changed(&app, &payload);
    Ok(payload)
}

#[tauri::command]
pub fn open_release_page(state: tauri::State<'_, Mutex<AppState>>) -> Result<(), String> {
    let url = {
        let guard = state.lock().map_err(|e| e.to_string())?;
        guard
            .update_info
            .as_ref()
            .map(|info| info.release_url.clone())
            .ok_or("No update information available")?
    };
    open_url_in_browser(&url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, UpdateState};
    use codexbar::updater::{UpdateCheckError, UpdateDelivery, UpdateInfo};

    fn sample_info(version: &str) -> UpdateInfo {
        sample_info_with_hash(version, &"a".repeat(64))
    }

    fn sample_info_with_hash(version: &str, sha256: &str) -> UpdateInfo {
        UpdateInfo {
            version: version.to_string(),
            download_url: "https://example.com/Ceiling-Setup.exe".to_string(),
            expected_sha256: Some(sha256.to_string()),
            release_url: "https://example.com/release".to_string(),
            release_notes: String::new(),
            delivery: UpdateDelivery::Installer,
        }
    }

    /// SBS-931: GitHub/network/parse failures must not become Idle.
    #[test]
    fn failed_github_check_is_error_not_idle() {
        for error in [
            UpdateCheckError::Network,
            UpdateCheckError::Http { status: 403 },
            UpdateCheckError::Parse,
            UpdateCheckError::Client,
        ] {
            let (state, info) = state_from_check_result(Ok(Err(error.clone())));
            match state {
                UpdateState::Error(message) => {
                    assert_eq!(message, error.user_message());
                    assert!(
                        !message.to_ascii_lowercase().contains("up to date"),
                        "{message}"
                    );
                }
                other => panic!("expected Error, got {other:?} for {error:?}"),
            }
            assert!(info.is_none());
        }
    }

    /// Only a successful "no newer release" is Idle.
    #[test]
    fn successful_current_release_is_idle() {
        let (state, info) = state_from_check_result(Ok(Ok(None)));
        assert_eq!(state, UpdateState::Idle);
        assert!(info.is_none());
    }

    #[test]
    fn successful_newer_release_is_available() {
        let info = sample_info("v99.0.0");
        let (state, stored) = state_from_check_result(Ok(Ok(Some(info))));
        assert_eq!(state, UpdateState::Available("v99.0.0".to_string()));
        assert_eq!(
            stored.as_ref().map(|item| item.version.as_str()),
            Some("v99.0.0")
        );
    }

    #[test]
    fn ready_check_is_not_skipped() {
        assert!(!should_skip_update_check(&UpdateState::Ready));
        assert!(should_skip_update_check(&UpdateState::Checking));
        assert!(should_skip_update_check(&UpdateState::Downloading(0.5)));
        assert!(!should_skip_update_check(&UpdateState::Idle));
        assert!(!should_skip_update_check(&UpdateState::Error(
            "GitHub did not return a release.".to_string()
        )));
        assert!(!should_skip_update_check(&UpdateState::Available(
            "v99.0.0".to_string()
        )));
    }

    #[test]
    fn a_ready_check_that_finds_no_newer_release_keeps_the_installer() {
        let staged_info = sample_info("v99.0.0");
        let staged_path = PathBuf::from("Ceiling-99.0.0-Setup.exe");
        let applied =
            apply_check_result(Ok(Ok(None)), (Some(staged_info), Some(staged_path.clone())));
        assert_eq!(applied.state, UpdateState::Ready);
        assert_eq!(
            applied.info.as_ref().map(|item| item.version.as_str()),
            Some("v99.0.0")
        );
        assert_eq!(
            applied.installer_path.as_deref(),
            Some(staged_path.as_path())
        );
    }

    /// SBS-962: claiming a download must enter Downloading in the same
    /// critical section that reads update_info, so a check cannot see Available.
    #[test]
    fn claiming_a_download_sets_downloading_under_the_same_lock() {
        let mut state = AppState::new();
        state.update_state = UpdateState::Available("v1.5.35".to_string());
        state.update_info = Some(sample_info("v1.5.35"));

        match claim_download_locked(&mut state).expect("claim") {
            DownloadStart::Ready { info, payload } => {
                assert_eq!(info.version, "v1.5.35");
                assert_eq!(payload.status, "downloading");
            }
            other => panic!("expected Ready, got {other:?}"),
        }
        assert!(matches!(state.update_state, UpdateState::Downloading(_)));
        assert!(should_skip_update_check(&state.update_state));
        assert_eq!(
            state.update_info.as_ref().map(|item| item.version.as_str()),
            Some("v1.5.35")
        );
    }

    #[test]
    fn claiming_a_download_does_not_stamp_over_an_in_flight_check() {
        let mut state = AppState::new();
        state.update_state = UpdateState::Checking;
        state.update_info = Some(sample_info("v1.5.35"));

        let error = claim_download_locked(&mut state).expect_err("checking");
        assert!(error.contains("No update available to download"), "{error}");
        assert_eq!(state.update_state, UpdateState::Checking);
    }

    #[test]
    fn claiming_a_download_while_already_downloading_is_a_no_op() {
        let mut state = AppState::new();
        state.update_state = UpdateState::Downloading(0.4);
        state.update_info = Some(sample_info("v1.5.35"));

        match claim_download_locked(&mut state).expect("claim") {
            DownloadStart::AlreadyDownloading(payload) => {
                assert_eq!(payload.status, "downloading");
            }
            other => panic!("expected AlreadyDownloading, got {other:?}"),
        }
        assert!(matches!(
            state.update_state,
            UpdateState::Downloading(progress) if (progress - 0.4).abs() < f32::EPSILON
        ));
    }

    /// SBS-962: a check that lost the slot must not replace the digest the
    /// in-flight download will be verified against.
    #[test]
    fn check_result_does_not_replace_an_in_flight_download() {
        let mut state = AppState::new();
        state.update_state = UpdateState::Downloading(0.2);
        state.update_info = Some(sample_info_with_hash("v1.5.35", &"a".repeat(64)));

        let newer = sample_info_with_hash("v1.5.36", &"b".repeat(64));
        assert!(!commit_check_result(&mut state, Ok(Ok(Some(newer)))));
        assert!(matches!(state.update_state, UpdateState::Downloading(_)));
        assert_eq!(
            state.update_info.as_ref().map(|item| item.version.as_str()),
            Some("v1.5.35")
        );
        assert_eq!(
            state
                .update_info
                .as_ref()
                .and_then(|item| item.expected_sha256.clone()),
            Some("a".repeat(64))
        );
    }

    #[test]
    fn superseded_staged_installer_is_deleted_on_ready_to_available() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("Ceiling-1.5.35-Setup.exe");
        std::fs::write(&path, b"installer bytes").expect("write installer");

        let mut state = AppState::new();
        state.update_state = UpdateState::Checking;
        state.update_info = Some(sample_info("v1.5.35"));
        state.installer_path = Some(path.clone());

        assert!(commit_check_result(
            &mut state,
            Ok(Ok(Some(sample_info("v1.5.36"))))
        ));
        assert_eq!(
            state.update_state,
            UpdateState::Available("v1.5.36".to_string())
        );
        assert!(state.installer_path.is_none());
        assert!(!path.exists(), "the superseded installer must be removed");
    }

    #[test]
    fn same_release_recheck_keeps_the_staged_installer() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("Ceiling-1.5.35-Setup.exe");
        std::fs::write(&path, b"installer bytes").expect("write installer");

        let mut state = AppState::new();
        state.update_state = UpdateState::Checking;
        state.update_info = Some(sample_info("v1.5.35"));
        state.installer_path = Some(path.clone());

        assert!(commit_check_result(&mut state, Ok(Ok(None))));
        assert_eq!(state.update_state, UpdateState::Ready);
        assert_eq!(state.installer_path.as_deref(), Some(path.as_path()));
        assert!(path.exists());
    }

    #[test]
    fn check_result_still_commits_while_the_check_owns_the_slot() {
        let mut state = AppState::new();
        state.update_state = UpdateState::Checking;
        let newer = sample_info("v1.5.36");
        assert!(commit_check_result(&mut state, Ok(Ok(Some(newer)))));
        assert_eq!(
            state.update_state,
            UpdateState::Available("v1.5.36".to_string())
        );
        assert_eq!(
            state.update_info.as_ref().map(|item| item.version.as_str()),
            Some("v1.5.36")
        );
    }

    /// SBS-962: finish stores the UpdateInfo the download started from, even
    /// if a later check wrote a different release into update_info.
    #[test]
    fn finish_pairs_the_started_release_with_the_staged_installer() {
        let mut state = AppState::new();
        state.update_state = UpdateState::Downloading(1.0);
        state.update_info = Some(sample_info_with_hash("v1.5.36", &"b".repeat(64)));

        let started = sample_info_with_hash("v1.5.35", &"a".repeat(64));
        let path = PathBuf::from("Ceiling-1.5.35-Setup.exe");
        record_download_finish(
            &mut state,
            UpdateState::Ready,
            Some(path.clone()),
            Some(started),
        );

        assert_eq!(state.update_state, UpdateState::Ready);
        assert_eq!(
            state.update_info.as_ref().map(|item| item.version.as_str()),
            Some("v1.5.35")
        );
        let (staged_path, digest) = staged_apply_target(&state).expect("staged");
        assert_eq!(staged_path, path);
        assert_eq!(digest, "a".repeat(64));
    }

    /// A dismiss that wins the slot while the download task is still finishing
    /// must stay Idle; Ready plus the start digest would make it applyable.
    #[test]
    fn finish_does_not_replace_a_later_idle_slot() {
        let mut state = AppState::new();
        state.update_state = UpdateState::Idle;
        state.update_info = None;
        state.installer_path = None;

        record_download_finish(
            &mut state,
            UpdateState::Ready,
            Some(PathBuf::from("Ceiling-1.5.35-Setup.exe")),
            Some(sample_info("v1.5.35")),
        );

        assert_eq!(state.update_state, UpdateState::Idle);
        assert!(state.installer_path.is_none());
        assert!(state.update_info.is_none());
    }

    /// SBS-962: the finish guard is only as good as what may re-arm the slot.
    /// A progress chunk arriving after Dismiss used to write Downloading back,
    /// which let the finish store Ready and bring Install & Restart back.
    #[test]
    fn progress_after_a_dismiss_does_not_re_arm_the_slot() {
        let mut state = AppState::new();
        state.update_state = UpdateState::Idle;

        assert!(
            !record_download_progress(&mut state, 0.5),
            "a dismissed download must not report progress"
        );
        assert_eq!(state.update_state, UpdateState::Idle);

        // The finish that follows must therefore still be discarded.
        record_download_finish(
            &mut state,
            UpdateState::Ready,
            None,
            Some(sample_info("v1.5.35")),
        );
        assert_eq!(state.update_state, UpdateState::Idle);
        assert!(state.update_info.is_none());
    }

    /// Progress still lands while the download does own the slot.
    #[test]
    fn progress_is_recorded_while_the_download_owns_the_slot() {
        let mut state = AppState::new();
        state.update_state = UpdateState::Downloading(0.0);

        assert!(record_download_progress(&mut state, 0.25));
        assert_eq!(state.update_state, UpdateState::Downloading(0.25));
    }

    /// SBS-962: a download that lands after the slot was given up has nothing
    /// that will ever apply it, so its file must not be left in staging.
    #[test]
    fn a_discarded_finish_removes_the_downloaded_installer() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("Ceiling-1.5.35-Setup.exe");
        std::fs::write(&path, b"installer bytes").expect("write installer");

        let mut state = AppState::new();
        state.update_state = UpdateState::Idle;

        record_download_finish(
            &mut state,
            UpdateState::Ready,
            Some(path.clone()),
            Some(sample_info("v1.5.35")),
        );

        assert_eq!(state.update_state, UpdateState::Idle);
        assert!(
            !path.exists(),
            "a discarded download must not leave an installer behind"
        );
    }

    /// SBS-962: apply hashes the staged file against the digest captured at
    /// download start. A swapped later-check digest must not be how a good
    /// download is verified.
    #[test]
    fn apply_uses_the_digest_paired_with_the_staged_file() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("Ceiling-1.5.35-Setup.exe");
        std::fs::write(&path, b"installer bytes").expect("write installer");
        // sha256 of the bytes written on the previous line.
        let matching =
            "e34210a6de4f653edf588301431c3d69a633638cbf587345cc50a7fed9f38f4c".to_string();
        let other = "0".repeat(64);

        let mut state = AppState::new();
        state.update_state = UpdateState::Downloading(1.0);
        state.update_info = Some(sample_info_with_hash("v1.5.36", &other));

        record_download_finish(
            &mut state,
            UpdateState::Ready,
            Some(path.clone()),
            Some(sample_info_with_hash("v1.5.35", &matching)),
        );

        let (staged_path, digest) = staged_apply_target(&state).expect("staged");
        assert_eq!(staged_path, path);
        assert_eq!(digest, matching);
        codexbar::updater::verify_installer_hash(&path, &digest).expect("matching digest");
    }

    #[test]
    fn apply_hash_mismatch_clears_the_staged_installer_without_running_it() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("Ceiling-1.5.35-Setup.exe");
        std::fs::write(&path, b"installer bytes").expect("write installer");

        let state = Mutex::new(AppState::new());
        {
            let mut guard = state.lock().expect("lock");
            guard.update_state = UpdateState::Ready;
            guard.installer_path = Some(path.clone());
            guard.update_info = Some(sample_info_with_hash("v1.5.36", &"0".repeat(64)));
        }

        let error = apply_ready_installer(&state).unwrap_err();
        assert!(
            error.contains("SHA256 mismatch"),
            "expected hash mismatch, got {error}"
        );
        assert!(path.exists(), "a hash mismatch must not delete the file");
        let guard = state.lock().expect("lock");
        assert!(guard.installer_path.is_none());
        match &guard.update_state {
            UpdateState::Error(message) => assert_eq!(message, &error),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn apply_time_signature_reject_does_not_leave_installer_ready() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("Ceiling-99.0.0-Setup.exe");
        std::fs::write(&path, b"unsigned installer").expect("write installer");

        let state = Mutex::new(AppState::new());
        {
            let mut guard = state.lock().expect("lock");
            guard.update_state = UpdateState::Ready;
            guard.installer_path = Some(path.clone());
            guard.update_info = Some(UpdateInfo {
                version: "v99.0.0".to_string(),
                download_url: "https://example.com/Ceiling-99.0.0-Setup.exe".to_string(),
                // sha256("unsigned installer")
                expected_sha256: Some(
                    "d27a9d9762922fc761ed69b30eeaf45b03b16931d0f2b2c21c5f02c3dbb1690b".to_string(),
                ),
                release_url: "https://example.com/release".to_string(),
                release_notes: String::new(),
                delivery: UpdateDelivery::Installer,
            });
        }

        let error = apply_ready_installer(&state).unwrap_err();
        assert!(
            error.contains("unsigned")
                || error.contains("invalid or untrusted")
                || error.contains("downloaded file was removed"),
            "{error}"
        );
        assert!(!path.exists(), "rejected installer must be deleted");

        let guard = state.lock().expect("lock");
        match &guard.update_state {
            UpdateState::Error(message) => {
                assert_eq!(message, &error);
            }
            other => panic!("expected Error, got {other:?}"),
        }
        assert!(
            guard.installer_path.is_none(),
            "a deleted installer must not remain applyable"
        );
        drop(guard);

        let retry_error = apply_ready_installer(&state).unwrap_err();
        assert!(
            retry_error.contains("No downloaded update available to apply"),
            "{retry_error}"
        );
        assert!(matches!(
            state.lock().expect("lock").update_state,
            UpdateState::Error(_)
        ));
    }
}
