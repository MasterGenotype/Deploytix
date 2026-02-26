//! Command execution utilities

use crate::utils::error::{DeploytixError, Result};
use std::process::{Command, Output, Stdio};
use tracing::{debug, warn};

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

/// Execute a command and return stdout as string
#[allow(dead_code)]
pub fn run_command_output(program: &str, args: &[&str]) -> Result<String> {
    let output = run_command(program, args)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Execute a command, allowing it to fail (returns None on failure)
#[allow(dead_code)]
pub fn run_command_optional(program: &str, args: &[&str]) -> Option<String> {
    run_command_output(program, args).ok()
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

/// Run a command in a chroot environment
#[allow(dead_code)]
pub fn run_in_chroot(chroot_path: &str, program: &str, args: &[&str]) -> Result<Output> {
    let mut chroot_args = vec![chroot_path, program];
    chroot_args.extend(args);
    run_command("chroot", &chroot_args)
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

/// Wrapper for command execution that respects dry-run mode
pub struct CommandRunner {
    dry_run: bool,
}

impl CommandRunner {
    pub fn new(dry_run: bool) -> Self {
        Self { dry_run }
    }

    pub fn run(&self, program: &str, args: &[&str]) -> Result<Option<Output>> {
        if crate::utils::signal::is_interrupted() {
            return Err(DeploytixError::Interrupted);
        }
        if self.dry_run {
            log_dry_run(program, args);
            Ok(None)
        } else {
            run_command(program, args).map(Some)
        }
    }

    #[allow(dead_code)]
    pub fn run_output(&self, program: &str, args: &[&str]) -> Result<Option<String>> {
        if self.dry_run {
            log_dry_run(program, args);
            Ok(None)
        } else {
            run_command_output(program, args).map(Some)
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
            run_in_artix_chroot(chroot_path, command).map(Some)
        }
    }

    /// Run a command regardless of interrupt state.
    /// Used for cleanup operations that must execute even after a signal.
    pub fn force_run(&self, program: &str, args: &[&str]) -> Result<Option<Output>> {
        if self.dry_run {
            log_dry_run(program, args);
            Ok(None)
        } else {
            run_command(program, args).map(Some)
        }
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }
}
