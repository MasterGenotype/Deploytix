//! Command execution utilities

use crate::utils::error::{DeploytixError, Result};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Record of a single command invocation captured during rehearsal mode.
#[derive(Debug, Clone)]
pub struct OperationRecord {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration: Duration,
    pub success: bool,
}

/// Execute a command and return the output
pub fn run_command(program: &str, args: &[&str]) -> Result<Output> {
    debug!("Running: {} {}", program, args.join(" "));

    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DeploytixError::CommandNotFound(program.to_string())
            } else {
                DeploytixError::Io(e)
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        warn!(
            "Command failed: {} {}\n  stderr: {}",
            program,
            args.join(" "),
            stderr.trim()
        );
        return Err(DeploytixError::CommandFailed {
            command: format!("{} {}", program, args.join(" ")),
            stderr,
        });
    }

    Ok(output)
}

/// Check if a command exists in PATH
pub fn command_exists(program: &str) -> bool {
    Command::new("which")
        .arg(program)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run a command in chroot using artix-chroot (if available) or plain chroot
pub fn run_in_artix_chroot(chroot_path: &str, command: &str) -> Result<Output> {
    if command_exists("artix-chroot") {
        run_command("artix-chroot", &[chroot_path, "bash", "-c", command])
    } else {
        // Fallback to plain chroot
        run_command("chroot", &[chroot_path, "bash", "-c", command])
    }
}

/// Log a command that would be run (for dry-run mode)
pub fn log_dry_run(program: &str, args: &[&str]) {
    println!("  [dry-run] {} {}", program, args.join(" "));
}

/// Wrapper for command execution that respects dry-run mode.
///
/// When a recorder channel is set, every executed command is captured as an
/// `OperationRecord` and sent through the channel.  This is used by the
/// rehearsal system to produce a detailed execution log.  The recorder is
/// opt-in and has zero overhead when not configured.
pub struct CommandRunner {
    dry_run: bool,
    recorder: Option<Sender<OperationRecord>>,
}

impl CommandRunner {
    pub fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            recorder: None,
        }
    }

    /// Attach a recording channel.  Every command execution will send an
    /// `OperationRecord` through the channel before returning.
    pub fn with_recorder(mut self, tx: Sender<OperationRecord>) -> Self {
        self.recorder = Some(tx);
        self
    }

    /// Record an executed command if a recorder is attached.
    fn record(&self, command_str: &str, output: &Output, elapsed: Duration) {
        if let Some(ref tx) = self.recorder {
            let _ = tx.send(OperationRecord {
                command: command_str.to_string(),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                exit_code: output.status.code().unwrap_or(-1),
                duration: elapsed,
                success: output.status.success(),
            });
        }
    }

    /// Record a failed command (one that could not be spawned at all).
    fn record_err(&self, command_str: &str, err: &DeploytixError, elapsed: Duration) {
        if let Some(ref tx) = self.recorder {
            let _ = tx.send(OperationRecord {
                command: command_str.to_string(),
                stdout: String::new(),
                stderr: format!("{}", err),
                exit_code: -1,
                duration: elapsed,
                success: false,
            });
        }
    }

    pub fn run(&self, program: &str, args: &[&str]) -> Result<Option<Output>> {
        if crate::utils::signal::is_interrupted() {
            return Err(DeploytixError::Interrupted);
        }
        if self.dry_run {
            log_dry_run(program, args);
            Ok(None)
        } else {
            let cmd_str = format!("{} {}", program, args.join(" "));
            let start = Instant::now();
            match run_command(program, args) {
                Ok(output) => {
                    self.record(&cmd_str, &output, start.elapsed());
                    Ok(Some(output))
                }
                Err(e) => {
                    self.record_err(&cmd_str, &e, start.elapsed());
                    Err(e)
                }
            }
        }
    }

    pub fn run_in_chroot(&self, chroot_path: &str, command: &str) -> Result<Option<Output>> {
        if crate::utils::signal::is_interrupted() {
            return Err(DeploytixError::Interrupted);
        }
        if self.dry_run {
            println!("  [dry-run] chroot {} bash -c '{}'", chroot_path, command);
            Ok(None)
        } else {
            let cmd_str = format!("chroot {} bash -c '{}'", chroot_path, command);
            let start = Instant::now();
            match run_in_artix_chroot(chroot_path, command) {
                Ok(output) => {
                    self.record(&cmd_str, &output, start.elapsed());
                    Ok(Some(output))
                }
                Err(e) => {
                    self.record_err(&cmd_str, &e, start.elapsed());
                    Err(e)
                }
            }
        }
    }

    /// Run a command regardless of interrupt state.
    /// Used for cleanup operations that must execute even after a signal.
    pub fn force_run(&self, program: &str, args: &[&str]) -> Result<Option<Output>> {
        if self.dry_run {
            log_dry_run(program, args);
            Ok(None)
        } else {
            let cmd_str = format!("{} {}", program, args.join(" "));
            let start = Instant::now();
            match run_command(program, args) {
                Ok(output) => {
                    self.record(&cmd_str, &output, start.elapsed());
                    Ok(Some(output))
                }
                Err(e) => {
                    self.record_err(&cmd_str, &e, start.elapsed());
                    Err(e)
                }
            }
        }
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }
}
