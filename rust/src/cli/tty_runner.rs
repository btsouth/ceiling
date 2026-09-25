//! TTY Command Runner
//!
//! Executes interactive CLI commands using the platform pseudo-console.
//! Provides PTY-like functionality for capturing output from interactive TUI programs.

#![allow(dead_code)]

use regex_lite::Regex;
use std::collections::HashMap;
use std::io::{Read, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use thiserror::Error;

/// How long to wait after the configured initial delay for a PTY child to
/// produce its first output before typing the script into it. Without this the
/// script could be written before Windows ConPTY attached its reader and the
/// keystrokes were discarded.
const READY_GRACE: Duration = Duration::from_secs(2);

/// The ConPTY cursor-position query (`CSI 6 n`) some Windows shells issue on
/// startup and block on until the terminal answers.
const DEVICE_STATUS_QUERY: &str = "\x1b[6n";
/// Longest suffix of a partial query that has to be carried into the next PTY
/// read so a query split across a read boundary is still seen.
const DEVICE_STATUS_QUERY_MAX_SPLIT: usize = DEVICE_STATUS_QUERY.len() - 1;
/// The answer, for a cursor at the home position.
const DEVICE_STATUS_REPLY: &str = "\x1b[1;1R";

/// Detects device-status queries that may straddle a PTY read boundary.
///
/// A ConPTY read is a byte-count slice, not a line, so the seven-byte query can
/// arrive as `"\x1b[6"` then `"n"`. Checking one chunk in isolation misses it,
/// the shell stays blocked, and it never consumes the scripted input. This
/// keeps just enough of the previous chunk to see a split query and counts each
/// occurrence exactly once.
#[derive(Default)]
struct DeviceStatusWatcher {
    carry: String,
}

impl DeviceStatusWatcher {
    /// Feed one read and return how many queries it completed.
    fn observe(&mut self, chunk: &str) -> usize {
        self.carry.push_str(chunk);
        let mut completed = 0;
        while let Some(at) = self.carry.find(DEVICE_STATUS_QUERY) {
            completed += 1;
            self.carry.drain(..at + DEVICE_STATUS_QUERY.len());
        }
        // Keep only a tail that could still be the start of a query; anything
        // longer cannot become one.
        if self.carry.len() > DEVICE_STATUS_QUERY_MAX_SPLIT {
            let cut = self.carry.len() - DEVICE_STATUS_QUERY_MAX_SPLIT;
            let mut cut = cut.min(self.carry.len());
            while cut > 0 && !self.carry.is_char_boundary(cut) {
                cut -= 1;
            }
            self.carry.drain(..cut);
        }
        completed
    }
}

/// Answer every ConPTY cursor-position query the watcher has completed,
/// including ones split across reads.
fn answer_device_status_report(
    watcher: &mut DeviceStatusWatcher,
    writer: &mut dyn Write,
    chunk: &str,
) {
    for _ in 0..watcher.observe(chunk) {
        let _ = write!(writer, "{DEVICE_STATUS_REPLY}");
    }
    let _ = writer.flush();
}

/// Result of running a TTY command
#[derive(Debug, Clone)]
pub struct TtyCommandResult {
    /// Captured output text
    pub text: String,
    /// Whether the command was interrupted early
    pub stopped_early: bool,
    /// URLs detected in output (if stop_on_url was set)
    pub detected_urls: Vec<String>,
}

impl TtyCommandResult {
    /// Extract the first URL found in the output
    pub fn first_url(&self) -> Option<&str> {
        self.detected_urls.first().map(|s| s.as_str())
    }
}

/// Options for running TTY commands
#[derive(Debug, Clone)]
pub struct TtyCommandOptions {
    /// Terminal rows (default: 50)
    pub rows: u16,
    /// Terminal columns (default: 160)
    pub cols: u16,
    /// Overall timeout in seconds (default: 20)
    pub timeout_secs: f64,
    /// Idle timeout - stop if no output for this duration (optional)
    pub idle_timeout_secs: Option<f64>,
    /// Working directory
    pub working_directory: Option<PathBuf>,
    /// Extra arguments to pass to the command
    pub extra_args: Vec<String>,
    /// Initial delay before sending script (default: 0.4s)
    pub initial_delay_secs: f64,
    /// Delay between script characters (default: 0s)
    pub script_char_delay_secs: f64,
    /// Delay between script lines (default: 0s)
    pub script_line_delay_secs: f64,
    /// Send enter/return every N seconds (optional)
    pub send_enter_every_secs: Option<f64>,
    /// Map of substrings to keys to send when detected
    pub send_on_substrings: HashMap<String, String>,
    /// Stop early when a URL is detected
    pub stop_on_url: bool,
    /// Stop early when any of these substrings are detected
    pub stop_on_substrings: Vec<String>,
    /// Settle time after stopping (default: 0.25s)
    pub settle_after_stop_secs: f64,
    /// Environment variables to set
    pub env: HashMap<String, String>,
}

impl Default for TtyCommandOptions {
    fn default() -> Self {
        Self {
            rows: 50,
            cols: 160,
            timeout_secs: 20.0,
            idle_timeout_secs: None,
            working_directory: None,
            extra_args: Vec::new(),
            initial_delay_secs: 0.4,
            script_char_delay_secs: 0.0,
            script_line_delay_secs: 0.0,
            send_enter_every_secs: None,
            send_on_substrings: HashMap::new(),
            stop_on_url: false,
            stop_on_substrings: Vec::new(),
            settle_after_stop_secs: 0.25,
            env: HashMap::new(),
        }
    }
}

impl TtyCommandOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_timeout(mut self, secs: f64) -> Self {
        self.timeout_secs = secs;
        self
    }

    pub fn with_idle_timeout(mut self, secs: f64) -> Self {
        self.idle_timeout_secs = Some(secs);
        self
    }

    pub fn with_initial_delay(mut self, secs: f64) -> Self {
        self.initial_delay_secs = secs;
        self
    }

    pub fn with_script_char_delay(mut self, secs: f64) -> Self {
        self.script_char_delay_secs = secs;
        self
    }

    pub fn with_script_line_delay(mut self, secs: f64) -> Self {
        self.script_line_delay_secs = secs;
        self
    }

    pub fn with_working_directory(mut self, dir: PathBuf) -> Self {
        self.working_directory = Some(dir);
        self
    }

    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    pub fn with_stop_on_url(mut self, stop: bool) -> Self {
        self.stop_on_url = stop;
        self
    }

    pub fn with_stop_on_substring(mut self, substring: impl Into<String>) -> Self {
        self.stop_on_substrings.push(substring.into());
        self
    }

    pub fn with_send_on_substring(
        mut self,
        trigger: impl Into<String>,
        keys: impl Into<String>,
    ) -> Self {
        self.send_on_substrings.insert(trigger.into(), keys.into());
        self
    }
}

/// Errors from TTY command execution
#[derive(Debug, Error)]
pub enum TtyCommandError {
    #[error("Binary not found: {0}. Install it or add it to PATH.")]
    BinaryNotFound(String),

    #[error("Failed to launch process: {0}")]
    LaunchFailed(String),

    #[error("Command timed out")]
    TimedOut,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Process error: {0}")]
    ProcessError(String),
}

/// TTY Command Runner
///
/// On Windows, this uses standard process I/O with some heuristics for
/// interactive programs. For true PTY support, consider using the
/// `conpty` crate or Windows ConPTY APIs directly.
pub struct TtyCommandRunner;

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowsLaunchKind {
    Direct,
    Batch,
}

impl TtyCommandRunner {
    /// Create a new runner instance
    pub fn new() -> Self {
        Self
    }

    #[cfg(windows)]
    fn windows_launch_kind(path: &Path) -> Option<WindowsLaunchKind> {
        if !path.is_file() {
            return None;
        }

        let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
        match extension.as_str() {
            "exe" | "com" => Some(WindowsLaunchKind::Direct),
            "cmd" | "bat" => Some(WindowsLaunchKind::Batch),
            _ => None,
        }
    }

    /// Pick the first candidate the platform can actually launch.
    ///
    /// On Windows a native executable wins over a batch shim even when the
    /// shim comes from an earlier entry, because a shim only runs through the
    /// command interpreter.
    fn select_launchable_candidate<I>(candidates: I) -> Option<PathBuf>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        #[cfg(windows)]
        {
            let mut batch_candidate = None;
            for candidate in candidates {
                match Self::windows_launch_kind(&candidate) {
                    Some(WindowsLaunchKind::Direct) => return Some(candidate),
                    Some(WindowsLaunchKind::Batch) if batch_candidate.is_none() => {
                        batch_candidate = Some(candidate);
                    }
                    _ => {}
                }
            }
            batch_candidate
        }

        #[cfg(not(windows))]
        {
            candidates
                .into_iter()
                .find(|candidate| Self::is_launchable_binary_path(candidate))
        }
    }

    fn is_launchable_binary_path(path: &Path) -> bool {
        #[cfg(windows)]
        {
            Self::windows_launch_kind(path).is_some()
        }

        #[cfg(not(windows))]
        {
            path.exists()
        }
    }

    #[cfg(windows)]
    fn windows_command_interpreter() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("COMSPEC").map(PathBuf::from)
            && Self::windows_launch_kind(&path) == Some(WindowsLaunchKind::Direct)
        {
            return Some(path);
        }

        let path = Self::run_where("cmd")?;
        (Self::windows_launch_kind(&path) == Some(WindowsLaunchKind::Direct)).then_some(path)
    }

    /// The characters `cmd.exe` treats specially outside of double quotes.
    #[cfg(windows)]
    const CMD_METACHARACTERS: &[char] = &['^', '&', '|', '<', '>', '(', ')', '@', '!'];

    /// Report whether `portable_pty` wraps this token in double quotes.
    ///
    /// This mirrors the `ArgvQuote` rule the crate applies on Windows: a token
    /// is quoted only when it is empty or holds whitespace or a quote.
    #[cfg(windows)]
    fn windows_token_is_quoted(token: &str) -> bool {
        token.is_empty() || token.contains([' ', '\t', '\n', '\u{000b}', '"'])
    }

    /// Caret-escape the `cmd.exe` metacharacters in an unquoted token.
    #[cfg(windows)]
    fn escape_cmd_token(token: &str) -> String {
        let mut escaped = String::with_capacity(token.len());
        for character in token.chars() {
            if Self::CMD_METACHARACTERS.contains(&character) {
                escaped.push('^');
            }
            escaped.push(character);
        }
        escaped
    }

    /// Reject a token `cmd.exe` cannot carry through a command tail intact.
    ///
    /// `cmd.exe` reads `\"` as a plain quote rather than as an escape, and it
    /// expands `%NAME%` with no escape available, so neither character can be
    /// delivered to a batch file.
    #[cfg(windows)]
    fn reject_unrepresentable_cmd_token(token: &str, label: &str) -> Result<(), TtyCommandError> {
        for character in ['"', '%'] {
            if token.contains(character) {
                return Err(TtyCommandError::LaunchFailed(format!(
                    "{label} holds a {character} character that cmd.exe cannot escape: {token}"
                )));
            }
        }
        Ok(())
    }

    /// Build the `cmd.exe` command tail that runs a batch file.
    ///
    /// `portable_pty` serializes arguments with `CreateProcessW` rules, which
    /// are not the rules `cmd.exe` applies, so each token is prepared for the
    /// interpreter here. A token `portable_pty` leaves bare is caret-escaped so
    /// the interpreter passes its metacharacters through; a token
    /// `portable_pty` wraps in quotes is already literal to the interpreter.
    ///
    /// `cmd.exe` keeps the quotes of a tail that starts with a quote only while
    /// that tail holds exactly one quoted, metacharacter-free token, and it has
    /// no escape for a quote inside a token, and it expands `%NAME%` with no
    /// escape available. Everything it cannot carry through intact is rejected
    /// instead of launched incorrectly.
    #[cfg(windows)]
    fn windows_batch_tail(
        resolved: &Path,
        extra_args: &[String],
    ) -> Result<Vec<String>, TtyCommandError> {
        let batch = resolved.to_string_lossy().into_owned();
        Self::reject_unrepresentable_cmd_token(&batch, "batch file path")?;

        let batch_is_quoted = Self::windows_token_is_quoted(&batch);
        if batch_is_quoted && batch.contains(Self::CMD_METACHARACTERS) {
            return Err(TtyCommandError::LaunchFailed(format!(
                "batch file path holds whitespace and a cmd.exe metacharacter: {batch}"
            )));
        }

        let mut tail = Vec::with_capacity(extra_args.len() + 1);
        tail.push(if batch_is_quoted {
            batch.clone()
        } else {
            Self::escape_cmd_token(&batch)
        });

        for argument in extra_args {
            Self::reject_unrepresentable_cmd_token(argument, "batch file argument")?;

            if !Self::windows_token_is_quoted(argument) {
                tail.push(Self::escape_cmd_token(argument));
                continue;
            }

            if batch_is_quoted {
                return Err(TtyCommandError::LaunchFailed(format!(
                    "cmd.exe cannot quote argument {argument:?} together with batch file path {batch}"
                )));
            }
            tail.push(argument.clone());
        }

        Ok(tail)
    }

    fn command_builder(
        resolved: &Path,
        extra_args: &[String],
    ) -> Result<portable_pty::CommandBuilder, TtyCommandError> {
        #[cfg(windows)]
        if Self::windows_launch_kind(resolved) == Some(WindowsLaunchKind::Batch) {
            let comspec = Self::windows_command_interpreter().ok_or_else(|| {
                TtyCommandError::LaunchFailed(
                    "could not locate a launchable Windows command interpreter for batch file"
                        .to_string(),
                )
            })?;
            let tail = Self::windows_batch_tail(resolved, extra_args)?;
            let mut cmd = portable_pty::CommandBuilder::new(comspec.as_os_str());
            cmd.arg("/d");
            cmd.arg("/c");
            cmd.args(tail);
            return Ok(cmd);
        }

        let mut cmd = portable_pty::CommandBuilder::new(resolved.as_os_str());
        cmd.args(extra_args);
        Ok(cmd)
    }

    /// Locate a binary using the system PATH
    pub fn which(tool: &str) -> Option<PathBuf> {
        // Check for specific tool overrides
        if tool == "codex"
            && let Some(path) = Self::locate_codex_binary()
        {
            return Some(path);
        }
        if tool == "claude"
            && let Some(path) = Self::locate_claude_binary()
        {
            return Some(path);
        }

        // Use `where` on Windows (equivalent to `which` on Unix)
        Self::run_where(tool)
    }

    /// Locate the Codex binary
    fn locate_codex_binary() -> Option<PathBuf> {
        // Check environment override
        if let Ok(path) = std::env::var("CODEX_BINARY") {
            let path = PathBuf::from(path);
            if Self::is_launchable_binary_path(&path) {
                return Some(path);
            }
        }

        // Check common Windows locations
        let candidates = [
            // npm global install locations
            dirs::data_local_dir().map(|d| d.join("npm").join("codex.cmd")),
            dirs::home_dir().map(|h| {
                h.join("AppData")
                    .join("Roaming")
                    .join("npm")
                    .join("codex.cmd")
            }),
            // Bun install
            dirs::home_dir().map(|h| h.join(".bun").join("bin").join("codex.exe")),
        ];

        if let Some(candidate) = Self::select_launchable_candidate(candidates.into_iter().flatten())
        {
            return Some(candidate);
        }

        // Fall back to PATH search
        Self::run_where("codex")
    }

    /// Locate the Claude binary
    fn locate_claude_binary() -> Option<PathBuf> {
        // Check environment override
        if let Ok(path) = std::env::var("CLAUDE_BINARY") {
            let path = PathBuf::from(path);
            if Self::is_launchable_binary_path(&path) {
                return Some(path);
            }
        }

        // Check common Windows locations
        let candidates = [
            // npm global install locations
            dirs::data_local_dir().map(|d| d.join("npm").join("claude.cmd")),
            dirs::home_dir().map(|h| {
                h.join("AppData")
                    .join("Roaming")
                    .join("npm")
                    .join("claude.cmd")
            }),
        ];

        if let Some(candidate) = Self::select_launchable_candidate(candidates.into_iter().flatten())
        {
            return Some(candidate);
        }

        Self::run_where("claude")
    }

    /// Find a binary in PATH.
    fn run_where(tool: &str) -> Option<PathBuf> {
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x08000000;

            let where_exe = crate::host::windows_where_exe()?;
            let mut command = Command::new(where_exe);
            command
                .arg(tool)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW);
            let output = command.output().ok()?;

            if !output.status.success() {
                return None;
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            Self::select_launchable_candidate(
                stdout
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(PathBuf::from),
            )
        }

        #[cfg(not(windows))]
        {
            which::which(tool).ok()
        }
    }

    fn is_explicit_binary_path(binary: &str) -> bool {
        let path = std::path::Path::new(binary);
        path.is_absolute() || path.components().count() > 1
    }

    /// Run a command and capture its output
    ///
    /// Uses the platform pseudo-terminal implementation, including Windows
    /// ConPTY, so interactive programs see a real terminal.
    pub fn run(
        &self,
        binary: &str,
        script: &str,
        options: TtyCommandOptions,
    ) -> Result<TtyCommandResult, TtyCommandError> {
        // Resolve the binary path
        let resolved = if Self::is_explicit_binary_path(binary) {
            let path = PathBuf::from(binary);
            if Self::is_launchable_binary_path(&path) {
                path
            } else {
                return Err(TtyCommandError::BinaryNotFound(binary.to_string()));
            }
        } else if let Some(path) = Self::which(binary) {
            path
        } else {
            return Err(TtyCommandError::BinaryNotFound(binary.to_string()));
        };

        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(portable_pty::PtySize {
                rows: options.rows,
                cols: options.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| TtyCommandError::LaunchFailed(e.to_string()))?;

        let mut cmd = Self::command_builder(&resolved, &options.extra_args)?;

        if let Some(ref dir) = options.working_directory {
            cmd.cwd(dir.as_os_str());
        }

        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (key, value) in &options.env {
            cmd.env(key, value);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| TtyCommandError::LaunchFailed(e.to_string()))?;
        drop(pair.slave);

        self.run_pty_session(pair.master, &mut child, script, &options)
    }

    /// Run an interactive session with the child process
    fn run_pty_session(
        &self,
        master: Box<dyn portable_pty::MasterPty + Send>,
        child: &mut Box<dyn portable_pty::Child + Send + Sync>,
        script: &str,
        options: &TtyCommandOptions,
    ) -> Result<TtyCommandResult, TtyCommandError> {
        let start = Instant::now();
        let timeout = Duration::from_secs_f64(options.timeout_secs);
        let idle_timeout = options.idle_timeout_secs.map(Duration::from_secs_f64);
        let settle = Duration::from_secs_f64(options.settle_after_stop_secs);

        let mut buffer = String::new();
        let mut stopped_early = false;
        let mut detected_urls = Vec::new();
        let mut last_output_time = Instant::now();
        let mut triggered_sends = std::collections::HashSet::new();

        // URL detection regex
        let url_regex = Regex::new(r"https?://[A-Za-z0-9._~:/?#\[\]@!$&'()*+,;=%-]+").ok();

        // Set up non-blocking readers using channels
        let (tx, rx) = mpsc::channel::<String>();
        let mut writer = master
            .take_writer()
            .map_err(|e| TtyCommandError::LaunchFailed(e.to_string()))?;
        let mut reader = master
            .try_clone_reader()
            .map_err(|e| TtyCommandError::LaunchFailed(e.to_string()))?;

        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(s) = String::from_utf8(buf[..n].to_vec()) {
                            let _ = tx.send(s);
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Initial delay
        std::thread::sleep(Duration::from_secs_f64(options.initial_delay_secs));

        // Windows ConPTY discards input written before the shell has attached
        // its console reader, so a fixed delay races process startup: typing
        // the script while the shell was still starting dropped it, and the
        // test that drives cmd.exe through this path failed intermittently
        // under parallel load for exactly that reason. After the configured
        // delay, give the shell a bounded window to produce its first output
        // (banner or prompt) before typing. A program that stays silent still
        // receives the script once the window closes, and the window is capped
        // by the remaining timeout, so no caller can hang.
        let ready_deadline =
            Instant::now() + READY_GRACE.min(timeout.saturating_sub(start.elapsed()));
        let mut device_status = DeviceStatusWatcher::default();
        loop {
            match rx.try_recv() {
                Ok(chunk) => {
                    // The readiness probe can be the first read of the session,
                    // so it has to answer the handshake too, not just break.
                    answer_device_status_report(&mut device_status, &mut *writer, &chunk);
                    buffer.push_str(&chunk);
                    last_output_time = Instant::now();
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if Instant::now() >= ready_deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }

        // Send the script if provided. PTYs expect carriage-return line endings
        // for interactive programs to treat writes like pressing Enter.
        let script_lines: Vec<&str> = script
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.trim().is_empty())
            .collect();
        for (idx, line) in script_lines.iter().enumerate() {
            if options.script_char_delay_secs > 0.0 {
                for ch in line.chars() {
                    let _ = write!(writer, "{}", ch);
                    let _ = writer.flush();
                    std::thread::sleep(Duration::from_secs_f64(options.script_char_delay_secs));
                }
                let _ = write!(writer, "\r\n");
            } else {
                let _ = write!(writer, "{}\r\n", line);
            }
            let _ = writer.flush();
            if idx + 1 < script_lines.len() && options.script_line_delay_secs > 0.0 {
                std::thread::sleep(Duration::from_secs_f64(options.script_line_delay_secs));
            }
        }

        let mut last_enter = Instant::now();

        // Main read loop
        loop {
            // Check timeout
            if start.elapsed() > timeout {
                break;
            }

            // Check idle timeout
            if let Some(idle) = idle_timeout
                && !buffer.is_empty()
                && last_output_time.elapsed() > idle
            {
                stopped_early = true;
                break;
            }

            // Check if process has exited
            if let Ok(Some(_)) = child.try_wait() {
                // Process exited, drain remaining output
                while let Ok(chunk) = rx.try_recv() {
                    buffer.push_str(&chunk);
                }
                break;
            }

            // Read available output
            while let Ok(chunk) = rx.try_recv() {
                buffer.push_str(&chunk);
                last_output_time = Instant::now();

                answer_device_status_report(&mut device_status, &mut *writer, &chunk);

                // Check for URLs
                if let Some(ref regex) = url_regex {
                    for mat in regex.find_iter(&chunk) {
                        let mut url = mat.as_str().to_string();
                        // Trim trailing punctuation
                        while url.ends_with(|c| {
                            matches!(
                                c,
                                '.' | ',' | ';' | ':' | ')' | ']' | '}' | '>' | '"' | '\''
                            )
                        }) {
                            url.pop();
                        }
                        if !detected_urls.contains(&url) {
                            detected_urls.push(url);
                        }
                    }

                    if options.stop_on_url && !detected_urls.is_empty() {
                        stopped_early = true;
                        break;
                    }
                }

                // Check for stop substrings
                for stop_str in &options.stop_on_substrings {
                    if buffer.contains(stop_str) {
                        stopped_early = true;
                        break;
                    }
                }

                // Check for send triggers
                for (trigger, keys) in &options.send_on_substrings {
                    if !triggered_sends.contains(trigger) && buffer.contains(trigger) {
                        let normalized = keys.replace('\n', "\r\n");
                        let _ = write!(writer, "{}", normalized);
                        let _ = writer.flush();
                        triggered_sends.insert(trigger.clone());
                    }
                }
            }

            if stopped_early {
                break;
            }

            // Send periodic enters if configured
            if let Some(interval) = options.send_enter_every_secs
                && last_enter.elapsed() >= Duration::from_secs_f64(interval)
            {
                let _ = write!(writer, "\r\n");
                let _ = writer.flush();
                last_enter = Instant::now();
            }

            // Small sleep to avoid busy loop
            std::thread::sleep(Duration::from_millis(50));
        }

        // Settle period - collect remaining output
        if stopped_early {
            let settle_start = Instant::now();
            while settle_start.elapsed() < settle {
                while let Ok(chunk) = rx.try_recv() {
                    buffer.push_str(&chunk);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }

        if buffer.is_empty() && !stopped_early {
            return Err(TtyCommandError::TimedOut);
        }

        Ok(TtyCommandResult {
            text: buffer,
            stopped_early,
            detected_urls,
        })
    }

    /// Get enriched PATH for finding CLI tools
    pub fn enriched_path() -> String {
        let mut paths = Vec::new();

        if let Some(home) = dirs::home_dir() {
            #[cfg(windows)]
            paths.push(home.join("AppData").join("Roaming").join("npm"));
            #[cfg(not(windows))]
            paths.push(home.join(".local").join("bin"));
            paths.push(home.join(".bun").join("bin"));
            paths.push(home.join(".deno").join("bin"));
            paths.push(home.join(".cargo").join("bin"));
        }

        #[cfg(windows)]
        if let Some(local) = dirs::data_local_dir() {
            paths.push(local.join("npm"));
        }

        let mut all_paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.exists()).collect();

        if let Some(current_path) = std::env::var_os("PATH") {
            all_paths.extend(std::env::split_paths(&current_path));
        }

        std::env::join_paths(all_paths)
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|_| std::env::var("PATH").unwrap_or_default())
    }

    /// Get enriched environment for CLI commands
    pub fn enriched_environment() -> HashMap<String, String> {
        let mut env: HashMap<String, String> = std::env::vars().collect();

        env.insert("PATH".to_string(), Self::enriched_path());
        env.entry("TERM".to_string())
            .or_insert_with(|| "xterm-256color".to_string());
        env.entry("COLORTERM".to_string())
            .or_insert_with(|| "truecolor".to_string());

        if let Some(home) = dirs::home_dir() {
            env.entry("HOME".to_string())
                .or_insert_with(|| home.to_string_lossy().to_string());
            env.entry("USERPROFILE".to_string())
                .or_insert_with(|| home.to_string_lossy().to_string());
        }

        env
    }
}

impl Default for TtyCommandRunner {
    fn default() -> Self {
        Self::new()
    }
}

/// Rolling buffer for pattern matching across chunks
#[derive(Debug)]
pub struct RollingBuffer {
    max_needle: usize,
    tail: String,
}

impl RollingBuffer {
    pub fn new(max_needle: usize) -> Self {
        Self {
            max_needle: max_needle.max(1),
            tail: String::new(),
        }
    }

    /// Append new data and return combined data for scanning
    pub fn append(&mut self, data: &str) -> String {
        if data.is_empty() {
            return String::new();
        }

        let mut combined = String::with_capacity(self.tail.len() + data.len());
        combined.push_str(&self.tail);
        combined.push_str(data);

        // Keep only the tail portion for next scan
        if combined.len() >= self.max_needle - 1 {
            let start = combined.len() - (self.max_needle - 1);
            self.tail = combined[start..].to_string();
        } else {
            self.tail = combined.clone();
        }

        combined
    }

    pub fn reset(&mut self) {
        self.tail.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tty_options_builder() {
        let opts = TtyCommandOptions::new()
            .with_timeout(30.0)
            .with_idle_timeout(5.0)
            .with_stop_on_url(true)
            .with_stop_on_substring("error");

        assert_eq!(opts.timeout_secs, 30.0);
        assert_eq!(opts.idle_timeout_secs, Some(5.0));
        assert!(opts.stop_on_url);
        assert!(opts.stop_on_substrings.contains(&"error".to_string()));
    }

    #[test]
    fn device_status_query_is_seen_across_read_boundaries() {
        // Every possible split of the query must be detected exactly once in
        // total, whether it lands whole in one read or across two.
        for split in 0..=DEVICE_STATUS_QUERY.len() {
            let (head, tail) = DEVICE_STATUS_QUERY.split_at(split);
            let mut watcher = DeviceStatusWatcher::default();

            let first = watcher.observe(head);
            let second = watcher.observe(tail);
            assert_eq!(
                first + second,
                1,
                "split at {split} ({head:?} | {tail:?}) must complete exactly once"
            );
            // A trailing read with no query must not re-fire.
            assert_eq!(watcher.observe(""), 0);
        }
    }

    #[test]
    fn device_status_query_counts_each_occurrence_once() {
        let mut watcher = DeviceStatusWatcher::default();
        assert_eq!(
            watcher.observe("\x1b[6n\x1b[6n"),
            2,
            "two queries in one read"
        );
        assert_eq!(watcher.observe("\x1b[6n"), 1, "a later read fires again");
        assert_eq!(watcher.observe("\x1b[6"), 0, "a partial query does not");
        assert_eq!(watcher.observe("n"), 1, "and completes on the next read");
    }

    #[test]
    fn device_status_replies_once_per_query() {
        let mut watcher = DeviceStatusWatcher::default();
        let mut out = Vec::new();
        answer_device_status_report(&mut watcher, &mut out, "\x1b[6");
        assert!(out.is_empty(), "partial query must not be answered yet");
        answer_device_status_report(&mut watcher, &mut out, "n");
        assert_eq!(String::from_utf8(out).expect("utf8"), DEVICE_STATUS_REPLY);
    }

    #[test]
    fn device_status_query_is_not_confused_by_other_escape_output() {
        let mut watcher = DeviceStatusWatcher::default();
        // A DSR query buried in ordinary banner output is still answered once.
        assert_eq!(watcher.observe("Microsoft Windows [\x1b[6nVersion 10]"), 1);
        assert_eq!(watcher.observe("no query here"), 0);
    }

    #[test]
    fn device_status_carry_does_not_grow_without_bound() {
        let mut watcher = DeviceStatusWatcher::default();
        for _ in 0..1000 {
            assert_eq!(watcher.observe("plain output with no query"), 0);
        }
        assert!(
            watcher.carry.len() <= DEVICE_STATUS_QUERY_MAX_SPLIT,
            "carry must stay bounded, got {}",
            watcher.carry.len()
        );
    }

    #[test]
    fn test_rolling_buffer() {
        let mut buf = RollingBuffer::new(10);

        let result1 = buf.append("hello");
        assert_eq!(result1, "hello");

        let result2 = buf.append(" world");
        assert!(result2.contains("hello"));
        assert!(result2.contains(" world"));
    }

    #[test]
    fn test_tty_result_first_url() {
        let result = TtyCommandResult {
            text: "Visit https://example.com for more info".to_string(),
            stopped_early: false,
            detected_urls: vec!["https://example.com".to_string()],
        };

        assert_eq!(result.first_url(), Some("https://example.com"));
    }

    #[test]
    fn test_run_sends_script_through_pty() {
        let runner = TtyCommandRunner::new();
        // Generous on purpose: this measures a real shell starting on
        // whatever machine runs the suite. A cold CI runner can take longer
        // than two seconds to reach a prompt, and the test then sees only the
        // banner and calls the PTY broken.
        let opts = TtyCommandOptions::new()
            .with_timeout(30.0)
            .with_idle_timeout(10.0)
            .with_script_line_delay(0.1);

        // Windows names the shell absolutely. A bare "cmd" resolves through
        // the enriched PATH, which puts the npm directory first, so a machine
        // carrying an extensionless npm shim called `cmd` fails the launch with
        // "not a valid Win32 application": a fact about that machine rather
        // than about the PTY this covers. Unix keeps the PATH lookup, where sh
        // is not always at /bin/sh.
        #[cfg(windows)]
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd".to_string());
        #[cfg(not(windows))]
        let shell = "sh".to_string();
        let result = runner.run(&shell, "echo CODEXBAR_PTY_OK\nexit", opts);

        let result = result.expect("pty command should run");
        assert!(result.text.contains("CODEXBAR_PTY_OK"), "{}", result.text);
    }

    #[cfg(windows)]
    fn windows_test_dir(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after Unix epoch")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("ceiling-tty-{name}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create tty test directory");
        dir
    }

    #[cfg(windows)]
    #[test]
    fn windows_candidate_selection_skips_extensionless_shim() {
        let dir = windows_test_dir("candidate-shim");
        let shim = dir.join("cmd");
        let executable = dir.join("cmd.exe");
        std::fs::write(&shim, "#!/bin/sh\n").expect("write shim");
        std::fs::write(&executable, []).expect("write executable candidate");

        let selected =
            TtyCommandRunner::select_launchable_candidate(vec![shim, executable.clone()]);
        assert_eq!(selected, Some(executable));

        std::fs::remove_dir_all(dir).expect("remove tty test directory");
    }

    #[cfg(windows)]
    #[test]
    fn windows_candidate_selection_prefers_native_executable_over_batch() {
        let dir = windows_test_dir("candidate-priority");
        let batch = dir.join("tool.cmd");
        let executable = dir.join("tool.exe");
        std::fs::write(&batch, "@echo off\r\n").expect("write batch candidate");
        std::fs::write(&executable, []).expect("write executable candidate");

        let selected =
            TtyCommandRunner::select_launchable_candidate(vec![batch, executable.clone()]);
        assert_eq!(selected, Some(executable));

        std::fs::remove_dir_all(dir).expect("remove tty test directory");
    }

    #[cfg(windows)]
    #[test]
    fn windows_candidate_selection_accepts_batch_when_needed() {
        let dir = windows_test_dir("candidate-batch");
        let batch = dir.join("tool.cmd");
        std::fs::write(&batch, "@echo off\r\n").expect("write batch candidate");

        let selected = TtyCommandRunner::select_launchable_candidate(vec![batch.clone()]);
        assert_eq!(selected, Some(batch));

        std::fs::remove_dir_all(dir).expect("remove tty test directory");
    }

    #[cfg(windows)]
    #[test]
    fn windows_explicit_path_rejects_directory_and_extensionless_file() {
        let dir = windows_test_dir("explicit-validation");
        let fake_exe_dir = dir.join("fake.exe");
        let extensionless = dir.join("tool");
        std::fs::create_dir(&fake_exe_dir).expect("create fake executable directory");
        std::fs::write(&extensionless, "#!/bin/sh\n").expect("write extensionless shim");

        assert!(!TtyCommandRunner::is_launchable_binary_path(&fake_exe_dir));
        assert!(!TtyCommandRunner::is_launchable_binary_path(&extensionless));

        std::fs::remove_dir_all(dir).expect("remove tty test directory");
    }

    #[cfg(windows)]
    #[test]
    fn windows_batch_file_runs_through_command_interpreter() {
        let dir = windows_test_dir("batch-run");
        let batch = dir.join("echo-ok.cmd");
        std::fs::write(&batch, "@echo off\r\necho CEILING_BATCH_OK %1\r\n")
            .expect("write batch file");

        let runner = TtyCommandRunner::new();
        let opts = TtyCommandOptions::new()
            .with_timeout(10.0)
            .with_idle_timeout(5.0)
            .with_initial_delay(0.0)
            .with_extra_args(vec!["ARG_OK".to_string()]);
        let result = runner
            .run(batch.to_string_lossy().as_ref(), "", opts)
            .expect("batch file should run through COMSPEC");
        assert!(
            result.text.contains("CEILING_BATCH_OK ARG_OK"),
            "{}",
            result.text
        );

        std::fs::remove_dir_all(dir).expect("remove tty test directory");
    }

    #[cfg(windows)]
    #[test]
    fn windows_batch_tail_escapes_bare_metacharacters() {
        let tail = TtyCommandRunner::windows_batch_tail(
            Path::new(r"C:\tools\npm\claude.cmd"),
            &["a&b".to_string(), "a|b".to_string(), "x^y".to_string()],
        )
        .expect("bare arguments should be representable");

        assert_eq!(
            tail,
            vec![
                r"C:\tools\npm\claude.cmd".to_string(),
                "a^&b".to_string(),
                "a^|b".to_string(),
                "x^^y".to_string(),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_batch_tail_escapes_metacharacters_in_the_batch_path() {
        let tail =
            TtyCommandRunner::windows_batch_tail(Path::new(r"C:\tools&more\claude.cmd"), &[])
                .expect("bare path should be representable");

        assert_eq!(tail, vec![r"C:\tools^&more\claude.cmd".to_string()]);
    }

    #[cfg(windows)]
    #[test]
    fn windows_batch_tail_keeps_quoted_arguments_literal() {
        let tail = TtyCommandRunner::windows_batch_tail(
            Path::new(r"C:\tools\npm\claude.cmd"),
            &["a & b".to_string()],
        )
        .expect("quoted arguments should be representable");

        assert_eq!(tail[1], "a & b");
    }

    #[cfg(windows)]
    #[test]
    fn windows_batch_tail_rejects_quote_characters() {
        let error = TtyCommandRunner::windows_batch_tail(
            Path::new(r"C:\tools\npm\claude.cmd"),
            &["a\"b".to_string()],
        )
        .expect_err("cmd.exe has no escape for a quote inside a token");

        assert!(
            matches!(error, TtyCommandError::LaunchFailed(_)),
            "{error:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_batch_tail_rejects_percent_expansion() {
        let error = TtyCommandRunner::windows_batch_tail(
            Path::new(r"C:\tools\npm\claude.cmd"),
            &["a%PATH%b".to_string()],
        )
        .expect_err("cmd.exe expands %NAME% with no escape available");

        assert!(
            matches!(error, TtyCommandError::LaunchFailed(_)),
            "{error:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_batch_tail_rejects_quoted_path_with_quoted_argument() {
        let error = TtyCommandRunner::windows_batch_tail(
            Path::new(r"C:\Program Files\npm\claude.cmd"),
            &["a b".to_string()],
        )
        .expect_err("cmd.exe strips the quotes of a tail that holds two quoted tokens");

        assert!(
            matches!(error, TtyCommandError::LaunchFailed(_)),
            "{error:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_batch_tail_allows_one_quoted_path() {
        let tail = TtyCommandRunner::windows_batch_tail(
            Path::new(r"C:\Program Files\npm\claude.cmd"),
            &["--setting-sources".to_string(), "user".to_string()],
        )
        .expect("cmd.exe keeps the quotes of a single quoted token");

        assert_eq!(tail[0], r"C:\Program Files\npm\claude.cmd");
    }

    #[cfg(windows)]
    #[test]
    fn windows_batch_file_receives_metacharacter_arguments() {
        let dir = windows_test_dir("batch-metacharacters");
        let batch = dir.join("echo-arg.cmd");
        std::fs::write(
            &batch,
            concat!(
                "@echo off\r\n",
                "setlocal enabledelayedexpansion\r\n",
                "set \"value=%~1\"\r\n",
                "echo CEILING_ARG [!value!]\r\n"
            ),
        )
        .expect("write batch file");

        let runner = TtyCommandRunner::new();
        let opts = TtyCommandOptions::new()
            .with_timeout(10.0)
            .with_idle_timeout(5.0)
            .with_initial_delay(0.0)
            .with_extra_args(vec!["a&b|c>d".to_string()]);
        let result = runner
            .run(batch.to_string_lossy().as_ref(), "", opts)
            .expect("batch file should run through COMSPEC");
        assert!(
            result.text.contains("CEILING_ARG [a&b|c>d]"),
            "{}",
            result.text
        );

        std::fs::remove_dir_all(dir).expect("remove tty test directory");
    }

    #[test]
    fn test_enriched_path() {
        let path = TtyCommandRunner::enriched_path();
        assert!(!path.is_empty());
        // Should contain path separator or at least a non-empty path string.
        assert!(path.contains(';') || !path.is_empty());
    }
}
