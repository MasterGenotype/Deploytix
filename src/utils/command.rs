//! Command execution utilities

use crate::utils::error::{DeploytixError, Result};
use std::io::Write;
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
        warn!("Command failed: {} {}\n  stderr: {}", program, args.join(" "), stderr.trim());
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

/// Run a command in chroot with text supplied via stdin (avoids sensitive data on command line)
pub fn run_in_artix_chroot_stdin(chroot_path: &str, command: &str, stdin_text: &str) -> Result<Output> {
    let program = if command_exists("artix-chroot") {
        "artix-chroot"
    } else {
        "chroot"
    };

    debug!("Running: {} {} bash -c '{}' (with stdin)", program, chroot_path, command);

    let mut child = Command::new(program)
        .args([chroot_path, "bash", "-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DeploytixError::CommandNotFound(program.to_string())
            } else {
                DeploytixError::Io(e)
            }
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(stdin_text.as_bytes()).map_err(DeploytixError::Io)?;
    }

    let output = child.wait_with_output().map_err(DeploytixError::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        warn!(
            "Command failed: {} {} bash -c '{}'\n  stderr: {}",
            program, chroot_path, command, stderr.trim()
        );
        return Err(DeploytixError::CommandFailed {
            command: format!("{} {} bash -c '{}'", program, chroot_path, command),
            stderr,
        });
    }

    Ok(output)
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
        if self.dry_run {
            println!("  [dry-run] chroot {} bash -c '{}'", chroot_path, command);
            Ok(None)
        } else {
            run_in_artix_chroot(chroot_path, command).map(Some)
        }
    }

    /// Run a command in chroot, passing sensitive text via stdin instead of the command line.
    /// Use this for passwords and other credentials to avoid exposure in process listings.
    pub fn run_in_chroot_stdin(
        &self,
        chroot_path: &str,
        command: &str,
        stdin_text: &str,
    ) -> Result<Option<Output>> {
        if self.dry_run {
            println!("  [dry-run] chroot {} bash -c '{}' (with stdin input)", chroot_path, command);
            Ok(None)
        } else {
            run_in_artix_chroot_stdin(chroot_path, command, stdin_text).map(Some)
        }
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }
}
