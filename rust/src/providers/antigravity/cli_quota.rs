//! Read-only quota command supported by Antigravity CLI.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;

use crate::cli::tty_runner::{TtyCommandOptions, TtyCommandRunner};
use crate::core::{ProviderError, ProviderFetchResult, RateWindow, UsageSnapshot};

const PLAN_READY: &str = "Ceiling Antigravity plan ready";
const EXPORT_PLAN: &str = r#"
$status = [Console]::In.ReadToEnd() | ConvertFrom-Json
if ($status.product -eq 'antigravity' -and $status.plan_tier -is [string] -and $status.plan_tier.Trim()) {
    [IO.File]::WriteAllText($env:CEILING_ANTIGRAVITY_PLAN_FILE, $status.plan_tier)
    [Console]::WriteLine('Ceiling Antigravity plan ready')
}
"#;

pub(super) fn binary() -> Option<PathBuf> {
    which::which("agy").ok().or_else(|| {
        let path = dirs::data_local_dir()?.join("agy/bin/agy.exe");
        path.is_file().then_some(path)
    })
}

pub(super) async fn fetch(binary: PathBuf) -> Result<ProviderFetchResult, ProviderError> {
    let workspace = tempfile::tempdir().map_err(|_| {
        ProviderError::Other("Could not prepare the Antigravity quota check.".into())
    })?;
    let run = |args: &[&str]| {
        let mut command = tokio::process::Command::new(&binary);
        command
            .args(args)
            .current_dir(workspace.path())
            .stdin(Stdio::null());
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        command
    };
    let version = crate::host::tokio_cli::output_with_timeout(
        &mut run(&["--version"]),
        Duration::from_secs(3),
    )
    .await
    .ok()
    .filter(|result| result.status.success());
    if !version.is_some_and(|output| supports_quota(&output.stdout)) {
        return Err(ProviderError::Other(
            "Update Antigravity CLI to use its read-only quota command.".into(),
        ));
    }

    let mut command = run(&[
        "--sandbox",
        "-p",
        "/usage",
        "--output-format",
        "json",
        "--print-timeout",
        "8s",
    ]);
    let output = crate::host::tokio_cli::output_with_timeout(&mut command, Duration::from_secs(12))
        .await
        .map_err(|_| ProviderError::Other("Antigravity CLI quota check did not finish.".into()))?;
    let mut usage = output
        .status
        .success()
        .then(|| parse_quota(&output.stdout))
        .flatten()
        .ok_or_else(|| {
            ProviderError::Other(
                "Antigravity CLI did not report quota. Check sign-in and /usage in agy.".into(),
            )
        })?;
    if let Some(trusted_workspace) = dirs::data_local_dir()
        .map(|dir| dir.join("Ceiling/gemini-quota"))
        .filter(|dir| dir.is_dir())
    {
        usage.login_method = tokio::task::spawn_blocking(move || {
            read_plan(&binary, &trusted_workspace, workspace.path())
        })
        .await
        .ok()
        .flatten();
    }
    Ok(ProviderFetchResult::new(usage, "cli"))
}

fn read_plan(
    binary: &std::path::Path,
    trusted_workspace: &std::path::Path,
    scratch: &std::path::Path,
) -> Option<String> {
    let powershell = crate::host::windows_powershell_exe()?;
    let powershell = powershell.to_str()?;
    // agy runs status-line commands through cmd /c. Use the trusted system path
    // only when it can be passed literally without shell quoting.
    if !powershell.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'\\' | b':' | b'.' | b'_' | b'-')
    }) {
        return None;
    }
    let script = STANDARD.encode(
        EXPORT_PLAN
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    );
    let command =
        format!("{powershell} -NoLogo -NoProfile -NonInteractive -EncodedCommand {script}");
    let settings = scratch.join("plan-settings.json");
    let output = scratch.join("plan.txt");
    let config = serde_json::json!({
        "statusLine": { "type": "command", "command": command, "enabled": true }
    });
    std::fs::write(&settings, config.to_string()).ok()?;
    let mut options = TtyCommandOptions::new()
        .with_working_directory(trusted_workspace.to_owned())
        .with_extra_args(vec![
            "--sandbox".into(),
            "--settings".into(),
            settings.to_string_lossy().into_owned(),
        ])
        .with_timeout(8.0)
        .with_initial_delay(0.0)
        .with_stop_on_substring(PLAN_READY)
        .with_stop_on_substring("Do you trust the contents of this project?");
    options.env.insert(
        "CEILING_ANTIGRAVITY_PLAN_FILE".into(),
        output.to_string_lossy().into_owned(),
    );
    TtyCommandRunner::new()
        .run(&binary.to_string_lossy(), "", options)
        .ok()?;
    sanitize_plan(&std::fs::read_to_string(output).ok()?)
}

fn sanitize_plan(plan: &str) -> Option<String> {
    let plan = plan.trim();
    (!plan.is_empty() && plan.chars().count() <= 80 && !plan.chars().any(char::is_control))
        .then(|| plan.to_owned())
}

fn supports_quota(version: &[u8]) -> bool {
    let Ok(version) = std::str::from_utf8(version) else {
        return false;
    };
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let (Some(major), Some(minor), Some(patch)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    // Read-only structured /usage is verified on 1.2.13 (issue #487).
    matches!((major.parse::<u32>(), minor.parse::<u32>(), patch.parse::<u32>()),
        (Ok(major), Ok(minor), Ok(patch)) if parts.next().is_none() && (major, minor, patch) >= (1, 2, 13))
}

#[derive(Deserialize)]
struct Response {
    status: String,
    num_turns: u32,
    command: QuotaCommand,
}

#[derive(Deserialize)]
struct QuotaCommand {
    name: String,
    data: QuotaData,
}

#[derive(Deserialize)]
struct QuotaData {
    groups: Vec<QuotaGroup>,
}

#[derive(Deserialize)]
struct QuotaGroup {
    buckets: Vec<Bucket>,
}

#[derive(Deserialize)]
struct Bucket {
    id: String,
    remaining_fraction: Option<f64>,
    reset_time: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    disabled: bool,
}

fn parse_quota(data: &[u8]) -> Option<UsageSnapshot> {
    let response: Response = serde_json::from_slice(data).ok()?;
    if response.status != "SUCCESS" || response.num_turns != 0 || response.command.name != "usage" {
        return None;
    }
    let buckets = response
        .command
        .data
        .groups
        .iter()
        .flat_map(|group| &group.buckets);
    let window = |id: &str, minutes: u32| {
        let bucket = buckets
            .clone()
            .find(|bucket| bucket.id == id && !bucket.disabled)?;
        let remaining = bucket
            .remaining_fraction
            .filter(|value| (0.0..=1.0).contains(value))?;
        Some(RateWindow::with_details(
            (1.0 - remaining) * 100.0,
            Some(minutes),
            bucket.reset_time,
            None,
        ))
    };
    let five = window("gemini-5h", 300);
    let weekly = window("gemini-weekly", 10_080);
    let mut usage = match (five, weekly) {
        (Some(five), Some(weekly)) => UsageSnapshot::new(five).with_secondary(weekly),
        (Some(window), None) | (None, Some(window)) => UsageSnapshot::new(window),
        (None, None) => return None,
    };
    usage.primary_label = Some(if usage.primary.window_minutes == Some(300) {
        "Gemini 5-hour".into()
    } else {
        "Gemini Weekly".into()
    });
    if usage.secondary.is_some() {
        usage.secondary_label = Some("Gemini Weekly".into());
    }
    for (id, title, minutes) in [
        ("3p-5h", "Claude and GPT 5-hour", 300),
        ("3p-weekly", "Claude and GPT weekly", 10_080),
    ] {
        if let Some(window) = window(id, minutes) {
            usage = usage.with_extra_rate_window(id, title, window);
        }
    }
    Some(usage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_badge_requires_a_nonempty_printable_tier() {
        assert_eq!(
            sanitize_plan(" Google AI Pro "),
            Some("Google AI Pro".into())
        );
        assert_eq!(sanitize_plan(" "), None);
        assert_eq!(sanitize_plan("Pro\nInjected"), None);
    }

    #[test]
    fn reads_zero_remaining_and_weekly_quota_from_the_command() {
        let response = br#"{"status":"SUCCESS","num_turns":0,"command":{"name":"usage","data":{"groups":[
            {"buckets":[{"id":"gemini-5h","remaining_fraction":0},{"id":"gemini-weekly","remaining_fraction":0.75}]},
            {"buckets":[{"id":"3p-weekly","remaining_fraction":1}]}]}}}"#;
        let usage = parse_quota(response).unwrap();
        assert_eq!(usage.primary.used_percent, 100.0);
        assert_eq!(usage.secondary.unwrap().used_percent, 25.0);
        assert_eq!(usage.extra_rate_windows[0].id, "3p-weekly");
    }

    #[test]
    fn rejects_cli_versions_without_verified_print_usage_support() {
        assert!(!supports_quota(b"1.2.12"));
        assert!(supports_quota(b"1.2.13"));
        assert!(supports_quota(b"1.5.16\r\n"));
        assert!(!supports_quota(b"unknown"));
    }
}
