//! Command execution utilities

use crate::utils::error::{DeploytixError, Result};
use crate::utils::interactive::{PacmanDecision, PacmanInvocation, PolicyHandle};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

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

/// Execute a command, writing `stdin_data` to its standard input.
///
/// Used for credentials (`chpasswd`, `cryptsetup` passphrases): the secret
/// travels over a pipe rather than appearing in argv, where it would be
/// visible in `/proc/<pid>/cmdline` to any local user for the lifetime of
/// the process.  `stdin_data` is never logged and never reaches an
/// [`OperationRecord`].
pub fn run_command_with_stdin(program: &str, args: &[&str], stdin_data: &str) -> Result<Output> {
    use std::io::Write;

    debug!("Running (stdin piped): {} {}", program, args.join(" "));

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DeploytixError::CommandNotFound(program.to_string())
            } else {
                DeploytixError::Io(e)
            }
        })?;

    // Take the handle so it is dropped (closing the pipe) before we wait —
    // otherwise a child reading to EOF would block forever.
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| DeploytixError::CommandFailed {
                command: program.to_string(),
                stderr: "failed to open stdin pipe".to_string(),
            })?;
        // A child that exits before reading (a failing `chpasswd`, a
        // missing binary behind a wrapper) gives us EPIPE here.  That is
        // not the interesting error: swallow it so `wait_with_output`
        // below can report the real exit status and stderr instead.
        if let Err(e) = stdin.write_all(stdin_data.as_bytes()) {
            if e.kind() != std::io::ErrorKind::BrokenPipe {
                return Err(DeploytixError::Io(e));
            }
        }
    }

    let output = child.wait_with_output()?;

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

/// Run a shell command string in the chroot via `bash -c`.
///
/// **This is the shell escape hatch.** Prefer [`run_in_artix_chroot_argv`]
/// (or [`CommandRunner::run_in_chroot_argv`]) for anything that does not
/// genuinely need pipes, redirection, `&&`, or command substitution: there
/// is no quoting layer here, so any value interpolated into `command` is
/// parsed by bash.
pub fn run_in_artix_chroot(chroot_path: &str, command: &str) -> Result<Output> {
    if command_exists("artix-chroot") {
        run_command("artix-chroot", &[chroot_path, "bash", "-c", command])
    } else {
        // Fallback to plain chroot
        run_command("chroot", &[chroot_path, "bash", "-c", command])
    }
}

/// Build the argv for running `argv` inside `chroot_path` with no shell.
fn chroot_argv<'a>(chroot_path: &'a str, argv: &[&'a str]) -> Vec<&'a str> {
    let mut full = Vec::with_capacity(argv.len() + 1);
    full.push(chroot_path);
    full.extend_from_slice(argv);
    full
}

/// Run `argv` inside the chroot **without a shell**.
///
/// Each element of `argv` reaches the target program as exactly one
/// argument, so usernames, group lists, package names and paths cannot be
/// re-parsed as shell syntax no matter what they contain.
pub fn run_in_artix_chroot_argv(chroot_path: &str, argv: &[&str]) -> Result<Output> {
    if argv.is_empty() {
        return Err(DeploytixError::CommandFailed {
            command: "<empty argv>".to_string(),
            stderr: "run_in_artix_chroot_argv called with no program".to_string(),
        });
    }
    let full = chroot_argv(chroot_path, argv);
    if command_exists("artix-chroot") {
        run_command("artix-chroot", &full)
    } else {
        run_command("chroot", &full)
    }
}

/// [`run_in_artix_chroot_argv`] with data piped to the child's stdin.
pub fn run_in_artix_chroot_argv_stdin(
    chroot_path: &str,
    argv: &[&str],
    stdin_data: &str,
) -> Result<Output> {
    if argv.is_empty() {
        return Err(DeploytixError::CommandFailed {
            command: "<empty argv>".to_string(),
            stderr: "run_in_artix_chroot_argv_stdin called with no program".to_string(),
        });
    }
    let full = chroot_argv(chroot_path, argv);
    if command_exists("artix-chroot") {
        run_command_with_stdin("artix-chroot", &full, stdin_data)
    } else {
        run_command_with_stdin("chroot", &full, stdin_data)
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
    policy: Option<PolicyHandle>,
}

impl CommandRunner {
    pub fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            recorder: None,
            policy: None,
        }
    }

    /// Attach a recording channel.  Every command execution will send an
    /// `OperationRecord` through the channel before returning.
    pub fn with_recorder(mut self, tx: Sender<OperationRecord>) -> Self {
        self.recorder = Some(tx);
        self
    }

    /// Attach an interactive policy that reviews user-facing pacman /
    /// basestrap / yay invocations before they run.  See
    /// `crate::utils::interactive` for the contract.
    pub fn with_policy(mut self, policy: PolicyHandle) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Borrow the attached policy, if any.  Used by the installer to
    /// drive the post-install extras step (phase 5.95).
    pub fn policy(&self) -> Option<&PolicyHandle> {
        self.policy.as_ref()
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

    /// Run a command with data piped to its stdin.
    ///
    /// `stdin_data` is deliberately absent from the logged and recorded
    /// command string — this is the path credentials take.
    pub fn run_with_stdin(
        &self,
        program: &str,
        args: &[&str],
        stdin_data: &str,
    ) -> Result<Option<Output>> {
        if crate::utils::signal::is_interrupted() {
            return Err(DeploytixError::Interrupted);
        }
        if self.dry_run {
            println!("  [dry-run] {} {} <<<[stdin]", program, args.join(" "));
            return Ok(None);
        }
        let cmd_str = format!("{} {} <<<[stdin]", program, args.join(" "));
        let start = Instant::now();
        match run_command_with_stdin(program, args, stdin_data) {
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

    /// Run `argv` inside the chroot with **no shell**.
    ///
    /// Prefer this over [`Self::run_in_chroot`] for every command that does
    /// not genuinely need shell syntax.  Values passed as argv elements
    /// cannot be reinterpreted as shell metacharacters.
    pub fn run_in_chroot_argv(&self, chroot_path: &str, argv: &[&str]) -> Result<Option<Output>> {
        if crate::utils::signal::is_interrupted() {
            return Err(DeploytixError::Interrupted);
        }
        let cmd_str = format!("chroot {} {}", chroot_path, argv.join(" "));
        if self.dry_run {
            println!("  [dry-run] {}", cmd_str);
            return Ok(None);
        }
        let start = Instant::now();
        match run_in_artix_chroot_argv(chroot_path, argv) {
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

    /// [`Self::run_in_chroot_argv`] with data piped to the child's stdin.
    ///
    /// `stdin_data` never appears in the log line or the
    /// [`OperationRecord`], which is what makes this safe for passwords.
    pub fn run_in_chroot_argv_stdin(
        &self,
        chroot_path: &str,
        argv: &[&str],
        stdin_data: &str,
    ) -> Result<Option<Output>> {
        if crate::utils::signal::is_interrupted() {
            return Err(DeploytixError::Interrupted);
        }
        let cmd_str = format!("chroot {} {} <<<[stdin]", chroot_path, argv.join(" "));
        if self.dry_run {
            println!("  [dry-run] {}", cmd_str);
            return Ok(None);
        }
        let start = Instant::now();
        match run_in_artix_chroot_argv_stdin(chroot_path, argv, stdin_data) {
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

    /// Run a shell command string in the chroot via `bash -c`.
    ///
    /// **Shell escape hatch** — see [`run_in_artix_chroot`].  Use
    /// [`Self::run_in_chroot_argv`] unless the command genuinely needs
    /// pipes, redirection, `&&` or command substitution.
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

    // ─── Interactive-aware install entry points ────────────────────────
    //
    // All user-facing package installs (basestrap, pacman -S in chroot,
    // yay -S as user) go through these helpers.  The attached policy (if
    // any) gets to approve / edit / skip / cancel each invocation.
    // Internal pacman housekeeping (`pacman -Sy`, `pacman-key`, the
    // signature-retry fallback) is NOT routed through here.

    /// Submit a [`PacmanInvocation`] to the attached policy (if any) and
    /// return the dispatched form.
    ///
    ///   * `Ok(Some(inv))`     — execute this (possibly edited) invocation.
    ///   * `Ok(None)`           — policy said skip; caller should no-op.
    ///   * `Err(UserCancelled)` — policy said cancel; caller bubbles.
    ///
    /// With no policy attached, the invocation is returned unchanged.
    /// Callers render the (possibly edited) result into their existing
    /// command-execution path (e.g. `pacman_install_chroot` for chroot
    /// pacman calls, the basestrap retry loop for basestrap calls).
    pub fn review_pacman(&self, inv: PacmanInvocation) -> Result<Option<PacmanInvocation>> {
        let Some(policy) = &self.policy else {
            return Ok(Some(inv));
        };
        match policy.confirm_pacman(&inv) {
            PacmanDecision::Approve => Ok(Some(inv)),
            PacmanDecision::EditedTo {
                packages,
                extra_flags,
            } => {
                let mut edited = inv;
                edited.packages = packages;
                edited.extra_flags = extra_flags;
                if edited.packages.is_empty() {
                    info!(
                        "Policy edited '{}' down to zero packages — skipping",
                        edited.label
                    );
                    return Ok(None);
                }
                Ok(Some(edited))
            }
            PacmanDecision::Skip => {
                info!("Policy skipped '{}'", inv.label);
                Ok(None)
            }
            PacmanDecision::Cancel => Err(DeploytixError::UserCancelled),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── argv construction ────────────────────────────────────────────────
    //
    // The whole point of the argv chroot API is that a value containing
    // shell metacharacters stays one argument.  These pin that down without
    // needing a chroot to execute in.

    #[test]
    fn chroot_argv_prepends_the_root() {
        assert_eq!(
            chroot_argv("/install", &["useradd", "-m", "tester"]),
            vec!["/install", "useradd", "-m", "tester"]
        );
    }

    #[test]
    fn chroot_argv_keeps_shell_metacharacters_in_one_element() {
        // A username like this would be catastrophic interpolated into a
        // `bash -c` string; as argv it is inert.
        let nasty = "tester; rm -rf /";
        let argv = chroot_argv("/install", &["useradd", "-m", nasty]);
        assert_eq!(argv.len(), 4);
        assert_eq!(argv[3], nasty);
        assert!(!argv.iter().any(|a| *a == "rm"));
    }

    #[test]
    fn chroot_argv_preserves_empty_and_spaced_arguments() {
        let argv = chroot_argv("/install", &["sh", "", "a b c"]);
        assert_eq!(argv, vec!["/install", "sh", "", "a b c"]);
    }

    #[test]
    fn empty_argv_is_rejected_rather_than_spawning_a_chroot_shell() {
        let err = run_in_artix_chroot_argv("/install", &[]).unwrap_err();
        assert!(matches!(err, DeploytixError::CommandFailed { .. }));

        let err = run_in_artix_chroot_argv_stdin("/install", &[], "x").unwrap_err();
        assert!(matches!(err, DeploytixError::CommandFailed { .. }));
    }

    // ── dry-run behaviour ────────────────────────────────────────────────

    #[test]
    fn dry_run_argv_executes_nothing_and_returns_none() {
        let cmd = CommandRunner::new(true);
        // /nonexistent is not a real root; a real execution would fail.
        let out = cmd
            .run_in_chroot_argv("/nonexistent", &["useradd", "tester"])
            .unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn dry_run_stdin_variants_execute_nothing() {
        let cmd = CommandRunner::new(true);
        assert!(cmd
            .run_in_chroot_argv_stdin("/nonexistent", &["chpasswd"], "u:p")
            .unwrap()
            .is_none());
        assert!(cmd
            .run_with_stdin("/nonexistent/bin/true", &[], "u:p")
            .unwrap()
            .is_none());
    }

    // ── secret handling ──────────────────────────────────────────────────

    #[test]
    fn stdin_payload_is_absent_from_the_recorded_command() {
        use std::sync::mpsc::channel;

        let (tx, rx) = channel();
        let cmd = CommandRunner::new(false).with_recorder(tx);

        // `cat` is present in the test image and reads stdin to EOF, which
        // also exercises the pipe-close-before-wait path.
        let out = cmd
            .run_with_stdin("cat", &[], "luks-passphrase-sentinel")
            .unwrap()
            .expect("not dry-run, so output is present");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "luks-passphrase-sentinel"
        );

        let record = rx.try_recv().expect("a record was emitted");
        assert!(
            !record.command.contains("luks-passphrase-sentinel"),
            "recorded command line leaked the stdin payload: {}",
            record.command
        );
        assert!(record.command.contains("<<<[stdin]"));
    }

    #[test]
    fn run_with_stdin_surfaces_a_nonzero_exit() {
        let cmd = CommandRunner::new(false);
        let err = cmd.run_with_stdin("false", &[], "ignored").unwrap_err();
        assert!(matches!(err, DeploytixError::CommandFailed { .. }));
    }

    #[test]
    fn run_with_stdin_reports_a_missing_program() {
        let cmd = CommandRunner::new(false);
        let err = cmd
            .run_with_stdin("deploytix-no-such-binary", &[], "x")
            .unwrap_err();
        assert!(matches!(err, DeploytixError::CommandNotFound(_)));
    }
}
