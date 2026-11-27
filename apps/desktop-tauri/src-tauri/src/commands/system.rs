use super::*;
use codexbar::login::{LoginOutcome, LoginPhase, LoginResult, run_claude_login, run_codex_login};

const PROVIDER_LOGIN_TIMEOUT_SECS: u64 = 5 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliLoginKind {
    Claude,
    Codex,
}

fn cli_login_kind(id: ProviderId) -> Option<CliLoginKind> {
    match id {
        ProviderId::Claude => Some(CliLoginKind::Claude),
        ProviderId::Codex => Some(CliLoginKind::Codex),
        // Gemini CLI currently exposes sign-in only through its interactive
        // TUI. The existing Gemini credential setup section remains the
        // truthful fallback until it offers a dedicated auth command.
        ProviderId::Gemini => None,
        _ => None,
    }
}

/// Whether `trigger_provider_login` can sign this provider in from the app.
/// Kept next to `cli_login_kind` so the Settings "Switch account" button and
/// the command it calls can never disagree.
pub(super) fn supports_in_app_login(id: ProviderId) -> bool {
    id == ProviderId::Copilot || cli_login_kind(id).is_some()
}

fn login_phase_name(phase: LoginPhase) -> &'static str {
    match phase {
        LoginPhase::Idle => "idle",
        LoginPhase::Requesting => "requesting",
        LoginPhase::WaitingBrowser => "waitingBrowser",
        LoginPhase::Complete => "complete",
    }
}

#[tauri::command]
pub fn get_app_info() -> AppInfoBridge {
    let settings = Settings::load();
    AppInfoBridge {
        name: "Ceiling".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        build_number: option_env!("BUILD_NUMBER").unwrap_or("dev").to_string(),
        update_channel: update_channel_label(settings.update_channel).to_string(),
        tagline: "Keep your AI capacity in view.".to_string(),
    }
}

pub(super) fn open_url_in_browser(url: &str) -> Result<(), String> {
    let url = validate_external_url(url)?;
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new(windows_system_binary("rundll32.exe")?)
            .arg("url.dll,FileProtocolHandler")
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to open URL: {e}"))?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener)
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to open URL: {e}"))?;
    }
    Ok(())
}

pub(crate) fn validate_external_url(url: &str) -> Result<&str, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("URL is empty".to_string());
    }
    if trimmed.len() > 2048 || trimmed.chars().any(char::is_control) {
        return Err("URL is invalid".to_string());
    }
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err("Only http and https URLs can be opened".to_string());
    }
    Ok(trimmed)
}

#[tauri::command]
pub fn open_external_url(url: String) -> Result<(), String> {
    open_url_in_browser(&url)
}

#[cfg(target_os = "windows")]
fn windows_system_binary(name: &str) -> Result<std::path::PathBuf, String> {
    codexbar::host::windows_system_exe(name)
        .ok_or_else(|| format!("Could not find trusted system binary {name}"))
}

// ════════════════════════════════════════════════════════════════════════════════
// PHASE 4 — Provider ordering, cookie source, region, credential detection,
// global shortcut capture, session/environment introspection, quick actions.
// ════════════════════════════════════════════════════════════════════════════════

/// Open a filesystem path in the OS file manager (Finder / Explorer /
/// xdg-open). Non-existent paths are rejected so the UI gets immediate
/// feedback instead of a silent no-op shell launch.
#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("Path is empty".into());
    }
    let pb = std::path::PathBuf::from(trimmed);
    if !pb.is_absolute() {
        return Err("Path must be absolute".into());
    }
    if !pb.exists() {
        return Err(format!("Path not found: {trimmed}"));
    }
    let canonical = pb
        .canonicalize()
        .map_err(|e| format!("Could not resolve path: {e}"))?;
    let allowed_roots = [Settings::settings_path()]
        .into_iter()
        .flatten()
        .filter_map(|path| path.parent().map(std::path::Path::to_path_buf))
        .filter_map(|path| path.canonicalize().ok())
        .collect::<Vec<_>>();
    let allowed_exact = allowed_open_paths()
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .collect::<Vec<_>>();
    if !path_is_allowed(&canonical, &allowed_roots, &allowed_exact) {
        return Err("Path is outside Ceiling's allowed locations".into());
    }
    // When given a file, open its parent directory so the file is highlighted
    // in a useful way across platforms without needing per-OS --select flags.
    let target = if canonical.is_file() {
        canonical
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| canonical.clone())
    } else {
        canonical
    };
    #[cfg(target_os = "windows")]
    let target = windows_shell_path(&target);
    let target_str = target.to_string_lossy().into_owned();

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new(windows_system_binary("explorer.exe")?)
            .arg(&target_str)
            .spawn()
            .map_err(|e| format!("Failed to open path: {e}"))?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(opener)
            .arg(&target_str)
            .spawn()
            .map_err(|e| format!("Failed to open path: {e}"))?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub(super) fn windows_shell_path(path: &std::path::Path) -> std::path::PathBuf {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    const EXTENDED_PREFIX: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    const UNC_PREFIX: &[u16] = &[b'U' as u16, b'N' as u16, b'C' as u16, b'\\' as u16];

    let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    let Some(remainder) = wide.strip_prefix(EXTENDED_PREFIX) else {
        return path.to_path_buf();
    };
    let normalized = if remainder.len() >= UNC_PREFIX.len()
        && remainder[..UNC_PREFIX.len()]
            .iter()
            .zip(UNC_PREFIX)
            .all(|(actual, expected)| {
                actual == expected
                    || ((*actual >= b'a' as u16 && *actual <= b'z' as u16)
                        && actual - (b'a' - b'A') as u16 == *expected)
            }) {
        [
            vec![b'\\' as u16, b'\\' as u16],
            remainder[UNC_PREFIX.len()..].to_vec(),
        ]
        .concat()
    } else {
        remainder.to_vec()
    };
    std::ffi::OsString::from_wide(&normalized).into()
}

pub(super) fn path_is_allowed(
    path: &std::path::Path,
    allowed_roots: &[std::path::PathBuf],
    allowed_exact: &[std::path::PathBuf],
) -> bool {
    allowed_exact.iter().any(|allowed| path == allowed)
        || allowed_roots.iter().any(|root| path.starts_with(root))
}

// ── Session / environment ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkAreaRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[tauri::command]
pub fn is_remote_session() -> Result<bool, String> {
    Ok(codexbar::host::session::is_ssh_session() || codexbar::host::session::is_remote_session())
}

#[tauri::command]
pub fn get_launch_block_reason() -> Result<Option<String>, String> {
    Ok(codexbar::host::session::current_launch_block_reason().map(|s| s.to_string()))
}

#[tauri::command]
pub fn get_work_area_rect(app: tauri::AppHandle) -> Result<WorkAreaRect, String> {
    use tauri::Manager;

    // Prefer the OS-native probe on Windows because it reliably excludes the
    // taskbar; Tauri's monitor API forwards to the same APIs but we keep the
    // direct path to preserve parity with the egui build.
    if let Some(area) = codexbar::host::session::primary_work_area_pixels() {
        return Ok(WorkAreaRect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: area.height,
        });
    }

    // Cross-platform fallback (macOS: NSScreen.visibleFrame; Linux: GTK /
    // X11 work-area) via Tauri's monitor wrapper. Require a window so tao's
    // screen backend is initialised.
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "Main window is not available".to_string())?;

    let monitor = window
        .current_monitor()
        .map_err(|e| e.to_string())?
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| "No monitor detected".to_string())?;

    let work_area = monitor.work_area();
    Ok(WorkAreaRect {
        x: work_area.position.x,
        y: work_area.position.y,
        width: work_area.size.width as i32,
        height: work_area.size.height as i32,
    })
}

// ── Misc UX ────────────────────────────────────────────────────────────

#[tauri::command]
pub fn play_notification_sound() -> Result<(), String> {
    // Use the shared sound helper, honouring the user's `sound_enabled` flag.
    let settings = Settings::load();
    codexbar::sound::play_alert(codexbar::sound::AlertSound::Success, &settings);
    Ok(())
}

/// Show a real Windows toast on demand so the user can confirm notifications
/// are delivered on their machine. Runs the full toast pipeline synchronously
/// and surfaces any failure back to the Settings button.
#[tauri::command]
pub fn send_test_notification() -> Result<(), String> {
    codexbar::notifications::send_test_notification()
}

/// Reposition the flyout window so its bottom-right corner stays anchored to
/// the system-tray area. Called from the frontend after dynamic resize.
///
/// Retargeted from `main` to the dedicated `flyout` window — the flyout is no
/// longer a state of `main`'s surface-mode machine, so `reanchor_tray_panel`
/// (still exported under its historical name — the frontend command name is
/// unchanged) now anchors the flyout window directly. The anchor math itself
/// lives in `shell::flyout_window::reanchor`, which this delegates to.
#[tauri::command]
pub fn reanchor_tray_panel(app: tauri::AppHandle) -> Result<(), String> {
    crate::shell::flyout_window::reanchor(&app)
}

#[tauri::command]
pub fn quit_app(app: tauri::AppHandle) {
    let settings = Settings::load();
    if settings.install_updates_on_quit
        && let Some(state) = app.try_state::<std::sync::Mutex<crate::state::AppState>>()
        && let Err(error) = super::updater::apply_ready_update(&app, &state)
    {
        tracing::debug!("install-on-quit skipped: {error}");
    }
    app.exit(0);
}

fn dashboard_url_for_provider(provider_id: &str) -> Option<String> {
    if provider_id == ProviderId::MiniMax.cli_name() {
        let settings = Settings::load();
        return Some(
            codexbar::providers::MiniMaxProvider::dashboard_url_for_region(Some(
                settings.api_region(ProviderId::MiniMax),
            )),
        );
    }

    if let Some(url) = codexbar::settings::get_api_key_providers()
        .into_iter()
        .find(|p| p.id.cli_name() == provider_id)
        .and_then(|p| p.dashboard_url.map(|s| s.to_string()))
    {
        return Some(url);
    }

    let id = ProviderId::from_cli_name(provider_id)?;
    let provider = instantiate_provider(id);
    provider.metadata().dashboard_url.map(|s| s.to_string())
}

fn status_page_url_for_provider(provider_id: &str) -> Option<String> {
    let id = ProviderId::from_cli_name(provider_id)?;
    let provider = instantiate_provider(id);
    provider.metadata().status_page_url.map(|s| s.to_string())
}

#[tauri::command]
pub fn open_provider_dashboard(provider_id: String) -> Result<(), String> {
    let provider_id = canonical_provider_arg(&provider_id)?;
    let url = dashboard_url_for_provider(&provider_id)
        .ok_or_else(|| format!("No dashboard URL registered for provider '{provider_id}'"))?;
    open_url_in_browser(&url)
}

#[tauri::command]
pub fn open_provider_status_page(provider_id: String) -> Result<(), String> {
    let provider_id = canonical_provider_arg(&provider_id)?;
    let url = status_page_url_for_provider(&provider_id)
        .ok_or_else(|| format!("No status page URL registered for provider '{provider_id}'"))?;
    open_url_in_browser(&url)
}

#[tauri::command]
pub async fn trigger_provider_login(
    app: tauri::AppHandle,
    provider_id: String,
) -> Result<(), String> {
    let id = parse_provider_arg(&provider_id)?;
    let provider_id = id.cli_name().to_string();

    let result = if id == ProviderId::Copilot {
        run_copilot_device_login(&app).await
    } else if let Some(kind) = cli_login_kind(id) {
        run_cli_provider_login(&app, id, kind).await
    } else {
        Err(format!(
            "{} does not support in-app login yet. Use its credential options below, or open its dashboard.",
            id.display_name()
        ))
    };

    if result.is_err() {
        events::emit_login_phase_changed(&app, &provider_id, "failed");
    }
    result
}

async fn run_cli_provider_login(
    app: &tauri::AppHandle,
    id: ProviderId,
    kind: CliLoginKind,
) -> Result<(), String> {
    let provider_id = id.cli_name().to_string();
    let phase_app = app.clone();
    let phase_provider_id = provider_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let on_phase = move |phase| {
            events::emit_login_phase_changed(
                &phase_app,
                &phase_provider_id,
                login_phase_name(phase),
            );
        };
        match kind {
            CliLoginKind::Claude => run_claude_login(PROVIDER_LOGIN_TIMEOUT_SECS, on_phase),
            CliLoginKind::Codex => run_codex_login(PROVIDER_LOGIN_TIMEOUT_SECS, on_phase),
        }
    })
    .await
    .map_err(|error| format!("{} login task failed: {error}", id.display_name()))?;
    require_successful_login(id, result)?;

    let _ = app.emit(
        "provider-updated",
        serde_json::json!({ "providerId": provider_id }),
    );
    Ok(())
}

fn require_successful_login(id: ProviderId, result: LoginResult) -> Result<(), String> {
    match result.outcome {
        LoginOutcome::Success => Ok(()),
        LoginOutcome::TimedOut => Err(format!(
            "{} login timed out. Try again, or use the manual credential options below.",
            id.display_name()
        )),
        LoginOutcome::Failed { status } => Err(format!(
            "{} login exited with status {status}. Try again, or use the manual credential options below.",
            id.display_name()
        )),
        LoginOutcome::MissingBinary => Err(format!(
            "{} login CLI was not found on PATH. Install it, or use the manual credential options below.",
            id.display_name()
        )),
        LoginOutcome::LaunchFailed(error) => Err(format!(
            "Could not start {} login: {error}. Use the manual credential options below.",
            id.display_name()
        )),
    }
}

/// Attempts to open the verification URL and reports the outcome, but never
/// fails the caller: by the time this runs, the code has already been
/// emitted and is valid on its own, so a browser-open failure (no default
/// browser, blocked by policy, ...) must not abort the login. `open` is
/// injected so this decision is testable without launching a real browser.
fn open_verification_url_best_effort(
    url: &str,
    open: impl FnOnce(&str) -> Result<(), String>,
) -> Option<String> {
    open(url).err()
}

async fn run_copilot_device_login(app: &tauri::AppHandle) -> Result<(), String> {
    events::emit_login_phase_changed(app, ProviderId::Copilot.cli_name(), "requesting");
    let flow = CopilotDeviceFlow::new();
    let device = flow
        .start_flow()
        .await
        .map_err(|e| format!("GitHub device login failed: {e}"))?;

    // Show the code and the URL to enter it at first: the device flow is
    // already valid and the user can navigate there by hand, so a
    // browser-open failure (no default browser, blocked by policy, ...) must
    // not strand them with no code and no link at all. Resolve the URL once
    // so the one shown/copied is guaranteed to be the one actually opened.
    let verification_url = device.verification_url_to_open();
    events::emit_login_phase_changed_with_code(
        app,
        ProviderId::Copilot.cli_name(),
        "waitingBrowser",
        &device.user_code,
        verification_url,
    );
    if let Some(error) = open_verification_url_best_effort(verification_url, open_url_in_browser) {
        tracing::warn!(%error, "failed to open the GitHub device-flow verification URL");
    }

    let token = flow
        .wait_for_token(&device.device_code, device.interval, device.expires_in)
        .await
        .map_err(|e| format!("GitHub device login failed: {e}"))?;

    let api = CopilotApi::new();
    let identity = api.fetch_identity_with_token(&token, None).await.ok();
    let plan = api
        .fetch_usage_with_token(&token, None)
        .await
        .ok()
        .and_then(|usage| usage.login_method);

    let login = identity.as_ref().map(|identity| identity.login.clone());
    let label = match (login.as_deref(), plan.as_deref()) {
        (Some(login), Some(plan)) => format!("{login} ({plan})"),
        (Some(login), None) => login.to_string(),
        (None, Some(plan)) => plan.to_string(),
        (None, None) => "GitHub Copilot".to_string(),
    };

    TokenAccountStore::new()
        .try_update_provider(ProviderId::Copilot, |data| {
            let existing_index = login.as_deref().and_then(|login| {
                data.accounts.iter().position(|account| {
                    account.label == login || account.label.starts_with(&format!("{login} ("))
                })
            });

            if let Some(index) = existing_index {
                data.accounts[index].token = token;
                data.accounts[index].label = label;
                data.set_active(index);
            } else {
                let mut account = TokenAccount::new(label, token);
                account.mark_used();
                data.add_account(account);
                data.set_active(data.accounts.len().saturating_sub(1));
            }
            Ok(())
        })
        .map_err(|e| e.to_string())?;

    let _ = app.emit(
        "provider-updated",
        serde_json::json!({ "providerId": "copilot" }),
    );
    events::emit_login_phase_changed(app, ProviderId::Copilot.cli_name(), "complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_url_resolves_from_codex_provider_metadata() {
        assert_eq!(
            dashboard_url_for_provider("codex").as_deref(),
            Some("https://chatgpt.com/codex/settings/usage")
        );
    }

    #[test]
    fn cli_login_routing_is_provider_specific() {
        assert_eq!(
            cli_login_kind(ProviderId::Claude),
            Some(CliLoginKind::Claude)
        );
        assert_eq!(cli_login_kind(ProviderId::Codex), Some(CliLoginKind::Codex));
        assert_eq!(cli_login_kind(ProviderId::Gemini), None);
        assert_eq!(cli_login_kind(ProviderId::Grok), None);
    }

    #[test]
    fn failed_login_does_not_expose_captured_cli_output() {
        let result = LoginResult {
            outcome: LoginOutcome::Failed { status: 7 },
            output: "secret-token-from-cli".to_string(),
            auth_link: None,
        };

        let error = require_successful_login(ProviderId::Codex, result).unwrap_err();

        assert!(error.contains("status 7"));
        assert!(!error.contains("secret-token-from-cli"));
        assert!(error.contains("manual credential options"));
    }

    #[test]
    fn verification_url_open_failure_is_reported_not_fatal() {
        let outcome = open_verification_url_best_effort("https://github.com/login/device", |_| {
            Err("no default browser".to_string())
        });

        assert_eq!(outcome, Some("no default browser".to_string()));
    }

    #[test]
    fn verification_url_open_success_reports_nothing() {
        let outcome =
            open_verification_url_best_effort("https://github.com/login/device", |_| Ok(()));

        assert_eq!(outcome, None);
    }
}
