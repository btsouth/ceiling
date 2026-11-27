//! Bounded, windowless CLI probes for synchronous call sites.
//!
//! `--version` and `where` probes run from sync paths (credential detection,
//! `Provider::detect_version`) where the tokio helper in
//! [`crate::host::tokio_cli`] cannot be awaited. A plain `Command::output()`
//! there waits forever on a CLI that hangs (an update prompt, a broken shim).
//! This runs the command with stdin closed, no console window on Windows, and
//! a wall-clock deadline after which the child is killed.

use std::io::Read;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Default deadline for a version or path probe.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Run `command` to completion, or kill it once `timeout` passes.
///
/// Returns `None` when the command cannot start or times out.
pub(crate) fn output_with_timeout(command: &mut Command, timeout: Duration) -> Option<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_windows_console(command);

    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take().map(spawn_reader);
    let stderr = child.stderr.take().map(spawn_reader);

    let deadline = Instant::now() + timeout;
    // On timeout the readers are abandoned, not joined: a grandchild (an npm
    // `.cmd` shim's node process) can keep the pipes open after the kill.
    let status = wait_with_deadline(&mut child, deadline)?;
    let grace = deadline
        .saturating_duration_since(Instant::now())
        .max(READ_GRACE);
    Some(Output {
        status,
        stdout: stdout
            .map(|reader| collect(reader, grace))
            .unwrap_or_default(),
        stderr: stderr
            .map(|reader| collect(reader, grace))
            .unwrap_or_default(),
    })
}

/// Extra time to drain pipes after the child exits.
const READ_GRACE: Duration = Duration::from_millis(500);

fn wait_with_deadline(child: &mut Child, deadline: Instant) -> Option<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL_INTERVAL),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn spawn_reader(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = pipe.read_to_end(&mut buffer);
        let _ = sender.send(buffer);
    });
    receiver
}

fn collect(reader: mpsc::Receiver<Vec<u8>>, wait: Duration) -> Vec<u8> {
    reader.recv_timeout(wait).unwrap_or_default()
}

#[cfg(windows)]
fn hide_windows_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_windows_console(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_command() -> Command {
        #[cfg(windows)]
        {
            let mut command = Command::new(
                crate::host::windows_system_exe("cmd.exe")
                    .unwrap_or_else(|| std::path::PathBuf::from("cmd.exe")),
            );
            command.args(["/C", "echo hello"]);
            command
        }
        #[cfg(not(windows))]
        {
            let mut command = Command::new("echo");
            command.arg("hello");
            command
        }
    }

    fn long_running_command() -> Command {
        #[cfg(windows)]
        {
            let powershell = crate::host::windows_powershell_exe()
                .expect("Windows PowerShell should exist for process tests");
            let mut command = Command::new(powershell);
            command.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ]);
            command
        }
        #[cfg(not(windows))]
        {
            let mut command = Command::new("sleep");
            command.arg("30");
            command
        }
    }

    #[test]
    fn captures_output_of_a_quick_command() {
        let output = output_with_timeout(&mut echo_command(), Duration::from_secs(10))
            .expect("echo completes");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
    }

    #[test]
    fn kills_a_command_that_outlives_its_deadline() {
        let started = Instant::now();
        let output = output_with_timeout(&mut long_running_command(), Duration::from_millis(300));
        assert!(output.is_none(), "a hung probe must time out");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the probe must not wait for the child to finish"
        );
    }

    #[test]
    fn a_missing_binary_is_none() {
        let mut command = Command::new("ceiling-definitely-missing-binary");
        assert!(output_with_timeout(&mut command, Duration::from_secs(1)).is_none());
    }
}
