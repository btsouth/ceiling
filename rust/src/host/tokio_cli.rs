//! Tokio CLI runner for provider fetch paths.
//!
//! `tokio::process::Command` defaults `kill_on_drop` to false. Desktop refresh
//! wraps `Provider::fetch_usage` in `tokio::time::timeout`, so a deadline cancel
//! drops the `Child` and leaves the CLI running. `CommandRunner` already kills
//! on deadline; this helper matches that contract for async fetch commands.

use std::process::{Output, Stdio};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Error from a timed CLI fetch.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    TimedOut,
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::TimedOut => write!(f, "Command timed out"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::TimedOut => None,
        }
    }
}

/// Mark a tokio command so dropping its `Child` kills the process.
pub fn configure(cmd: &mut Command) -> &mut Command {
    cmd.kill_on_drop(true)
}

/// Capture stdout/stderr. Cancellation drops the child and kills it.
pub async fn output(cmd: &mut Command) -> std::io::Result<Output> {
    configure(cmd);
    cmd.output().await
}

/// Capture stdout/stderr with a wall-clock deadline.
///
/// On timeout this kill+waits like [`crate::host::command_runner::CommandRunner`].
/// `kill_on_drop` still covers cancel of this future itself (outer fetch timeout).
pub async fn output_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<Output, Error> {
    configure(cmd);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let collect = async {
        let (status, stdout, stderr) =
            tokio::try_join!(child.wait(), read_pipe(&mut stdout), read_pipe(&mut stderr))?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    };

    match tokio::time::timeout(timeout, collect).await {
        Ok(result) => result.map_err(Error::Io),
        Err(_) => {
            finish_child(&mut child).await;
            Err(Error::TimedOut)
        }
    }
}

async fn read_pipe(pipe: &mut Option<impl AsyncReadExt + Unpin>) -> std::io::Result<Vec<u8>> {
    match pipe {
        Some(reader) => {
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await?;
            Ok(buf)
        }
        None => Ok(Vec::new()),
    }
}

async fn finish_child(child: &mut tokio::process::Child) {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) | Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn long_running_command() -> Command {
        #[cfg(windows)]
        {
            let powershell = crate::host::windows_powershell_exe()
                .expect("Windows PowerShell should exist for process tests");
            let mut cmd = Command::new(powershell);
            cmd.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ]);
            cmd
        }
        #[cfg(not(windows))]
        {
            let mut cmd = Command::new("sleep");
            cmd.arg("30");
            cmd
        }
    }

    fn echo_command() -> Command {
        #[cfg(windows)]
        {
            let mut cmd = Command::new(
                crate::host::windows_system_exe("cmd.exe")
                    .unwrap_or_else(|| std::path::PathBuf::from("cmd.exe")),
            );
            cmd.args(["/C", "echo hello"]);
            cmd
        }
        #[cfg(not(windows))]
        {
            let mut cmd = Command::new("echo");
            cmd.arg("hello");
            cmd
        }
    }

    fn process_is_live(pid: u32) -> bool {
        #[cfg(unix)]
        {
            let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
                return false;
            };
            for line in status.lines() {
                if let Some(state) = line.strip_prefix("State:") {
                    return !state.contains('Z');
                }
            }
            true
        }
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::CloseHandle;
            use windows::Win32::System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            };

            unsafe {
                let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                    return false;
                };
                let mut code = 0u32;
                let ok = GetExitCodeProcess(handle, &mut code).is_ok();
                let _ = CloseHandle(handle);
                ok && code == 259
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = pid;
            false
        }
    }

    async fn wait_until(predicate: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        predicate()
    }

    #[tokio::test]
    async fn output_returns_successful_command_stdout() {
        let mut cmd = echo_command();
        let output = output(&mut cmd).await.expect("echo should run");
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.to_ascii_lowercase().contains("hello"));
    }

    #[tokio::test]
    async fn dropping_child_with_kill_on_drop_stops_the_process() {
        let mut cmd = long_running_command();
        configure(&mut cmd);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());

        let child = cmd.spawn().expect("spawn sleeper");
        let pid = child.id().expect("child pid");
        assert!(
            wait_until(|| process_is_live(pid)).await,
            "sleeper {pid} should be running after spawn"
        );

        drop(child);

        assert!(
            wait_until(|| !process_is_live(pid)).await,
            "sleeper {pid} should be killed when Child drops with kill_on_drop"
        );
    }

    #[tokio::test]
    async fn timeout_kills_and_reaps_like_command_runner() {
        let mut cmd = long_running_command();
        configure(&mut cmd);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd.spawn().expect("spawn sleeper");
        let pid = child.id().expect("child pid");
        assert!(
            wait_until(|| process_is_live(pid)).await,
            "sleeper {pid} should be running after spawn"
        );

        let started = Instant::now();
        let timed_out = tokio::time::timeout(Duration::from_millis(150), child.wait())
            .await
            .is_err();
        assert!(timed_out, "sleeper should still be running at the deadline");
        finish_child(&mut child).await;

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout took {:?}",
            started.elapsed()
        );
        assert!(
            wait_until(|| !process_is_live(pid)).await,
            "sleeper {pid} should be gone after kill+wait"
        );
    }

    #[tokio::test]
    async fn output_with_timeout_returns_timed_out_for_a_long_command() {
        let mut cmd = long_running_command();
        let started = Instant::now();
        let error = output_with_timeout(&mut cmd, Duration::from_millis(150))
            .await
            .expect_err("long sleeper should time out");
        assert!(matches!(error, Error::TimedOut));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout took {:?}",
            started.elapsed()
        );
    }
}
