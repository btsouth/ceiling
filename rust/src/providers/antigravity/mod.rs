//! Antigravity provider implementation
//!
//! Fetches usage data from Antigravity's local language server probe
//! Uses Windows process detection to find CSRF token

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use regex_lite::Regex;
use serde::Deserialize;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::OnceLock;

use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    RateWindow, SourceMode, UsageSnapshot,
};

const NOT_RUNNING_MESSAGE: &str = "Antigravity language server not running. Start Google Antigravity or run `agy`, sign in, then retry.";
pub(crate) const CLI_TOKEN_REQUIRED_MESSAGE: &str = "This version of the Antigravity CLI requires a local access token that Ceiling cannot read. Start agy with `agy --csrf_token <any random value>` (PowerShell: agy --csrf_token (New-Guid)), then retry.";

/// Antigravity provider
pub struct AntigravityProvider {
    metadata: ProviderMetadata,
}

impl AntigravityProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::Antigravity,
                display_name: "Antigravity",
                session_label: "Claude",
                weekly_label: "Gemini Pro",
                supports_opus: true,
                supports_credits: false,
                default_enabled: false,
                is_primary: false,
                dashboard_url: None,
                status_page_url: None,
            },
        }
    }

    /// Detect running Antigravity language server and extract connection info.
    ///
    /// Covers both the Google Antigravity IDE (`language_server.exe`) and the
    /// Antigravity CLI (`agy` / `antigravity-cli`). Some CLI versions permit
    /// tokenless local Connect API requests; others require CSRF. Only use tokens advertised by the matched process.
    fn detect_process_infos() -> Result<Vec<ProcessInfo>, ProviderError> {
        // Use PowerShell to get process command lines
        #[cfg(windows)]
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let powershell = crate::host::windows_powershell_exe()
            .ok_or_else(|| ProviderError::Other("Trusted Windows PowerShell not found".into()))?;
        let mut cmd = Command::new(powershell);
        cmd.args([
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            // Match by image name only where possible. Scanning every CommandLine for
            // "agy" also matched the PowerShell detector itself (and other shells that
            // mention the binary in a script), which then had no listening ports.
            // node + antigravity-cli package is the only CommandLine-based case.
            "Get-CimInstance Win32_Process | Where-Object { \
                    $_.Name -like '*language_server_windows*' -or \
                    $_.Name -like 'language_server.exe' -or \
                    $_.Name -eq 'agy.exe' -or \
                    $_.Name -like '*antigravity-cli*' -or \
                    $_.Name -like '*antigravity_cli*' -or \
                    ($_.Name -eq 'node.exe' -and $_.CommandLine -and \
                        $_.CommandLine -match '(?i)antigravity[-_]cli') \
                } | ForEach-Object { \"$($_.ProcessId)`t$($_.Name)`t$($_.CommandLine)\" }",
        ]);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let output = cmd
            .output()
            .map_err(|e| ProviderError::Other(format!("Failed to run PowerShell: {}", e)))?;

        if !output.status.success() {
            return Err(ProviderError::NotInstalled(
                "Failed to detect Antigravity process".to_string(),
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let processes = Self::parse_process_infos(&stdout);
        if processes.is_empty() {
            return Err(ProviderError::NotInstalled(NOT_RUNNING_MESSAGE.to_string()));
        }
        Ok(processes)
    }

    /// Whether a command line is an Antigravity CLI process (`agy` / `antigravity-cli`).
    ///
    /// Matches the **executable token** (or a node entrypoint under `antigravity-cli`),
    /// not a random later argument. That avoids treating a shell whose script text
    /// mentions `agy` as the language server.
    fn is_cli_command_line(command_line: &str) -> bool {
        let trimmed = command_line.trim();
        // First token is the image path; strip surrounding quotes Windows often adds.
        let first = trimmed
            .trim_start_matches('"')
            .split(['"', ' ', '\t'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if first.ends_with("agy.exe")
            || first.ends_with("/agy")
            || first.ends_with("\\agy")
            || first == "agy"
        {
            return true;
        }
        let lower = trimmed.to_ascii_lowercase();
        // node …/antigravity-cli/… package entrypoints (bare `node` or node.exe)
        let is_node = first == "node"
            || first.ends_with("node.exe")
            || first.ends_with("/node")
            || first.ends_with("\\node");
        if is_node {
            return lower.contains("antigravity-cli") || lower.contains("antigravity_cli");
        }
        first.contains("antigravity-cli") || first.contains("antigravity_cli")
    }

    /// Whether an image name is an Antigravity CLI process.
    ///
    /// The PowerShell filter already matches `agy.exe` by image name, so the
    /// name alone is enough even when WMI cannot read the process's command
    /// line. That covers the `agy.exe` case where `$_.CommandLine` is empty.
    fn is_cli_image_name(image_name: &str) -> bool {
        let name = image_name.trim().to_ascii_lowercase();
        !name.is_empty()
            && (name == "agy.exe"
                || name == "agy"
                || name.contains("antigravity-cli")
                || name.contains("antigravity_cli"))
    }

    /// Whether a command line is an Antigravity IDE language_server process.
    fn is_ide_command_line(command_line: &str) -> bool {
        let lower = command_line.to_ascii_lowercase();
        lower.contains("language_server")
            && (lower.contains("antigravity")
                || lower.contains("override_ide_name")
                || lower.contains("app_data_dir")
                || lower.contains("csrf_token"))
    }

    #[cfg(test)]
    fn parse_process_info(stdout: &str) -> Option<ProcessInfo> {
        Self::parse_process_infos(stdout).into_iter().next()
    }

    fn parse_process_infos(stdout: &str) -> Vec<ProcessInfo> {
        // Parse command line for CSRF token and port — compiled once.
        // Antigravity 2.3+ may omit `--extension_server_port` entirely and instead
        // advertise `--https_server_port 0` (OS-assigned). Discovery then depends on
        // the process PID + listening-port enumeration, not a fixed advertised port.
        //
        // Older CLI versions may accept tokenless requests. Keep those candidates,
        // but do not assume that newer versions will accept them.
        static CSRF_RE: OnceLock<Regex> = OnceLock::new();
        static EXT_CSRF_RE: OnceLock<Regex> = OnceLock::new();
        static EXT_PORT_RE: OnceLock<Regex> = OnceLock::new();
        static HTTPS_PORT_RE: OnceLock<Regex> = OnceLock::new();
        // Accept both `--flag value` and `--flag=value` forms.
        let csrf_regex = CSRF_RE.get_or_init(|| {
            Regex::new(r#"(?:^|\s)--csrf_token(?:=|\s+)(?:"([^"\r\n]+)"|([^\s"]+))"#)
                .expect("valid regex")
        });
        let ext_csrf_regex = EXT_CSRF_RE.get_or_init(|| {
            Regex::new(
                r#"(?:^|\s)--extension_server_csrf_token(?:=|\s+)(?:"([^"\r\n]+)"|([^\s"]+))"#,
            )
            .expect("valid regex")
        });
        let ext_port_regex = EXT_PORT_RE.get_or_init(|| {
            Regex::new(r"--extension_server_port(?:=|\s+)(\d+)").expect("valid regex")
        });
        let https_port_regex = HTTPS_PORT_RE
            .get_or_init(|| Regex::new(r"--https_server_port(?:=|\s+)(\d+)").expect("valid regex"));

        let mut processes = Vec::new();
        for raw_line in stdout.lines() {
            if raw_line.trim().is_empty() {
                continue;
            }

            // Line is "<pid>\t<image name>\t<command line>". The image name is
            // emitted separately because WMI can return an empty CommandLine,
            // and the CLI is identified by its image name alone. Older
            // two-column output (pid + command line) still parses.
            let columns: Vec<&str> = raw_line.splitn(3, '\t').collect();
            let (pid, image_name, line) = match columns.as_slice() {
                [pid, name, line] => (pid.trim().parse::<u32>().ok(), name.trim(), line.trim()),
                [pid, line] => (pid.trim().parse::<u32>().ok(), "", line.trim()),
                _ => (None, "", raw_line.trim()),
            };

            let is_cli = Self::is_cli_command_line(line) || Self::is_cli_image_name(image_name);
            let is_ide = Self::is_ide_command_line(line);
            if !is_cli && !is_ide {
                continue;
            }

            let csrf_token = csrf_regex
                .captures(line)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
                .filter(|m| !m.as_str().starts_with("--"))
                .map(|m| m.as_str().to_string());

            let ext_csrf_token = ext_csrf_regex
                .captures(line)
                .and_then(|c| c.get(1).or_else(|| c.get(2)))
                .filter(|m| !m.as_str().starts_with("--"))
                .map(|m| m.as_str().to_string());

            let extension_port = ext_port_regex
                .captures(line)
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<u16>().ok());

            let https_port = https_port_regex
                .captures(line)
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse::<u16>().ok());

            // Prefer the explicit extension server port when present and non-zero.
            // A zero HTTPS port means "OS assigns the real port" and is not a probe
            // target; keep 0 so discovery relies on PID enumeration instead.
            let port = match extension_port {
                Some(p) if p > 0 => p,
                _ => match https_port {
                    Some(p) if p > 0 => p,
                    _ => 0,
                },
            };

            // Need a real advertised port or a PID to enumerate listening ports.
            if port == 0 && pid.is_none() {
                continue;
            }

            // IDE language_server requires CSRF. Tokenless IDE matches are skipped so a
            // later valid IDE (or CLI) candidate can still be found.
            // Keep tokenless CLI candidates so older versions still work.
            let token = match (is_cli, csrf_token) {
                (_, Some(token)) => token,
                (true, None) => String::new(),
                (false, None) => continue,
            };

            processes.push(ProcessInfo {
                is_cli,
                csrf_token: token,
                extension_server_csrf_token: ext_csrf_token,
                extension_port: port,
                pid,
            });
        }

        processes
    }

    /// CLI credentials must stay paired with that process's own listening ports.
    /// IDEs and tokenless CLIs retain the historical heuristic port fallback.
    fn api_port_candidates(process: &ProcessInfo) -> Vec<u16> {
        let mut ports = process
            .pid
            .map(Self::listening_ports_for_pid)
            .unwrap_or_default();
        if !process.is_cli
            || (process.csrf_token.is_empty() && process.extension_server_csrf_token.is_none())
        {
            if process.extension_port > 0 {
                ports
                    .extend((0..20u16).map(|offset| process.extension_port.saturating_add(offset)));
            }
            ports.extend([53835, 53836, 53837, 53838, 53845, 53849]);
        }
        let mut unique = Vec::new();
        for port in ports {
            if port > 0 && !unique.contains(&port) {
                unique.push(port);
            }
        }
        unique
    }

    /// Enumerate the TCP ports a given PID is listening on (Windows `lsof` equivalent).
    /// On Windows this uses `Get-NetTCPConnection`; it returns an empty list on any failure
    /// so the caller deterministically falls back to the heuristic candidate ports.
    #[cfg(windows)]
    fn listening_ports_for_pid(pid: u32) -> Vec<u16> {
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let Some(powershell) = crate::host::windows_powershell_exe() else {
            return Vec::new();
        };
        let mut cmd = Command::new(powershell);
        cmd.args([
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &format!(
                "Get-NetTCPConnection -OwningProcess {pid} -State Listen \
                 -ErrorAction SilentlyContinue | Select-Object -ExpandProperty LocalPort"
            ),
        ]);
        cmd.creation_flags(CREATE_NO_WINDOW);

        let Ok(output) = cmd.output() else {
            return Vec::new();
        };
        if !output.status.success() {
            return Vec::new();
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut ports: Vec<u16> = stdout
            .lines()
            .filter_map(|l| l.trim().parse::<u16>().ok())
            .collect();
        ports.sort_unstable();
        ports.dedup();
        ports
    }

    /// Non-Windows platforms have no `Get-NetTCPConnection`; return an empty list by design so
    /// IDEs and tokenless CLIs retain heuristic ports; credentialed CLIs require owned listeners.
    #[cfg(not(windows))]
    fn listening_ports_for_pid(_pid: u32) -> Vec<u16> {
        Vec::new()
    }

    /// Fetch usage from Antigravity's local language-server API.
    ///
    /// Prefers `RetrieveUserQuotaSummary` (shared model-group weekly / 5-hour
    /// pools that match Settings → Models). Falls back to per-model
    /// `remainingFraction` on `GetUserStatus` when the summary is missing.
    async fn fetch_user_status(&self) -> Result<UsageSnapshot, ProviderError> {
        let processes = Self::detect_process_infos()?;
        // SECURITY: self-signed TLS is allowed only for the loopback URLs below.
        let client = crate::core::credentialed_http_client_builder()
            .timeout(std::time::Duration::from_secs(8))
            .danger_accept_invalid_certs(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ProviderError::Other(e.to_string()))?;

        let mut failure = None;
        for process in processes {
            let urls: Vec<String> = Self::api_port_candidates(&process)
                .into_iter()
                .map(|port| format!("https://127.0.0.1:{port}"))
                .collect();
            match self.fetch_from_candidates(&client, &process, &urls).await {
                Ok(snapshot) => return Ok(snapshot),
                Err(error) => {
                    // Keep a reached-but-rejected server's diagnosis ahead of transport noise.
                    if failure.is_none() || error.to_string().contains("HTTP ") {
                        failure = Some((error, process));
                    }
                }
            }
        }
        Err(match failure {
            Some((error, process)) => Self::user_facing_failure(&process, error),
            None => ProviderError::Other("Could not find Antigravity API port".into()),
        })
    }

    /// Translate only the final selected failure, after all ports/processes were tried.
    fn user_facing_failure(process: &ProcessInfo, error: ProviderError) -> ProviderError {
        if process.is_cli
            && process.csrf_token.is_empty()
            && process.extension_server_csrf_token.is_none()
            && error.to_string().contains("HTTP 401 (missing CSRF token)")
        {
            // The underlying error retains safe PID, auth-source and endpoint detail.
            // Keep that context in diagnostics rather than the fixed user guidance.
            tracing::debug!(%error, "Antigravity CLI requires an explicit local access token");
            ProviderError::OAuth(CLI_TOKEN_REQUIRED_MESSAGE.into())
        } else {
            error
        }
    }

    async fn fetch_from_candidates(
        &self,
        client: &reqwest::Client,
        process: &ProcessInfo,
        urls: &[String],
    ) -> Result<UsageSnapshot, ProviderError> {
        let mut failure = None;
        for base_url in urls {
            if !Self::probe_api_port(client, base_url).await {
                continue;
            }
            // A 200/401 on GetUnleashData does not establish that this port serves
            // usable quota. Try the actual quota methods, then the remaining ports.
            match self.fetch_from_api(client, process, base_url).await {
                Ok(snapshot) => return Ok(snapshot),
                Err(error) => {
                    tracing::debug!(%error, "Antigravity local quota candidate failed");
                    if failure.is_none() || error.to_string().contains("HTTP ") {
                        failure = Some(error);
                    }
                }
            }
        }
        Err(failure.unwrap_or_else(|| {
            ProviderError::Other(format!(
                "Could not find Antigravity API port ({})",
                process.connection_description()
            ))
        }))
    }

    /// Recognize the language server without sending credentials: guessed
    /// ports may belong to another local TLS listener.
    async fn probe_api_port(client: &reqwest::Client, base_url: &str) -> bool {
        // Keep rejected endpoints for a useful quota/auth diagnosis, but never
        // let them stop probing the other ports or processes.
        Self::connect_request(
            client,
            base_url,
            "GetUnleashData",
            &serde_json::json!({}),
            "",
        )
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok_and(|response| {
            matches!(
                response.status(),
                reqwest::StatusCode::OK | reqwest::StatusCode::UNAUTHORIZED
            )
        })
    }

    fn connect_request(
        client: &reqwest::Client,
        base_url: &str,
        method: &str,
        body: &serde_json::Value,
        csrf_token: &str,
    ) -> reqwest::RequestBuilder {
        let url = format!("{base_url}/exa.language_server_pb.LanguageServerService/{method}");
        let mut request = client
            .post(url)
            .header("Content-Type", "application/json")
            .header("Connect-Protocol-Version", "1")
            .json(body);
        if !csrf_token.is_empty() {
            request = request.header("X-Codeium-Csrf-Token", csrf_token);
        }
        request
    }

    async fn fetch_from_api(
        &self,
        client: &reqwest::Client,
        process_info: &ProcessInfo,
        base_url: &str,
    ) -> Result<UsageSnapshot, ProviderError> {
        let (csrf_token, alt_csrf) = process_info.csrf_tokens();

        // Plan / identity still live on GetUserStatus. Keep safe error reasons;
        // server response bodies may contain credentials and must not be logged.
        // Dropping them here is what left "no user status" with no way to tell
        // a signed-out server from a changed response shape (#412).
        let mut status_reason: Option<String> = None;
        let user_status = match Self::post_connect_json::<UserStatusResponse>(
            client,
            base_url,
            "GetUserStatus",
            &serde_json::json!({
                "metadata": {
                    "ideName": "antigravity",
                    "extensionName": "antigravity",
                    "ideVersion": "unknown",
                    "locale": "en"
                }
            }),
            csrf_token,
            alt_csrf,
        )
        .await
        {
            Ok(response) => {
                if response.user_status.is_none() {
                    status_reason = Some("empty response".to_string());
                }
                response.user_status
            }
            Err(error) => {
                tracing::debug!(reason = %short_reason(&error), "Antigravity GetUserStatus failed");
                status_reason = Some(short_reason(&error));
                None
            }
        };

        let plan_name = user_status.as_ref().and_then(|status| {
            status
                .plan_status
                .as_ref()
                .and_then(|ps| ps.plan_info.as_ref())
                .and_then(|pi| pi.plan_display_name.clone().or(pi.plan_name.clone()))
        });
        let email = user_status.as_ref().and_then(|status| status.email.clone());

        // Shared group pools (what Antigravity Settings shows).
        let summary_reason: Option<String> = match Self::post_connect_json::<QuotaSummaryResponse>(
            client,
            base_url,
            "RetrieveUserQuotaSummary",
            &serde_json::json!({}),
            csrf_token,
            alt_csrf,
        )
        .await
        {
            Ok(summary) => match parse_quota_summary(&summary) {
                Some(mut snapshot) => {
                    if let Some(plan) = plan_name {
                        snapshot = snapshot.with_login_method(plan);
                    }
                    if let Some(email) = email {
                        snapshot = snapshot.with_email(email);
                    }
                    return Ok(snapshot);
                }
                None => {
                    tracing::debug!("Antigravity quota summary carried no usable quota");
                    Some("empty response".to_string())
                }
            },
            Err(error) => {
                tracing::debug!(reason = %short_reason(&error), "Antigravity RetrieveUserQuotaSummary failed");
                Some(short_reason(&error))
            }
        };

        // Fallback: older path using per-model remainingFraction on GetUserStatus.
        let Some(status) = user_status else {
            return Err(ProviderError::Other(format!(
                "Antigravity returned no user status and no quota summary ({}; {}; endpoint: {base_url})",
                describe_unusable(status_reason.as_deref(), summary_reason.as_deref()),
                process_info.connection_description()
            )));
        };
        let mut snapshot = self.parse_user_status(UserStatusResponse {
            user_status: Some(status),
        })?;
        if let Some(email) = email {
            snapshot = snapshot.with_email(email);
        }
        Ok(snapshot)
    }

    /// POST a Connect/JSON method on the local language server.
    async fn post_connect_json<T: for<'de> Deserialize<'de>>(
        client: &reqwest::Client,
        base_url: &str,
        method: &str,
        body: &serde_json::Value,
        csrf_token: &str,
        alt_csrf: Option<&str>,
    ) -> Result<T, ProviderError> {
        let resp = Self::connect_request(client, base_url, method, body, csrf_token)
            .send()
            .await
            .map_err(|e| ProviderError::Other(format!("API request failed: {}", e)))?;

        if !resp.status().is_success() {
            if let Some(alt) = alt_csrf {
                let retry = Self::connect_request(client, base_url, method, body, alt)
                    .send()
                    .await;
                if let Ok(retry) = retry
                    && retry.status().is_success()
                {
                    return retry
                        .json()
                        .await
                        .map_err(|e| ProviderError::Parse(e.to_string()));
                }
            }
            let status = resp.status();
            // Only preserve an exact, known CSRF diagnosis. Never echo the body.
            let detail = if status == reqwest::StatusCode::UNAUTHORIZED {
                resp.json::<serde_json::Value>()
                    .await
                    .ok()
                    .filter(|value| value["code"] == "unauthenticated")
                    .and_then(|value| match value["message"].as_str() {
                        Some("missing CSRF token") => Some("missing CSRF token"),
                        Some("invalid CSRF token") => Some("invalid CSRF token"),
                        _ => None,
                    })
            } else {
                None
            };
            return Err(ProviderError::Other(format!(
                "API error {status} on {method}{}",
                detail.map(|text| format!(": {text}")).unwrap_or_default()
            )));
        }

        resp.json()
            .await
            .map_err(|e| ProviderError::Other(format!("Failed to parse {method}: {e}")))
    }

    fn parse_user_status(
        &self,
        response: UserStatusResponse,
    ) -> Result<UsageSnapshot, ProviderError> {
        let user_status = response
            .user_status
            .ok_or_else(|| ProviderError::Other("Missing userStatus".to_string()))?;

        let model_configs = user_status
            .cascade_model_config_data
            .and_then(|d| d.client_model_configs)
            .unwrap_or_default();

        let mut quota_configs = model_configs
            .iter()
            .filter(|config| config.quota_info.is_some())
            .filter(|config| !model_label(config).is_empty())
            .collect::<Vec<_>>();
        quota_configs.sort_by(|a, b| compare_model_configs(a, b));

        let summary_candidates = quota_configs
            .iter()
            .copied()
            .filter(|config| !is_noisy_summary_model(model_label(config)))
            .collect::<Vec<_>>();

        let primary = best_summary_model(&summary_candidates, ModelFamily::Claude)
            .and_then(|config| config.quota_info.as_ref())
            .map(rate_window_from_quota)
            .or_else(|| {
                summary_candidates
                    .first()
                    .and_then(|config| config.quota_info.as_ref())
                    .map(rate_window_from_quota)
            })
            .or_else(|| {
                quota_configs
                    .first()
                    .and_then(|config| config.quota_info.as_ref())
                    .map(rate_window_from_quota)
            });

        let secondary = best_summary_model(&summary_candidates, ModelFamily::GeminiPro)
            .and_then(|config| config.quota_info.as_ref())
            .map(rate_window_from_quota);

        let tertiary = best_summary_model(&summary_candidates, ModelFamily::GeminiFlash)
            .and_then(|config| config.quota_info.as_ref())
            .map(rate_window_from_quota);

        let primary = primary.unwrap_or_else(|| RateWindow::new(0.0));
        let mut snapshot = UsageSnapshot::new(primary);

        if let Some(sec) = secondary {
            snapshot = snapshot.with_secondary(sec);
        }
        if let Some(ter) = tertiary {
            snapshot = snapshot.with_model_specific(ter);
        }

        for config in quota_configs {
            let Some(quota) = &config.quota_info else {
                continue;
            };
            let title = clean_model_label(model_label(config));
            if title.is_empty() {
                continue;
            }
            snapshot = snapshot.with_extra_rate_window(
                model_window_id(config),
                title,
                rate_window_from_quota(quota),
            );
        }

        // Add plan info
        let plan_name = user_status
            .plan_status
            .and_then(|ps| ps.plan_info)
            .and_then(|pi| pi.plan_display_name.or(pi.plan_name));

        if let Some(plan) = plan_name {
            snapshot = snapshot.with_login_method(&plan);
        }

        Ok(snapshot)
    }
}

impl Default for AntigravityProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for AntigravityProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Antigravity
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, _ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        tracing::debug!("Fetching Antigravity usage via local probe");

        match self.fetch_user_status().await {
            Ok(usage) => Ok(ProviderFetchResult::new(usage, "local")),
            Err(e) => {
                tracing::warn!("Antigravity probe failed: {}", e);
                Err(e)
            }
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::Cli]
    }

    fn supports_cli(&self) -> bool {
        true
    }
}

struct ProcessInfo {
    is_cli: bool,
    csrf_token: String,
    extension_server_csrf_token: Option<String>,
    extension_port: u16,
    pid: Option<u32>,
}

impl ProcessInfo {
    fn csrf_tokens(&self) -> (&str, Option<&str>) {
        let primary = self
            .extension_server_csrf_token
            .as_deref()
            .unwrap_or(&self.csrf_token);
        let alternate = self
            .extension_server_csrf_token
            .as_ref()
            .filter(|_| !self.csrf_token.is_empty() && self.csrf_token != primary)
            .map(|_| self.csrf_token.as_str());
        (primary, alternate)
    }

    fn connection_description(&self) -> String {
        let source = if self.extension_server_csrf_token.is_some() {
            "--extension_server_csrf_token"
        } else if !self.csrf_token.is_empty() {
            "--csrf_token"
        } else {
            "none (tokenless CLI)"
        };
        format!(
            "process: {}; PID: {}; CSRF source: {source}{}",
            if self.is_cli {
                "agy/Antigravity CLI"
            } else {
                "IDE language server"
            },
            self.pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "unknown".into()),
            if self.csrf_tokens().1.is_some() {
                "; alternate: --csrf_token"
            } else {
                ""
            }
        )
    }
}

// API Response types

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserStatusResponse {
    user_status: Option<UserStatus>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserStatus {
    email: Option<String>,
    plan_status: Option<PlanStatus>,
    cascade_model_config_data: Option<ModelConfigData>,
}

/// Shared model-group quota pools from `RetrieveUserQuotaSummary`.
/// This is what Antigravity Settings → Models displays (weekly / 5-hour per group).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaSummaryResponse {
    response: Option<QuotaSummaryBody>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaSummaryBody {
    groups: Option<Vec<QuotaGroup>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaGroup {
    display_name: Option<String>,
    #[allow(dead_code)]
    description: Option<String>,
    buckets: Option<Vec<QuotaBucket>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaBucket {
    bucket_id: Option<String>,
    display_name: Option<String>,
    window: Option<String>,
    remaining_fraction: Option<f64>,
    reset_time: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuotaGroupKind {
    ClaudeGpt,
    Gemini,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuotaWindowKind {
    FiveHour,
    Weekly,
    Other,
}

struct ParsedQuotaBucket {
    group_title: String,
    bucket_title: String,
    group_kind: QuotaGroupKind,
    window_kind: QuotaWindowKind,
    rate: RateWindow,
    window_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanStatus {
    plan_info: Option<PlanInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanInfo {
    plan_name: Option<String>,
    plan_display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelConfigData {
    client_model_configs: Option<Vec<ModelConfig>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelConfig {
    #[serde(default)]
    label: String,
    #[serde(default)]
    model_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    quota_info: Option<QuotaInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuotaInfo {
    remaining_fraction: Option<f64>,
    reset_time: Option<String>,
}

// ── Model-family classification ──────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum ModelFamily {
    Claude,
    ClaudeThinking,
    GeminiPro,
    GeminiFlash,
    Other,
}

fn classify_model(label: &str) -> ModelFamily {
    let lower = label.to_lowercase();
    if lower.contains("claude") {
        if lower.contains("thinking") {
            ModelFamily::ClaudeThinking
        } else {
            ModelFamily::Claude
        }
    } else if lower.contains("gemini") && lower.contains("pro") {
        ModelFamily::GeminiPro
    } else if lower.contains("gemini") && lower.contains("flash") {
        ModelFamily::GeminiFlash
    } else if lower.contains("pro") && !is_noisy_summary_model(&lower) {
        ModelFamily::GeminiPro
    } else if lower.contains("flash") {
        ModelFamily::GeminiFlash
    } else {
        ModelFamily::Other
    }
}

fn best_summary_model<'a>(
    candidates: &[&'a ModelConfig],
    family: ModelFamily,
) -> Option<&'a ModelConfig> {
    candidates
        .iter()
        .copied()
        .filter(|config| classify_model(model_label(config)) == family)
        .min_by(|a, b| {
            let a_label = model_label(a);
            let b_label = model_label(b);
            let a_priority = selection_priority(a_label, family);
            let b_priority = selection_priority(b_label, family);
            a_priority
                .cmp(&b_priority)
                .then_with(|| compare_model_configs(a, b))
        })
}

fn selection_priority(label: &str, family: ModelFamily) -> u8 {
    let lower = label.to_lowercase();
    match family {
        ModelFamily::GeminiPro if lower.contains("low") => 0,
        ModelFamily::GeminiPro => 1,
        _ => 0,
    }
}

fn compare_model_configs(a: &ModelConfig, b: &ModelConfig) -> std::cmp::Ordering {
    let a_label = model_label(a);
    let b_label = model_label(b);
    family_rank(classify_model(a_label))
        .cmp(&family_rank(classify_model(b_label)))
        .then_with(|| parse_model_version(b_label).cmp(&parse_model_version(a_label)))
        .then_with(|| tier_rank(a_label).cmp(&tier_rank(b_label)))
        .then_with(|| clean_model_label(a_label).cmp(&clean_model_label(b_label)))
}

fn family_rank(family: ModelFamily) -> u8 {
    match family {
        ModelFamily::Claude => 0,
        ModelFamily::GeminiPro => 1,
        ModelFamily::GeminiFlash => 2,
        ModelFamily::ClaudeThinking => 3,
        ModelFamily::Other => 4,
    }
}

fn tier_rank(label: &str) -> u8 {
    let lower = label.to_lowercase();
    if lower.contains("high") {
        0
    } else if lower.contains("medium") {
        1
    } else if lower.contains("low") {
        2
    } else {
        3
    }
}

fn parse_model_version(label: &str) -> (u16, u16) {
    static VERSION_RE: OnceLock<Regex> = OnceLock::new();
    let regex =
        VERSION_RE.get_or_init(|| Regex::new(r"(?i)(\d+)(?:[.-](\d+))?").expect("valid regex"));
    let Some(caps) = regex.captures(label) else {
        return (0, 0);
    };
    let major = caps
        .get(1)
        .and_then(|m| m.as_str().parse::<u16>().ok())
        .unwrap_or(0);
    let minor = caps
        .get(2)
        .and_then(|m| m.as_str().parse::<u16>().ok())
        .unwrap_or(0);
    (major, minor)
}

fn is_noisy_summary_model(label: &str) -> bool {
    let lower = label.to_lowercase();
    lower.contains("image")
        || lower.contains("lite")
        || lower.contains("autocomplete")
        || lower.contains("completion")
        || lower.contains("internal")
}

fn model_label(config: &ModelConfig) -> &str {
    if !config.label.trim().is_empty() {
        &config.label
    } else if let Some(model_id) = config.model_id.as_deref() {
        model_id
    } else {
        config.id.as_deref().unwrap_or_default()
    }
}

fn model_window_id(config: &ModelConfig) -> String {
    let raw = config
        .model_id
        .as_deref()
        .or(config.id.as_deref())
        .unwrap_or_else(|| model_label(config));
    let slug = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    format!("model-{}", if slug.is_empty() { "unknown" } else { &slug })
}

/// The part of an API failure that is safe to show: the HTTP status when there
/// is one, otherwise the shape of the failure.
///
/// The message `post_connect_json` builds also carries the response body, and
/// this string ends up in the error the panel displays, so only the status code
/// is carried across.
fn short_reason(error: &ProviderError) -> String {
    let text = error.to_string();
    if let Some(rest) = text.strip_prefix("API error ") {
        let csrf_detail = if rest.starts_with("401 ") {
            ["missing CSRF token", "invalid CSRF token"]
                .into_iter()
                .find(|detail| rest.ends_with(&format!(": {detail}")))
        } else {
            None
        };
        return match rest.split_whitespace().next() {
            Some(code) => format!(
                "HTTP {code}{}",
                csrf_detail
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default()
            ),
            None => "API error".to_string(),
        };
    }
    if text.starts_with("Failed to parse") || matches!(error, ProviderError::Parse(_)) {
        return "unparseable response".to_string();
    }
    "request failed".to_string()
}

/// Compact account of why both language-server calls came back unusable, for
/// the error the panel shows (#412).
fn describe_unusable(status_reason: Option<&str>, summary_reason: Option<&str>) -> String {
    match (status_reason, summary_reason) {
        (Some(status), Some(summary)) => {
            format!("user status: {status}; quota summary: {summary}")
        }
        (Some(status), None) => format!("user status: {status}"),
        (None, Some(summary)) => format!("quota summary: {summary}"),
        (None, None) => "no detail available".to_string(),
    }
}

fn rate_window_from_quota(quota: &QuotaInfo) -> RateWindow {
    rate_window_from_remaining(quota.remaining_fraction, quota.reset_time.clone())
}

fn rate_window_from_remaining(
    remaining_fraction: Option<f64>,
    reset_time: Option<String>,
) -> RateWindow {
    let remaining = remaining_fraction.unwrap_or(1.0);
    let used_percent = ((1.0 - remaining) * 100.0).clamp(0.0, 100.0);
    // The language server reports this as an RFC 3339 timestamp, and it has to
    // land in `resets_at`: that is the field every surface formats, as a
    // relative countdown or in the user's local time. Passing it as the
    // description instead made the panel print the raw ISO string (#412).
    let resets_at = reset_time
        .as_deref()
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|parsed| parsed.with_timezone(&Utc));
    let description = match (&resets_at, reset_time) {
        // A parsed timestamp needs no fallback text; the UI formats it.
        (Some(_), _) => None,
        // An unexpected shape keeps working exactly as it did before.
        (None, Some(raw)) => Some(raw),
        (None, None) => None,
    };
    RateWindow::with_details(used_percent, None, resets_at, description)
}

fn clean_model_label(label: &str) -> String {
    let mut out = label.trim().replace('_', " ");
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    out
}

fn classify_quota_group(display_name: &str) -> QuotaGroupKind {
    let lower = display_name.to_lowercase();
    if lower.contains("claude") || lower.contains("gpt") {
        QuotaGroupKind::ClaudeGpt
    } else if lower.contains("gemini") {
        QuotaGroupKind::Gemini
    } else {
        QuotaGroupKind::Other
    }
}

fn classify_quota_window(bucket: &QuotaBucket) -> QuotaWindowKind {
    let haystack = format!(
        "{} {} {}",
        bucket.window.as_deref().unwrap_or(""),
        bucket.display_name.as_deref().unwrap_or(""),
        bucket.bucket_id.as_deref().unwrap_or("")
    )
    .to_lowercase();
    if haystack.contains("five")
        || haystack.contains("5-hour")
        || haystack.contains("5 hour")
        || haystack.contains("5h")
        || haystack.contains("five_hour")
        || haystack.contains("fivehour")
    {
        QuotaWindowKind::FiveHour
    } else if haystack.contains("week") {
        QuotaWindowKind::Weekly
    } else {
        QuotaWindowKind::Other
    }
}

fn parse_quota_summary(response: &QuotaSummaryResponse) -> Option<UsageSnapshot> {
    let groups = response.response.as_ref()?.groups.as_ref()?;
    if groups.is_empty() {
        return None;
    }

    let mut buckets: Vec<ParsedQuotaBucket> = Vec::new();
    for group in groups {
        let group_title = group
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Models")
            .to_string();
        let group_kind = classify_quota_group(&group_title);
        for bucket in group.buckets.as_deref().unwrap_or(&[]) {
            let bucket_title = bucket
                .display_name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .or(bucket.window.as_deref())
                .unwrap_or("Limit")
                .to_string();
            let window_kind = classify_quota_window(bucket);
            let id_raw = bucket
                .bucket_id
                .clone()
                .unwrap_or_else(|| format!("{group_title}-{bucket_title}"));
            let window_id = format!(
                "quota-{}",
                id_raw
                    .chars()
                    .map(|ch| {
                        if ch.is_ascii_alphanumeric() {
                            ch.to_ascii_lowercase()
                        } else {
                            '-'
                        }
                    })
                    .collect::<String>()
                    .trim_matches('-')
            );
            buckets.push(ParsedQuotaBucket {
                group_title: group_title.clone(),
                bucket_title,
                group_kind,
                window_kind,
                rate: rate_window_from_remaining(
                    bucket.remaining_fraction,
                    bucket.reset_time.clone(),
                ),
                window_id,
            });
        }
    }

    if buckets.is_empty() {
        return None;
    }

    // Prefer the tighter five-hour window for summary meters when present.
    let pick = |kind: QuotaGroupKind| {
        buckets
            .iter()
            .filter(|b| b.group_kind == kind)
            .min_by_key(|b| match b.window_kind {
                QuotaWindowKind::FiveHour => 0u8,
                QuotaWindowKind::Weekly => 1,
                QuotaWindowKind::Other => 2,
            })
    };

    let primary = pick(QuotaGroupKind::ClaudeGpt)
        .or_else(|| buckets.first())
        .map(|b| b.rate.clone())
        .unwrap_or_else(|| RateWindow::new(0.0));
    let mut snapshot = UsageSnapshot::new(primary);

    if let Some(gemini) = pick(QuotaGroupKind::Gemini) {
        snapshot = snapshot.with_secondary(gemini.rate.clone());
    }

    // If the Claude/GPT group has both five-hour and weekly, surface weekly as
    // model_specific so both AG Settings rows stay visible in the detail list.
    if let Some(weekly) = buckets.iter().find(|b| {
        b.group_kind == QuotaGroupKind::ClaudeGpt && b.window_kind == QuotaWindowKind::Weekly
    }) {
        let primary_is_five = pick(QuotaGroupKind::ClaudeGpt)
            .map(|b| b.window_kind == QuotaWindowKind::FiveHour)
            .unwrap_or(false);
        if primary_is_five {
            snapshot = snapshot.with_model_specific(weekly.rate.clone());
        }
    }

    for bucket in &buckets {
        let title = format!("{} · {}", bucket.group_title, bucket.bucket_title);
        snapshot =
            snapshot.with_extra_rate_window(bucket.window_id.clone(), title, bucket.rate.clone());
    }

    Some(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_model_families() {
        assert_eq!(classify_model("Claude 3.5 Sonnet"), ModelFamily::Claude);
        assert_eq!(classify_model("claude-4-opus"), ModelFamily::Claude);
        assert_eq!(
            classify_model("Claude Thinking"),
            ModelFamily::ClaudeThinking
        );
        assert_eq!(
            classify_model("claude-3.5-sonnet-thinking"),
            ModelFamily::ClaudeThinking
        );
        assert_eq!(classify_model("Gemini 2.5 Pro Low"), ModelFamily::GeminiPro);
        assert_eq!(classify_model("gemini-pro-low"), ModelFamily::GeminiPro);
        assert_eq!(classify_model("Pro Low Latency"), ModelFamily::GeminiPro);
        assert_eq!(classify_model("Gemini 2.5 Flash"), ModelFamily::GeminiFlash);
        assert_eq!(classify_model("gemini-flash"), ModelFamily::GeminiFlash);
        assert_eq!(classify_model("Flash Model"), ModelFamily::GeminiFlash);
        assert_eq!(classify_model("GPT-4o"), ModelFamily::Other);
        assert_eq!(classify_model("unknown-model"), ModelFamily::Other);
    }

    #[test]
    fn parses_current_language_server_process() {
        let output = r"4242	C:\Users\test\AppData\Local\Programs\Antigravity\resources\bin\language_server.exe --csrf_token 11111111-2222-3333-4444-555555555555 --extension_server_port 54123";

        let process = AntigravityProvider::parse_process_info(output).expect("process info");

        assert_eq!(process.pid, Some(4242));
        assert_eq!(process.extension_port, 54123);
        assert_eq!(process.csrf_token, "11111111-2222-3333-4444-555555555555");
    }

    /// Antigravity 2.3+ on Windows no longer advertises `--extension_server_port`.
    /// It launches with `--https_server_port 0` (OS-assigned) and a CSRF token.
    /// Detection must still succeed so PID port enumeration can find the API.
    #[test]
    fn parses_language_server_with_https_server_port_zero() {
        let output = r#"8844	C:\Users\test\AppData\Local\Programs\Antigravity\resources\bin\language_server.exe --standalone --override_ide_name antigravity --https_server_port 0 --csrf_token aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee --app_data_dir antigravity"#;

        let process = AntigravityProvider::parse_process_info(output).expect(
            "https_server_port 0 without extension_server_port must still detect the process",
        );

        assert_eq!(process.pid, Some(8844));
        assert_eq!(
            process.csrf_token, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "csrf token must be captured"
        );
        assert_eq!(
            process.extension_port, 0,
            "zero means discover via PID listening ports, not a fixed probe base"
        );
    }

    #[test]
    fn parses_equals_form_flags() {
        let output = r"1001	language_server.exe --csrf_token=11111111-2222-3333-4444-555555555555 --https_server_port=0";

        let process = AntigravityProvider::parse_process_info(output).expect("equals-form flags");

        assert_eq!(process.pid, Some(1001));
        assert_eq!(process.csrf_token, "11111111-2222-3333-4444-555555555555");
        assert_eq!(process.extension_port, 0);
    }

    #[test]
    fn prefers_extension_server_port_over_https_port() {
        let output = r"55	language_server.exe --csrf_token 11111111-2222-3333-4444-555555555555 --https_server_port 0 --extension_server_port 54123";

        let process = AntigravityProvider::parse_process_info(output).expect("process info");

        assert_eq!(process.extension_port, 54123);
    }

    #[test]
    fn rejects_csrf_without_port_or_pid() {
        // No PID prefix and no usable port: cannot discover the API endpoint.
        let output = "language_server.exe --csrf_token 11111111-2222-3333-4444-555555555555 --https_server_port 0";

        assert!(AntigravityProvider::parse_process_info(output).is_none());
    }

    /// Older Antigravity CLI (`agy`) versions may omit `--csrf_token`.
    /// Detection must accept the process so PID port enumeration can find the API.
    #[test]
    fn parses_antigravity_cli_agy_without_csrf() {
        let output = r"199364	C:\Users\test\AppData\Local\agy\bin\agy.exe";

        let process = AntigravityProvider::parse_process_info(output)
            .expect("agy CLI without csrf_token must still be detected");

        assert_eq!(process.pid, Some(199364));
        assert!(
            process.csrf_token.is_empty(),
            "keep a tokenless CLI candidate for older versions"
        );
        assert_eq!(process.extension_port, 0);
    }

    #[test]
    fn parses_antigravity_cli_by_path_segment() {
        let output = r"42	/Users/test/.local/bin/agy -p demo --print-timeout 150s";

        let process =
            AntigravityProvider::parse_process_info(output).expect("path-anchored agy CLI");

        assert_eq!(process.pid, Some(42));
        assert!(process.csrf_token.is_empty());
    }

    #[test]
    fn parses_antigravity_cli_package_name() {
        let output =
            r"77	node C:\Users\test\AppData\Roaming\npm\node_modules\antigravity-cli\bin\cli.js";

        let process =
            AntigravityProvider::parse_process_info(output).expect("antigravity-cli package");

        assert_eq!(process.pid, Some(77));
        assert!(process.csrf_token.is_empty());
    }

    /// The detector now emits `<pid>\t<image name>\t<command line>` and matches
    /// the CLI by image name as well, so a running `agy.exe` is still found when
    /// WMI reports an empty `CommandLine`.
    #[test]
    fn agy_image_name_is_enough_when_the_command_line_is_empty() {
        let output = "199364\tagy.exe\t";

        let process = AntigravityProvider::parse_process_info(output)
            .expect("agy.exe must be detected from its image name alone");

        assert_eq!(process.pid, Some(199364));
        assert!(process.csrf_token.is_empty());
        assert_eq!(process.extension_port, 0);
    }

    #[test]
    fn parses_the_three_column_process_line() {
        let output = "45516\tagy.exe\t\"C:\\Users\\test\\AppData\\Local\\agy\\bin\\agy.exe\"";

        let process =
            AntigravityProvider::parse_process_info(output).expect("three-column agy.exe");

        assert_eq!(process.pid, Some(45516));
        assert!(process.csrf_token.is_empty());
    }

    #[test]
    fn a_non_antigravity_image_name_is_not_matched() {
        let output = "123\tchrome.exe\t\"C:\\Program Files\\Google\\Chrome\\chrome.exe\"";

        assert!(AntigravityProvider::parse_process_info(output).is_none());
    }

    #[test]
    fn rejects_false_positive_agy_substrings() {
        // Path-anchored matcher must not treat "legacy" / "imagymagic" as agy.
        assert!(
            AntigravityProvider::parse_process_info(r"1	C:\tools\legacy.exe --https_server_port 0")
                .is_none()
        );
        assert!(
            AntigravityProvider::parse_process_info(r"2	C:\tools\imagymagic.exe --something")
                .is_none()
        );
    }

    #[test]
    fn rejects_shell_scripts_that_merely_mention_agy() {
        // The detector used to match its own PowerShell command line (or any
        // shell that embeds the path in a script), then probe a PID with no
        // language-server ports.
        let output = r#"187720	C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe -Command "Where-Object { $_.Name -eq 'agy.exe' }""#;
        assert!(AntigravityProvider::parse_process_info(output).is_none());
    }

    #[test]
    fn tokenless_ide_language_server_is_skipped() {
        // IDE without CSRF must not be accepted (would shadow a later valid server).
        let output = r"9	C:\Programs\Antigravity\resources\bin\language_server.exe --standalone --override_ide_name antigravity --https_server_port 0";

        assert!(AntigravityProvider::parse_process_info(output).is_none());
    }

    #[test]
    fn cli_is_preferred_when_listed_before_tokenless_ide() {
        let output = "\
9\tC:\\Programs\\Antigravity\\resources\\bin\\language_server.exe --standalone --override_ide_name antigravity --https_server_port 0\n\
199364\tC:\\Users\\test\\AppData\\Local\\agy\\bin\\agy.exe\n";

        let process =
            AntigravityProvider::parse_process_info(output).expect("skip tokenless IDE, take CLI");

        assert_eq!(process.pid, Some(199364));
        assert!(process.csrf_token.is_empty());
    }

    #[test]
    fn parses_complete_agy_tokens_in_quoted_and_equals_forms() {
        for flags in [
            r#"--csrf_token "AbC_token+123/=" --extension_server_csrf_token "EXT_token-456""#,
            "--csrf_token=AbC_token+123/= --extension_server_csrf_token=EXT_token-456",
        ] {
            let output = format!("42\tagy.exe\tagy.exe {flags}");
            let process = AntigravityProvider::parse_process_info(&output).unwrap();
            assert_eq!(process.csrf_token, "AbC_token+123/=");
            assert_eq!(
                process.csrf_tokens(),
                ("EXT_token-456", Some("AbC_token+123/="))
            );
            let description = process.connection_description();
            assert!(description.contains("PID: 42"));
            assert!(description.contains("--extension_server_csrf_token"));
            assert!(!description.contains("AbC_token"));
            assert!(!description.contains("EXT_token-456"));
        }
    }

    #[test]
    fn missing_token_value_does_not_consume_the_next_flag() {
        let process = AntigravityProvider::parse_process_info(
            "42\tagy.exe\tagy.exe --csrf_token --https_server_port 0",
        )
        .unwrap();
        assert!(process.csrf_token.is_empty());
    }

    #[test]
    fn collects_all_candidates_without_mixing_their_credentials() {
        let output = "42\tagy.exe\tagy.exe --csrf_token CLI_token\n\
            43\tlanguage_server.exe\tlanguage_server.exe --csrf_token IDE_token --https_server_port 0";
        let processes = AntigravityProvider::parse_process_infos(output);
        assert_eq!(processes.len(), 2);
        assert!(processes[0].is_cli);
        assert!(!processes[1].is_cli);
        assert_eq!(processes[0].csrf_tokens(), ("CLI_token", None));
        assert_eq!(processes[1].csrf_tokens(), ("IDE_token", None));
    }

    #[test]
    fn cli_without_pid_cannot_send_its_token_to_guessed_ports() {
        let process = AntigravityProvider::parse_process_info(
            "agy.exe --csrf_token private-token --https_server_port 53835",
        )
        .unwrap();
        assert!(AntigravityProvider::api_port_candidates(&process).is_empty());
    }

    #[test]
    fn tokenless_cli_keeps_advertised_window_and_known_fallback_ports() {
        let process =
            AntigravityProvider::parse_process_info("agy.exe --extension_server_port 53830")
                .unwrap();
        assert_eq!(
            AntigravityProvider::api_port_candidates(&process),
            (53830..53850).collect::<Vec<_>>()
        );
        let process = AntigravityProvider::parse_process_info("42\tagy.exe\tagy.exe").unwrap();
        let ports = AntigravityProvider::api_port_candidates(&process);
        for port in [53835, 53836, 53837, 53838, 53845, 53849] {
            assert!(ports.contains(&port));
        }
    }

    #[test]
    fn cli_with_only_extension_token_cannot_probe_guessed_ports() {
        let process = AntigravityProvider::parse_process_info(
            "agy.exe --extension_server_csrf_token private-token --https_server_port 53835",
        )
        .unwrap();
        assert!(AntigravityProvider::api_port_candidates(&process).is_empty());
    }

    #[test]
    fn parses_powershell_new_guid_in_windows_command_line() {
        let output = r#""C:\Users\x\AppData\Local\agy\bin\agy.exe" --csrf_token 1b4e28ba-2fa1-11d2-883f-0016d3cca427"#;
        let process =
            AntigravityProvider::parse_process_info(&format!("42\tagy.exe\t{output}")).unwrap();
        assert!(process.is_cli);
        assert_eq!(process.csrf_token, "1b4e28ba-2fa1-11d2-883f-0016d3cca427");
        assert_eq!(process.csrf_tokens(), (process.csrf_token.as_str(), None));
    }

    fn mock_quota_endpoint(
        server: &mut mockito::ServerGuard,
        method: &str,
        status: usize,
        body: &str,
        token: mockito::Matcher,
    ) -> mockito::Mock {
        server
            .mock(
                "POST",
                format!("/exa.language_server_pb.LanguageServerService/{method}").as_str(),
            )
            .match_header("content-type", "application/json")
            .match_header("connect-protocol-version", "1")
            .match_header("x-codeium-csrf-token", token)
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create()
    }

    #[tokio::test]
    async fn rejected_quota_port_does_not_shadow_a_working_tokenless_cli_port() {
        let mut rejected = mockito::Server::new_async().await;
        let mut working = mockito::Server::new_async().await;
        let mut mocks = Vec::new();
        for method in [
            "GetUnleashData",
            "GetUserStatus",
            "RetrieveUserQuotaSummary",
        ] {
            mocks.push(mock_quota_endpoint(
                &mut rejected,
                method,
                401,
                r#"{"code":"unauthenticated","message":"missing CSRF token"}"#,
                mockito::Matcher::Missing,
            ));
        }
        mocks.push(mock_quota_endpoint(
            &mut working,
            "GetUnleashData",
            200,
            "{}",
            mockito::Matcher::Missing,
        ));
        mocks.push(mock_quota_endpoint(
            &mut working,
            "GetUserStatus",
            200,
            r#"{"userStatus":{"email":"cli@example.test"}}"#,
            mockito::Matcher::Missing,
        ));
        mocks.push(mock_quota_endpoint(&mut working, "RetrieveUserQuotaSummary", 200,
            r#"{"response":{"groups":[{"displayName":"Gemini Models","buckets":[{"bucketId":"gemini-weekly","displayName":"Weekly Limit","window":"weekly","remainingFraction":0.75}]}]}}"#,
            mockito::Matcher::Missing));
        let process = AntigravityProvider::parse_process_info("42\tagy.exe\tagy.exe").unwrap();
        let snapshot = AntigravityProvider::new()
            .fetch_from_candidates(
                &reqwest::Client::new(),
                &process,
                &[rejected.url(), working.url()],
            )
            .await
            .unwrap();
        assert!((snapshot.primary.used_percent - 25.0).abs() < 0.01);
        assert_eq!(snapshot.account_email.as_deref(), Some("cli@example.test"));
        for mock in mocks {
            mock.assert();
        }
    }

    #[tokio::test]
    async fn ide_extension_token_and_language_server_retry_still_work() {
        let mut server = mockito::Server::new_async().await;
        let first = mock_quota_endpoint(
            &mut server,
            "GetUserStatus",
            401,
            r#"{"code":"unauthenticated","message":"invalid CSRF token"}"#,
            mockito::Matcher::Exact("extension-token".into()),
        );
        let retry = mock_quota_endpoint(
            &mut server,
            "GetUserStatus",
            200,
            r#"{"userStatus":{"email":"ide@example.test"}}"#,
            mockito::Matcher::Exact("language-token".into()),
        );
        let process = AntigravityProvider::parse_process_info(
            "43\tlanguage_server.exe\tlanguage_server.exe --csrf_token language-token --extension_server_csrf_token extension-token --https_server_port 0",
        ).unwrap();
        let (primary, alternate) = process.csrf_tokens();
        let response: UserStatusResponse = AntigravityProvider::post_connect_json(
            &reqwest::Client::new(),
            &server.url(),
            "GetUserStatus",
            &serde_json::json!({}),
            primary,
            alternate,
        )
        .await
        .unwrap();
        assert_eq!(
            response.user_status.unwrap().email.as_deref(),
            Some("ide@example.test")
        );
        first.assert();
        retry.assert();
    }

    #[tokio::test]
    async fn csrf_failures_include_safe_connection_context_without_echoing_the_body() {
        let mut server = mockito::Server::new_async().await;
        let mut mocks = Vec::new();
        for method in [
            "GetUnleashData",
            "GetUserStatus",
            "RetrieveUserQuotaSummary",
        ] {
            mocks.push(mock_quota_endpoint(&mut server, method, 401,
                r#"{"code":"unauthenticated","message":"missing CSRF token","private":"secret-value"}"#,
                mockito::Matcher::Missing));
        }
        let process = AntigravityProvider::parse_process_info("42\tagy.exe\tagy.exe").unwrap();
        let error = AntigravityProvider::new()
            .fetch_from_candidates(&reqwest::Client::new(), &process, &[server.url()])
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("HTTP 401 (missing CSRF token)"));
        assert!(error.contains("agy/Antigravity CLI; PID: 42; CSRF source: none (tokenless CLI)"));
        assert!(error.contains(&server.url()));
        assert!(!error.contains("secret-value"));
        let actionable =
            AntigravityProvider::user_facing_failure(&process, ProviderError::Other(error));
        assert!(
            matches!(&actionable, ProviderError::OAuth(message) if message == CLI_TOKEN_REQUIRED_MESSAGE)
        );
        // Tauri forwards this redacted Display text to its provider error UI.
        assert_eq!(
            crate::logging::safe_error_message(actionable),
            format!("OAuth error: {CLI_TOKEN_REQUIRED_MESSAGE}")
        );
        for mock in mocks {
            mock.assert();
        }
    }

    #[test]
    fn cli_token_guidance_does_not_replace_unrelated_or_credentialed_failures() {
        for (command, reason) in [
            ("agy.exe", "HTTP 401 (invalid CSRF token)"),
            ("agy.exe", "HTTP 500"),
            (
                "agy.exe --csrf_token private-token",
                "HTTP 401 (missing CSRF token)",
            ),
            (
                "agy.exe --extension_server_csrf_token private-token",
                "HTTP 401 (missing CSRF token)",
            ),
            (
                "language_server.exe --csrf_token private-token",
                "HTTP 401 (missing CSRF token)",
            ),
        ] {
            let process =
                AntigravityProvider::parse_process_info(&format!("42\t{command}")).unwrap();
            let error = AntigravityProvider::user_facing_failure(
                &process,
                ProviderError::Other(reason.into()),
            );
            assert!(matches!(error, ProviderError::Other(message) if message == reason));
        }
    }

    fn make_response(models: Vec<(&str, f64)>) -> UserStatusResponse {
        let json = serde_json::json!({
            "userStatus": {
                "cascadeModelConfigData": {
                    "clientModelConfigs": models.iter().map(|(label, remaining)| {
                        serde_json::json!({
                            "label": label,
                            "quotaInfo": {
                                "remainingFraction": remaining
                            }
                        })
                    }).collect::<Vec<_>>()
                }
            }
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn test_parse_user_status_standard() {
        let resp = make_response(vec![
            ("Claude 3.5 Sonnet", 0.8),
            ("Gemini 2.5 Pro Low", 0.5),
            ("Gemini 2.5 Flash", 0.9),
        ]);
        let provider = AntigravityProvider::new();
        let snap = provider.parse_user_status(resp).unwrap();

        assert!((snap.primary.used_percent - 20.0).abs() < 0.1);
        let sec = snap.secondary.unwrap();
        assert!((sec.used_percent - 50.0).abs() < 0.1);
        let ter = snap.model_specific.unwrap();
        assert!((ter.used_percent - 10.0).abs() < 0.1);
        assert_eq!(snap.extra_rate_windows.len(), 3);
        assert!(
            snap.extra_rate_windows
                .iter()
                .any(|window| window.title == "Gemini 2.5 Flash")
        );
    }

    #[test]
    fn test_parse_user_status_thinking_skipped() {
        let resp = make_response(vec![
            ("Claude Thinking", 0.6),
            ("Claude 3.5 Sonnet", 0.7),
            ("Gemini 2.5 Flash", 0.5),
        ]);
        let provider = AntigravityProvider::new();
        let snap = provider.parse_user_status(resp).unwrap();

        assert!((snap.primary.used_percent - 30.0).abs() < 0.1);
    }

    #[test]
    fn test_parse_user_status_fallback_first() {
        let resp = make_response(vec![("GPT-4o", 0.4), ("Mistral Large", 0.6)]);
        let provider = AntigravityProvider::new();
        let snap = provider.parse_user_status(resp).unwrap();

        assert!((snap.primary.used_percent - 60.0).abs() < 0.1);
        assert!(snap.secondary.is_none());
        assert!(snap.model_specific.is_none());
    }

    #[test]
    fn test_noisy_models_do_not_drive_summary_windows() {
        let resp = make_response(vec![
            ("Gemini 2.5 Flash Image", 0.01),
            ("Gemini 2.5 Pro Lite", 0.02),
            ("Gemini autocomplete internal", 0.03),
            ("Claude 4 Sonnet", 0.8),
            ("Gemini 2.5 Pro Low", 0.6),
            ("Gemini 2.5 Flash", 0.7),
        ]);
        let provider = AntigravityProvider::new();
        let snap = provider.parse_user_status(resp).unwrap();

        assert!((snap.primary.used_percent - 20.0).abs() < 0.1);
        assert!((snap.secondary.unwrap().used_percent - 40.0).abs() < 0.1);
        assert!((snap.model_specific.unwrap().used_percent - 30.0).abs() < 0.1);
        assert!(
            snap.extra_rate_windows
                .iter()
                .any(|window| window.title == "Gemini 2.5 Flash Image")
        );
    }

    #[test]
    fn not_running_error_tells_user_how_to_start() {
        let error = ProviderError::NotInstalled(NOT_RUNNING_MESSAGE.to_string()).to_string();

        assert!(error.contains("Start Google Antigravity or run `agy`"));
        assert!(error.contains("sign in"));
    }

    fn make_quota_summary(groups: serde_json::Value) -> QuotaSummaryResponse {
        serde_json::from_value(serde_json::json!({ "response": { "groups": groups } })).unwrap()
    }

    #[test]
    fn parse_quota_summary_matches_settings_group_pools() {
        // Live shape from RetrieveUserQuotaSummary (weekly-only Pro seat).
        let summary = make_quota_summary(serde_json::json!([
            {
                "displayName": "Gemini Models",
                "description": "Models within this group: Gemini Flash, Gemini Pro",
                "buckets": [{
                    "bucketId": "gemini-weekly",
                    "displayName": "Weekly Limit",
                    "window": "weekly",
                    "remainingFraction": 0.75,
                    "resetTime": "2026-08-03T22:52:04Z"
                }]
            },
            {
                "displayName": "Claude and GPT models",
                "description": "Models within this group: Claude Opus, Claude Sonnet, GPT-OSS",
                "buckets": [{
                    "bucketId": "3p-weekly",
                    "displayName": "Weekly Limit",
                    "window": "weekly",
                    "remainingFraction": 0.59,
                    "resetTime": "2026-08-03T22:52:04Z"
                }]
            }
        ]));

        let snap = parse_quota_summary(&summary).expect("summary");
        // Primary = Claude/GPT weekly used% = (1 - 0.59) * 100
        assert!((snap.primary.used_percent - 41.0).abs() < 0.1);
        let sec = snap.secondary.expect("gemini secondary");
        assert!((sec.used_percent - 25.0).abs() < 0.1);
        assert_eq!(snap.extra_rate_windows.len(), 2);
        assert!(
            snap.extra_rate_windows
                .iter()
                .any(|w| w.title.contains("Claude and GPT") && w.title.contains("Weekly"))
        );
        assert!(
            snap.extra_rate_windows
                .iter()
                .any(|w| w.title.contains("Gemini") && w.title.contains("Weekly"))
        );
    }

    #[test]
    fn parse_quota_summary_prefers_five_hour_over_weekly_for_primary() {
        // Matches AG Settings dual rows (reporter screenshot shape).
        let summary = make_quota_summary(serde_json::json!([
            {
                "displayName": "Gemini Models",
                "buckets": [
                    {
                        "bucketId": "gemini-weekly",
                        "displayName": "Weekly Limit",
                        "window": "weekly",
                        "remainingFraction": 0.75
                    },
                    {
                        "bucketId": "gemini-five-hour",
                        "displayName": "Five Hour Limit",
                        "window": "five_hour",
                        "remainingFraction": 0.98
                    }
                ]
            },
            {
                "displayName": "Claude and GPT models",
                "buckets": [
                    {
                        "bucketId": "3p-weekly",
                        "displayName": "Weekly Limit",
                        "window": "weekly",
                        "remainingFraction": 0.59
                    },
                    {
                        "bucketId": "3p-five-hour",
                        "displayName": "Five Hour Limit",
                        "window": "five_hour",
                        "remainingFraction": 1.0
                    }
                ]
            }
        ]));

        let snap = parse_quota_summary(&summary).expect("summary");
        // Claude five-hour remaining 1.0 → 0% used
        assert!((snap.primary.used_percent - 0.0).abs() < 0.1);
        // Gemini five-hour remaining 0.98 → 2% used
        let sec = snap.secondary.expect("gemini");
        assert!((sec.used_percent - 2.0).abs() < 0.1);
        // Claude weekly also exposed when five-hour is primary
        let weekly = snap.model_specific.expect("claude weekly");
        assert!((weekly.used_percent - 41.0).abs() < 0.1);
        assert_eq!(snap.extra_rate_windows.len(), 4);
    }

    #[test]
    fn parse_quota_summary_empty_groups_returns_none() {
        let summary = make_quota_summary(serde_json::json!([]));
        assert!(parse_quota_summary(&summary).is_none());
    }

    #[test]
    fn a_reset_timestamp_lands_in_resets_at_not_in_the_description() {
        // #412: the language server sends RFC 3339, and every surface formats
        // `resets_at`. Passing it as the description made the panel print the
        // raw ISO string instead.
        let window =
            rate_window_from_remaining(Some(0.5), Some("2026-09-16T22:33:14Z".to_string()));
        assert_eq!(
            window.resets_at,
            Some("2026-09-16T22:33:14Z".parse::<DateTime<Utc>>().unwrap())
        );
        assert_eq!(window.reset_description, None);
    }

    #[test]
    fn a_reset_time_in_an_unexpected_shape_is_still_shown() {
        let window = rate_window_from_remaining(Some(0.5), Some("in a while".to_string()));
        assert_eq!(window.resets_at, None);
        assert_eq!(window.reset_description.as_deref(), Some("in a while"));
    }

    #[test]
    fn a_failure_reason_carries_the_status_but_never_the_response_body() {
        let http = ProviderError::Other(
            "API error 401 on GetUserStatus: {\"detail\":\"account 42\"}".to_string(),
        );
        assert_eq!(short_reason(&http), "HTTP 401");
        assert!(!short_reason(&http).contains("account"));
        assert_eq!(
            short_reason(&ProviderError::Other(
                "API request failed: error sending request".to_string()
            )),
            "request failed"
        );
        assert_eq!(
            short_reason(&ProviderError::Parse("expected value".to_string())),
            "unparseable response"
        );
        assert_eq!(
            short_reason(&ProviderError::Other(
                "Failed to parse GetUserStatus: expected value".to_string()
            )),
            "unparseable response"
        );
    }

    #[test]
    fn the_message_names_whichever_call_failed() {
        assert_eq!(
            describe_unusable(Some("HTTP 401"), Some("HTTP 401")),
            "user status: HTTP 401; quota summary: HTTP 401"
        );
        assert_eq!(
            describe_unusable(Some("HTTP 500"), None),
            "user status: HTTP 500"
        );
        assert_eq!(
            describe_unusable(None, Some("empty response")),
            "quota summary: empty response"
        );
        assert_eq!(describe_unusable(None, None), "no detail available");
    }
}
