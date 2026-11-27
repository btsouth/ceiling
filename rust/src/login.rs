//! Login flow runners for various providers
//!
//! Runs CLI login commands and captures output/URLs

#![allow(dead_code)]

use regex_lite::Regex;
use std::io::{BufRead, BufReader, Read};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const CLAUDE_LOGIN_ARGS: &[&str] = &["auth", "login"];
const CODEX_LOGIN_ARGS: &[&str] = &["login", "--device-auth"];
const COPILOT_LOGIN_ARGS: &[&str] = &["auth", "login", "-w"];

/// Hosts each CLI is expected to print for sign-in. Ceiling only opens or
/// reports an HTTPS URL on one of these exact hosts, so CLI output (or
/// anything a compromised shim prints) cannot steer the browser elsewhere.
const CLAUDE_AUTH_HOSTS: &[&str] = &[
    "claude.ai",
    "claude.com",
    "platform.claude.com",
    "console.anthropic.com",
];
const CODEX_AUTH_HOSTS: &[&str] = &["auth.openai.com"];
const COPILOT_AUTH_HOSTS: &[&str] = &["github.com"];

/// Result of a login attempt
#[derive(Debug, Clone)]
pub struct LoginResult {
    pub outcome: LoginOutcome,
    pub output: String,
    pub auth_link: Option<String>,
}

/// Outcome of login attempt
#[derive(Debug, Clone)]
pub enum LoginOutcome {
    Success,
    TimedOut,
    Failed { status: i32 },
    MissingBinary,
    LaunchFailed(String),
}

/// Phase of the login process
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LoginPhase {
    Idle,
    Requesting,
    WaitingBrowser,
    Complete,
}

/// How a CLI login flow gets the user into a browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserOpenMode {
    /// The CLI opens the browser itself. Any URL it prints is only a manual
    /// fallback, so Ceiling records it but never opens it.
    CliManaged,
    /// The CLI prints a verification URL and expects the user to open it, so
    /// Ceiling opens the first printed URL once.
    OpenPrintedUrl,
}

/// Static description of one CLI login flow.
#[derive(Debug, Clone, Copy)]
struct CliLoginSpec {
    binary: &'static str,
    args: &'static [&'static str],
    success_markers: &'static [&'static str],
    browser_mode: BrowserOpenMode,
    auth_hosts: &'static [&'static str],
}

const COMMON_SUCCESS_MARKERS: &[&str] = &[
    "Successfully logged in",
    "Login successful",
    "Logged in successfully",
];

/// `claude auth login` starts a localhost callback server and opens the
/// browser to it on its own. The URL it prints to stdout is the manual
/// copy-paste fallback, which Ceiling has no way to accept, so opening it
/// would hijack the automatic flow.
fn claude_login_spec() -> CliLoginSpec {
    CliLoginSpec {
        binary: "claude",
        args: CLAUDE_LOGIN_ARGS,
        success_markers: COMMON_SUCCESS_MARKERS,
        browser_mode: BrowserOpenMode::CliManaged,
        auth_hosts: CLAUDE_AUTH_HOSTS,
    }
}

/// `codex login --device-auth` prints a device verification URL.
fn codex_login_spec() -> CliLoginSpec {
    CliLoginSpec {
        binary: "codex",
        args: CODEX_LOGIN_ARGS,
        success_markers: COMMON_SUCCESS_MARKERS,
        browser_mode: BrowserOpenMode::OpenPrintedUrl,
        auth_hosts: CODEX_AUTH_HOSTS,
    }
}

/// `gh auth login -w` prints a device verification URL.
fn copilot_login_spec() -> CliLoginSpec {
    CliLoginSpec {
        binary: "gh",
        args: COPILOT_LOGIN_ARGS,
        success_markers: &["Logged in as", "Authentication complete"],
        browser_mode: BrowserOpenMode::OpenPrintedUrl,
        auth_hosts: COPILOT_AUTH_HOSTS,
    }
}

/// Run Claude CLI login. The CLI opens its own browser tab; Ceiling only
/// reports progress and records any fallback URL it prints.
pub fn run_claude_login<F>(timeout_secs: u64, on_phase: F) -> LoginResult
where
    F: Fn(LoginPhase) + Send + 'static,
{
    run_cli_login(claude_login_spec(), timeout_secs, on_phase)
}

/// Run Codex CLI device login, opening the printed verification URL.
pub fn run_codex_login<F>(timeout_secs: u64, on_phase: F) -> LoginResult
where
    F: Fn(LoginPhase) + Send + 'static,
{
    run_cli_login(codex_login_spec(), timeout_secs, on_phase)
}

/// Run Copilot/GitHub device flow login, opening the printed verification URL.
pub fn run_copilot_login<F>(timeout_secs: u64, on_phase: F) -> LoginResult
where
    F: Fn(LoginPhase) + Send + 'static,
{
    run_cli_login(copilot_login_spec(), timeout_secs, on_phase)
}

/// Generic CLI login runner
fn run_cli_login<F>(spec: CliLoginSpec, timeout_secs: u64, on_phase: F) -> LoginResult
where
    F: Fn(LoginPhase) + Send + 'static,
{
    let binary_path = match which::which(spec.binary) {
        Ok(p) => p,
        Err(_) => return missing_binary_result(spec.binary),
    };

    on_phase(LoginPhase::Requesting);

    let child = match spawn_login_process(binary_path.as_path(), spec.args) {
        Ok(c) => c,
        Err(e) => return launch_failed_result(e),
    };

    let open_url = |url: &str| {
        let _ = open::that(url);
    };
    let state = CliLoginState::new(
        timeout_secs,
        &on_phase,
        spec.success_markers,
        spec.browser_mode,
        spec.auth_hosts,
        &open_url,
    );

    monitor_login_process(child, state, &on_phase)
}

fn missing_binary_result(binary: &str) -> LoginResult {
    LoginResult {
        outcome: LoginOutcome::MissingBinary,
        output: format!("{} not found in PATH", binary),
        auth_link: None,
    }
}

fn launch_failed_result(error: String) -> LoginResult {
    LoginResult {
        outcome: LoginOutcome::LaunchFailed(error),
        output: String::new(),
        auth_link: None,
    }
}

fn spawn_login_process(binary_path: &std::path::Path, args: &[&str]) -> Result<Child, String> {
    #[cfg(windows)]
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let mut cmd = Command::new(binary_path);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    cmd.spawn().map_err(|e| e.to_string())
}

struct CliLoginState<'a, F>
where
    F: Fn(LoginPhase),
{
    output: String,
    auth_link: Option<String>,
    url_regex: Regex,
    on_phase: &'a F,
    success_markers: &'a [&'a str],
    browser_mode: BrowserOpenMode,
    auth_hosts: &'a [&'a str],
    open_url: &'a dyn Fn(&str),
    waiting_browser_emitted: bool,
    start: Instant,
    timeout: Duration,
}

impl<'a, F> CliLoginState<'a, F>
where
    F: Fn(LoginPhase),
{
    fn new(
        timeout_secs: u64,
        on_phase: &'a F,
        success_markers: &'a [&'a str],
        browser_mode: BrowserOpenMode,
        auth_hosts: &'a [&'a str],
        open_url: &'a dyn Fn(&str),
    ) -> Self {
        Self {
            output: String::new(),
            auth_link: None,
            url_regex: Regex::new(r"https?://[A-Za-z0-9._~:/?#\[\]@!$&'()*+,;=%-]+").unwrap(),
            on_phase,
            success_markers,
            browser_mode,
            auth_hosts,
            open_url,
            waiting_browser_emitted: false,
            start: Instant::now(),
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    fn handle_line(&mut self, line: &str) -> Option<LoginOutcome> {
        self.output.push_str(line);
        self.output.push('\n');
        self.capture_auth_link(line);

        if self
            .success_markers
            .iter()
            .any(|marker| line.contains(marker))
        {
            (self.on_phase)(LoginPhase::Complete);
            return Some(LoginOutcome::Success);
        }

        self.start
            .elapsed()
            .gt(&self.timeout)
            .then_some(LoginOutcome::TimedOut)
    }

    fn capture_auth_link(&mut self, line: &str) {
        if self.browser_mode == BrowserOpenMode::CliManaged
            && line.to_lowercase().contains("opening browser")
        {
            self.emit_waiting_browser();
        }

        if self.auth_link.is_some() {
            return;
        }

        let Some(m) = self.url_regex.find(line) else {
            return;
        };

        let url = m.as_str().to_string();
        if !is_allowed_auth_url(&url, self.auth_hosts) {
            tracing::debug!("Ignoring login URL outside the expected sign-in hosts");
            return;
        }
        self.emit_waiting_browser();
        if self.browser_mode == BrowserOpenMode::OpenPrintedUrl {
            (self.open_url)(&url);
        }
        self.auth_link = Some(url);
    }

    fn emit_waiting_browser(&mut self) {
        if !self.waiting_browser_emitted {
            self.waiting_browser_emitted = true;
            (self.on_phase)(LoginPhase::WaitingBrowser);
        }
    }

    fn into_result(self, outcome: LoginOutcome) -> LoginResult {
        LoginResult {
            outcome,
            output: self.output,
            auth_link: self.auth_link,
        }
    }
}

fn forward_login_stream<R>(stream: Option<R>, sender: mpsc::Sender<String>)
where
    R: Read + Send + 'static,
{
    let Some(stream) = stream else {
        return;
    };
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
}

fn monitor_login_process<F>(
    mut child: Child,
    mut state: CliLoginState<'_, F>,
    on_phase: &F,
) -> LoginResult
where
    F: Fn(LoginPhase),
{
    const POLL_INTERVAL: Duration = Duration::from_millis(100);

    let (sender, receiver) = mpsc::channel();
    forward_login_stream(child.stdout.take(), sender.clone());
    forward_login_stream(child.stderr.take(), sender);

    loop {
        if state.start.elapsed() >= state.timeout {
            return stop_child_with_outcome(&mut child, state, LoginOutcome::TimedOut);
        }

        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(line) => {
                if let Some(outcome) = state.handle_line(&line) {
                    return stop_child_with_outcome(&mut child, state, outcome);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => thread::sleep(POLL_INTERVAL),
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                for line in receiver.try_iter() {
                    if let Some(outcome) = state.handle_line(&line) {
                        return state.into_result(outcome);
                    }
                }
                if status.success() {
                    on_phase(LoginPhase::Complete);
                    return state.into_result(LoginOutcome::Success);
                }
                return state.into_result(LoginOutcome::Failed {
                    status: status.code().unwrap_or(-1),
                });
            }
            Ok(None) => {}
            Err(error) => {
                return stop_child_with_outcome(
                    &mut child,
                    state,
                    LoginOutcome::LaunchFailed(error.to_string()),
                );
            }
        }
    }
}

fn stop_child_with_outcome<F>(
    child: &mut Child,
    state: CliLoginState<'_, F>,
    outcome: LoginOutcome,
) -> LoginResult
where
    F: Fn(LoginPhase),
{
    let _ = child.kill();
    let _ = child.wait();
    state.into_result(outcome)
}

/// Whether `url` is HTTPS on exactly one of `allowed_hosts`, with no
/// userinfo or explicit port.
fn is_allowed_auth_url(url: &str, allowed_hosts: &[&str]) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let Some(url::Host::Domain(host)) = parsed.host() else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.port().is_none()
        && allowed_hosts
            .iter()
            .any(|allowed| host.eq_ignore_ascii_case(allowed))
}

/// Open a provider sign-in URL in the default browser.
///
/// Only HTTPS URLs on the known Claude, OpenAI and GitHub sign-in hosts are
/// opened.
pub fn open_auth_url(url: &str) -> anyhow::Result<()> {
    let allowed = [CLAUDE_AUTH_HOSTS, CODEX_AUTH_HOSTS, COPILOT_AUTH_HOSTS]
        .iter()
        .any(|hosts| is_allowed_auth_url(url, hosts));
    if !allowed {
        anyhow::bail!("Refusing to open an unexpected sign-in URL");
    }
    open::that(url)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Lines printed by `claude auth login` (Claude Code 2.1.332).
    const CLAUDE_AUTH_LOGIN_OUTPUT: &[&str] = &[
        "Opening browser to sign in…",
        "If the browser didn't open, visit:",
        "https://claude.ai/oauth/authorize?z-AbC123_xyz",
        "Paste code here if prompted >",
    ];

    #[test]
    fn login_commands_match_current_dedicated_auth_surfaces() {
        assert_eq!(CLAUDE_LOGIN_ARGS, ["auth", "login"]);
        assert_eq!(CODEX_LOGIN_ARGS, ["login", "--device-auth"]);
        assert_eq!(COPILOT_LOGIN_ARGS, ["auth", "login", "-w"]);
    }

    #[test]
    fn claude_login_lets_the_cli_open_its_own_browser() {
        let spec = claude_login_spec();
        assert_eq!(spec.binary, "claude");
        assert_eq!(spec.args, CLAUDE_LOGIN_ARGS);
        assert_eq!(spec.browser_mode, BrowserOpenMode::CliManaged);
    }

    #[test]
    fn device_auth_logins_open_the_printed_url() {
        let codex = codex_login_spec();
        assert_eq!(codex.binary, "codex");
        assert_eq!(codex.args, CODEX_LOGIN_ARGS);
        assert_eq!(codex.browser_mode, BrowserOpenMode::OpenPrintedUrl);

        let copilot = copilot_login_spec();
        assert_eq!(copilot.binary, "gh");
        assert_eq!(copilot.args, COPILOT_LOGIN_ARGS);
        assert_eq!(copilot.browser_mode, BrowserOpenMode::OpenPrintedUrl);
    }

    #[test]
    fn cli_managed_mode_never_opens_the_fallback_url() {
        let phases = RefCell::new(Vec::new());
        let opened = RefCell::new(Vec::<String>::new());
        let on_phase = |phase| phases.borrow_mut().push(phase);
        let open_url = |url: &str| opened.borrow_mut().push(url.to_string());
        let spec = claude_login_spec();
        let mut state = CliLoginState::new(
            30,
            &on_phase,
            spec.success_markers,
            spec.browser_mode,
            spec.auth_hosts,
            &open_url,
        );

        for line in CLAUDE_AUTH_LOGIN_OUTPUT {
            assert!(state.handle_line(line).is_none());
        }

        assert_eq!(*phases.borrow(), vec![LoginPhase::WaitingBrowser]);
        assert!(opened.borrow().is_empty());
        let result = state.into_result(LoginOutcome::TimedOut);
        assert_eq!(
            result.auth_link.as_deref(),
            Some("https://claude.ai/oauth/authorize?z-AbC123_xyz")
        );
    }

    #[test]
    fn cli_managed_mode_reports_waiting_from_a_url_alone() {
        let phases = RefCell::new(Vec::new());
        let opened = RefCell::new(Vec::<String>::new());
        let on_phase = |phase| phases.borrow_mut().push(phase);
        let open_url = |url: &str| opened.borrow_mut().push(url.to_string());
        let mut state = CliLoginState::new(
            30,
            &on_phase,
            &["Login successful"],
            BrowserOpenMode::CliManaged,
            CLAUDE_AUTH_HOSTS,
            &open_url,
        );

        state.handle_line("Visit https://claude.ai/oauth/authorize?z-1");
        state.handle_line("OPENING BROWSER again");

        assert_eq!(*phases.borrow(), vec![LoginPhase::WaitingBrowser]);
        assert!(opened.borrow().is_empty());
        let result = state.into_result(LoginOutcome::TimedOut);
        assert_eq!(
            result.auth_link.as_deref(),
            Some("https://claude.ai/oauth/authorize?z-1")
        );
    }

    #[test]
    fn open_printed_url_mode_opens_the_first_url_once() {
        let phases = RefCell::new(Vec::new());
        let opened = RefCell::new(Vec::<String>::new());
        let on_phase = |phase| phases.borrow_mut().push(phase);
        let open_url = |url: &str| opened.borrow_mut().push(url.to_string());
        let spec = codex_login_spec();
        let mut state = CliLoginState::new(
            30,
            &on_phase,
            spec.success_markers,
            spec.browser_mode,
            spec.auth_hosts,
            &open_url,
        );

        state.handle_line("Follow these steps to sign in:");
        state.handle_line("1. Open https://auth.openai.com/codex/device in your browser");
        state.handle_line("2. Enter this code: ABCD-1234");
        state.handle_line("Or visit https://example.com/other");

        assert_eq!(*phases.borrow(), vec![LoginPhase::WaitingBrowser]);
        assert_eq!(
            *opened.borrow(),
            vec!["https://auth.openai.com/codex/device".to_string()]
        );
        let result = state.into_result(LoginOutcome::TimedOut);
        assert_eq!(
            result.auth_link.as_deref(),
            Some("https://auth.openai.com/codex/device")
        );
    }

    #[test]
    fn open_printed_url_mode_waits_for_a_url_before_reporting() {
        let phases = RefCell::new(Vec::new());
        let opened = RefCell::new(Vec::<String>::new());
        let on_phase = |phase| phases.borrow_mut().push(phase);
        let open_url = |url: &str| opened.borrow_mut().push(url.to_string());
        let mut state = CliLoginState::new(
            30,
            &on_phase,
            &["Logged in as"],
            BrowserOpenMode::OpenPrintedUrl,
            COPILOT_AUTH_HOSTS,
            &open_url,
        );

        state.handle_line("Opening browser to complete login");

        assert!(phases.borrow().is_empty());
        assert!(opened.borrow().is_empty());
    }

    #[test]
    fn success_marker_completes_and_notifies_the_caller() {
        let phases = RefCell::new(Vec::new());
        let on_phase = |phase| phases.borrow_mut().push(phase);
        let open_url = |_: &str| {};
        let mut state = CliLoginState::new(
            30,
            &on_phase,
            &["Login successful"],
            BrowserOpenMode::OpenPrintedUrl,
            CODEX_AUTH_HOSTS,
            &open_url,
        );

        let outcome = state.handle_line("Login successful");

        assert!(matches!(outcome, Some(LoginOutcome::Success)));
        assert_eq!(*phases.borrow(), vec![LoginPhase::Complete]);
    }

    #[test]
    fn auth_url_allowlist_accepts_exact_https_sign_in_hosts() {
        assert!(is_allowed_auth_url(
            "https://auth.openai.com/codex/device",
            CODEX_AUTH_HOSTS
        ));
        assert!(is_allowed_auth_url(
            "https://github.com/login/device",
            COPILOT_AUTH_HOSTS
        ));
        assert!(is_allowed_auth_url(
            "https://GitHub.com/login/device",
            COPILOT_AUTH_HOSTS
        ));
        assert!(is_allowed_auth_url(
            "https://claude.ai/oauth/authorize?code=true",
            CLAUDE_AUTH_HOSTS
        ));
        assert!(is_allowed_auth_url(
            "https://platform.claude.com/oauth/authorize",
            CLAUDE_AUTH_HOSTS
        ));
    }

    #[test]
    fn auth_url_allowlist_rejects_lookalikes_and_other_hosts() {
        for url in [
            "http://github.com/login/device",
            "https://github.com.evil.test/login/device",
            "https://evilgithub.com/login/device",
            "https://gist.github.com/login/device",
            "https://github.com@evil.test/login/device",
            "https://user@github.com/login/device",
            "https://github.com:8443/login/device",
            "https://140.82.112.3/login/device",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(!is_allowed_auth_url(url, COPILOT_AUTH_HOSTS), "{url}");
        }
        assert!(!is_allowed_auth_url(
            "https://github.com/login/device",
            CODEX_AUTH_HOSTS
        ));
    }

    #[test]
    fn open_printed_url_mode_skips_urls_outside_the_allowlist() {
        let phases = RefCell::new(Vec::new());
        let opened = RefCell::new(Vec::<String>::new());
        let on_phase = |phase| phases.borrow_mut().push(phase);
        let open_url = |url: &str| opened.borrow_mut().push(url.to_string());
        let spec = copilot_login_spec();
        let mut state = CliLoginState::new(
            30,
            &on_phase,
            spec.success_markers,
            spec.browser_mode,
            spec.auth_hosts,
            &open_url,
        );

        state.handle_line("See https://evil.test/login/device for help");
        assert!(phases.borrow().is_empty());
        assert!(opened.borrow().is_empty());

        state.handle_line("Open https://github.com/login/device in your browser");
        assert_eq!(*phases.borrow(), vec![LoginPhase::WaitingBrowser]);
        assert_eq!(
            *opened.borrow(),
            vec!["https://github.com/login/device".to_string()]
        );
        let result = state.into_result(LoginOutcome::TimedOut);
        assert_eq!(
            result.auth_link.as_deref(),
            Some("https://github.com/login/device")
        );
    }

    #[test]
    fn open_auth_url_refuses_unexpected_hosts() {
        let error = open_auth_url("https://evil.test/oauth").expect_err("must refuse");
        assert!(error.to_string().contains("unexpected sign-in URL"));
    }
}
