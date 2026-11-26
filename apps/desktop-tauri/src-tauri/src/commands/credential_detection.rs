use super::*;

// ── Credential detection ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiCliStatus {
    pub signed_in: bool,
    pub credentials_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VertexAiStatus {
    pub has_credentials: bool,
    pub credentials_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JetbrainsIde {
    pub id: String,
    pub display_name: String,
    pub path: String,
    pub detected: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroStatus {
    pub available: bool,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DetectedAccountStatus {
    Ready,
    Locked,
    Installed,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedProviderAccount {
    pub provider_id: String,
    pub display_name: String,
    pub status: DetectedAccountStatus,
    pub source_label: String,
    pub detail: String,
}

fn gemini_cli_credentials_path() -> Option<std::path::PathBuf> {
    codexbar::host::session::gemini_cli_credentials_path()
}

fn vertexai_credentials_path_raw() -> Option<std::path::PathBuf> {
    codexbar::host::session::vertexai_credentials_path()
}

fn jetbrains_detected_ide_paths() -> Vec<std::path::PathBuf> {
    codexbar::host::session::jetbrains_detected_ide_paths()
}

pub(super) fn allowed_open_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    paths.extend(gemini_cli_credentials_path());
    paths.extend(vertexai_credentials_path_raw());
    paths.extend(jetbrains_detected_ide_paths());
    paths.extend(codexbar::providers::kiro::find_kiro_cli());

    let override_path = Settings::load()
        .jetbrains_ide_base_path()
        .trim()
        .to_string();
    if !override_path.is_empty() {
        paths.push(std::path::PathBuf::from(override_path));
    }
    paths
}

#[tauri::command]
pub fn get_gemini_cli_signed_in() -> Result<GeminiCliStatus, String> {
    let path = gemini_cli_credentials_path();
    let signed_in = path.as_ref().map(|p| p.exists()).unwrap_or(false);
    Ok(GeminiCliStatus {
        signed_in,
        credentials_path: path.map(|p| p.to_string_lossy().into_owned()),
    })
}

#[tauri::command]
pub fn get_detected_provider_accounts() -> Vec<DetectedProviderAccount> {
    use codexbar::providers::claude::ClaudeDesktopSessionStatus;
    use codexbar::providers::cursor::CursorIdeSessionStatus;

    let codex_ready = codexbar::providers::codex::local_credentials_available();
    let codex_installed = codexbar::providers::codex::cli_installed();

    let claude_code_ready = codexbar::providers::claude::claude_code_credentials_available();
    let claude_desktop = codexbar::providers::claude::claude_desktop_session_status();

    let cursor = codexbar::providers::cursor::ide_session_status();

    let gemini_ready = codexbar::host::session::gemini_cli_signed_in();
    let gemini_installed = codexbar::providers::gemini::cli_installed();

    let grok_ready = codexbar::providers::grok::local_credentials_available();
    let grok_installed = codexbar::providers::grok::cli_installed();

    vec![
        detected_account(
            "codex",
            "Codex",
            if codex_ready {
                DetectedAccountStatus::Ready
            } else if codex_installed {
                DetectedAccountStatus::Installed
            } else {
                DetectedAccountStatus::Unavailable
            },
            "Codex CLI",
        ),
        detected_account(
            "claude",
            "Claude",
            if claude_code_ready || claude_desktop == ClaudeDesktopSessionStatus::Ready {
                DetectedAccountStatus::Ready
            } else {
                match claude_desktop {
                    ClaudeDesktopSessionStatus::Locked => DetectedAccountStatus::Locked,
                    ClaudeDesktopSessionStatus::SignedOut => DetectedAccountStatus::Installed,
                    ClaudeDesktopSessionStatus::Unavailable => DetectedAccountStatus::Unavailable,
                    ClaudeDesktopSessionStatus::Ready => DetectedAccountStatus::Ready,
                }
            },
            if claude_code_ready {
                "Claude Code"
            } else {
                "Claude Desktop"
            },
        ),
        detected_account(
            "cursor",
            "Cursor",
            match cursor {
                CursorIdeSessionStatus::Ready => DetectedAccountStatus::Ready,
                CursorIdeSessionStatus::Locked => DetectedAccountStatus::Locked,
                CursorIdeSessionStatus::SignedOut => DetectedAccountStatus::Installed,
                CursorIdeSessionStatus::Unavailable => DetectedAccountStatus::Unavailable,
            },
            "Cursor for Windows",
        ),
        detected_account(
            "gemini",
            "Gemini Code Assist",
            if gemini_ready {
                DetectedAccountStatus::Ready
            } else if gemini_installed {
                DetectedAccountStatus::Installed
            } else {
                DetectedAccountStatus::Unavailable
            },
            "Gemini CLI",
        ),
        detected_account(
            "grok",
            "Grok",
            if grok_ready {
                DetectedAccountStatus::Ready
            } else if grok_installed {
                DetectedAccountStatus::Installed
            } else {
                DetectedAccountStatus::Unavailable
            },
            "Grok Build",
        ),
    ]
}

fn detected_account(
    provider_id: &str,
    display_name: &str,
    status: DetectedAccountStatus,
    source_label: &str,
) -> DetectedProviderAccount {
    let detail = match status {
        DetectedAccountStatus::Ready => "Signed in and ready to track",
        DetectedAccountStatus::Locked => "Close the desktop app once to finish connecting",
        DetectedAccountStatus::Installed => "Installed; sign in to start tracking",
        DetectedAccountStatus::Unavailable => "Not found on this PC",
    };
    DetectedProviderAccount {
        provider_id: provider_id.to_string(),
        display_name: display_name.to_string(),
        status,
        source_label: source_label.to_string(),
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod detected_account_tests {
    use super::*;

    #[test]
    fn discovery_payload_contains_status_but_no_identity_or_secret_fields() {
        let account = detected_account(
            "gemini",
            "Gemini",
            DetectedAccountStatus::Ready,
            "Gemini CLI",
        );
        let value = serde_json::to_value(account).unwrap();

        assert_eq!(value["providerId"], "gemini");
        assert_eq!(value["status"], "ready");
        assert_eq!(value["sourceLabel"], "Gemini CLI");
        for forbidden in ["email", "path", "token", "cookie", "accountId"] {
            assert!(
                value.get(forbidden).is_none(),
                "unexpected {forbidden} field"
            );
        }
    }
}

#[tauri::command]
pub fn get_vertexai_status() -> Result<VertexAiStatus, String> {
    let path = vertexai_credentials_path_raw();
    let has = path.as_ref().map(|p| p.exists()).unwrap_or(false);
    Ok(VertexAiStatus {
        has_credentials: has,
        credentials_path: path.map(|p| p.to_string_lossy().into_owned()),
    })
}

#[tauri::command]
pub fn list_jetbrains_detected_ides() -> Result<Vec<JetbrainsIde>, String> {
    let settings = Settings::load();
    let override_path = settings.jetbrains_ide_base_path().to_string();

    let mut entries: Vec<JetbrainsIde> = jetbrains_detected_ide_paths()
        .into_iter()
        .map(|p| {
            let display = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string());
            JetbrainsIde {
                id: display.to_lowercase(),
                display_name: display,
                path: p.to_string_lossy().into_owned(),
                detected: true,
            }
        })
        .collect();

    // If the user has an override that isn't already in the detected list,
    // surface it explicitly with `detected: false`.
    if !override_path.is_empty() && !entries.iter().any(|e| e.path == override_path) {
        let path_buf = std::path::PathBuf::from(&override_path);
        let display = path_buf
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| override_path.clone());
        entries.push(JetbrainsIde {
            id: format!("override::{display}").to_lowercase(),
            display_name: display,
            path: override_path,
            detected: false,
        });
    }

    Ok(entries)
}

#[tauri::command]
pub fn set_jetbrains_ide_path(path: String) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("JetBrains IDE path is empty".to_string());
    }
    let pb = std::path::PathBuf::from(trimmed);
    if !pb.is_absolute() {
        return Err("JetBrains IDE path must be absolute".to_string());
    }
    if !pb.is_dir() {
        return Err(format!("JetBrains IDE path is not a directory: {trimmed}"));
    }
    Settings::update(|settings| {
        settings.set_jetbrains_ide_base_path(pb.to_string_lossy().into_owned())
    })
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_kiro_status() -> Result<KiroStatus, String> {
    if let Some(path) = codexbar::providers::kiro::find_kiro_cli() {
        Ok(KiroStatus {
            available: true,
            hint: Some(path.to_string_lossy().into_owned()),
        })
    } else {
        Ok(KiroStatus {
            available: false,
            hint: Some("kiro-cli: not found on PATH or known install locations".into()),
        })
    }
}
