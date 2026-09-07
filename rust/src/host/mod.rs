//! Host module for process management and command execution

pub mod command_runner;
pub mod session;
pub mod tokio_cli;
pub mod windows_system;

pub use windows_system::{windows_powershell_exe, windows_system_exe, windows_where_exe};

// Re-exports for future CLI integration
#[allow(unused_imports)]
pub use command_runner::{
    CommandError, CommandOptions, CommandResult, CommandRunner, RollingBuffer,
};
