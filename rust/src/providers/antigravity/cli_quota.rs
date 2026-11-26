//! Read-only quota command supported by Antigravity CLI.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;

use crate::core::{ProviderError, ProviderFetchResult, RateWindow, UsageSnapshot};

fn scratch_workspace() -> Result<(PathBuf, Option<tempfile::TempDir>), ProviderError> {
    if let Some(dir) = dirs::data_local_dir().map(|dir| dir.join("Ceiling/agy-scratch"))
        && std::fs::create_dir_all(&dir).is_ok()
    {
        return Ok((dir, None));
    }
    let temp = tempfile::tempdir().map_err(|_| {
        ProviderError::Other("Could not prepare the Antigravity quota check.".into())
    })?;
    Ok((temp.path().to_path_buf(), Some(temp)))
}

pub(super) fn binary() -> Option<PathBuf> {
    which::which("agy").ok().or_else(|| {
        let path = dirs::data_local_dir()?.join("agy/bin/agy.exe");
        path.is_file().then_some(path)
    })
}

pub(super) async fn fetch(binary: PathBuf) -> Result<ProviderFetchResult, ProviderError> {
    let (workspace_dir, _temp_guard) = scratch_workspace()?;
    let run = |args: &[&str]| {
        let mut command = tokio::process::Command::new(&binary);
        command
            .args(args)
            .current_dir(&workspace_dir)
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
    let usage = output
        .status
        .success()
        .then(|| parse_quota(&output.stdout))
        .flatten()
        .ok_or_else(|| {
            ProviderError::Other(
                "Antigravity CLI did not report quota. Check sign-in and /usage in agy.".into(),
            )
        })?;
    Ok(ProviderFetchResult::new(usage, "cli"))
}

fn supports_quota(version: &[u8]) -> bool {
    let Ok(version) = std::str::from_utf8(version) else {
        return false;
    };
    // Read-only structured /usage is verified on 1.2.13 (issue #487).
    // Accept bare `1.2.13`, prefixed `v1.2.13` / `agy 1.2.13`, and prerelease
    // or build suffixes such as `1.3.0-preview.1` or `1.2.13+abc`.
    version.split_whitespace().any(|token| {
        let core = token
            .trim_start_matches('v')
            .split(['-', '+'])
            .next()
            .unwrap_or_default();
        let mut parts = core.split('.');
        let (Some(major), Some(minor), Some(patch)) = (parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        matches!(
            (major.parse::<u32>(), minor.parse::<u32>(), patch.parse::<u32>()),
            (Ok(major), Ok(minor), Ok(patch))
                if parts.next().is_none() && (major, minor, patch) >= (1, 2, 13)
        )
    })
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
        ("3p-weekly", "Claude and GPT Weekly", 10_080),
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
        assert!(!supports_quota(b"1.2.12-rc.1"));
        assert!(!supports_quota(b"1.2"));
        assert!(supports_quota(b"1.2.13"));
        assert!(supports_quota(b"v1.2.13"));
        assert!(supports_quota(b"agy 1.2.13 (build 42)"));
        assert!(supports_quota(b"1.3.0-preview.1"));
        assert!(supports_quota(
            b"1.5.16
"
        ));
        assert!(!supports_quota(b"unknown"));
    }

    #[test]
    fn parse_quota_handles_weekly_only_disabled_and_invalid_responses() {
        let weekly_only = br#"{"status":"SUCCESS","num_turns":0,"command":{"name":"usage","data":{"groups":[
            {"buckets":[{"id":"gemini-5h","remaining_fraction":0.5,"disabled":true},{"id":"gemini-weekly","remaining_fraction":0.6}]}]}}}"#;
        let usage = parse_quota(weekly_only).unwrap();
        assert_eq!(usage.primary_label.as_deref(), Some("Gemini Weekly"));
        assert!(usage.secondary.is_none());

        let only_3p =
            br#"{"status":"SUCCESS","num_turns":0,"command":{"name":"usage","data":{"groups":[
            {"buckets":[{"id":"3p-weekly","remaining_fraction":0.5}]}]}}}"#;
        assert!(parse_quota(only_3p).is_none());

        let out_of_range =
            br#"{"status":"SUCCESS","num_turns":0,"command":{"name":"usage","data":{"groups":[
            {"buckets":[{"id":"gemini-5h","remaining_fraction":1.5}]}]}}}"#;
        assert!(parse_quota(out_of_range).is_none());

        let wrong_status =
            br#"{"status":"ERROR","num_turns":0,"command":{"name":"usage","data":{"groups":[
            {"buckets":[{"id":"gemini-5h","remaining_fraction":0.5}]}]}}}"#;
        assert!(parse_quota(wrong_status).is_none());
    }
}
