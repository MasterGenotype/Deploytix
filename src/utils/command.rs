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

/// Shell that mounts the API filesystems a chroot needs (`/proc`, `/sys`,
/// `/dev`, `/run`, `/tmp`), mirroring what `artix-chroot` does internally.
///
/// This is only used on the plain-`chroot` fallback path: `artools-base` is a
/// host/ISO dependency and is *not* installed into deployed systems, so
/// `deploytix update` / `rollback` on an immutable install always lands here.
/// Without `/proc`, `/etc/mtab` (a symlink to `../proc/self/mounts`) dangles and
/// pacman aborts with "could not determine filesystem mount points".
///
/// Every mount is best-effort: an already-mounted or unsupported one must not
/// abort the whole setup, and a genuinely missing mount surfaces as the chrooted
/// command's own error.
pub fn chroot_api_setup_cmd(chroot_path: &str) -> String {
    format!(
        "t={t}; mkdir -p \"$t/proc\" \"$t/sys\" \"$t/dev\" \"$t/run\" \"$t/tmp\" 2>/dev/null; \
         mount -t proc proc \"$t/proc\" -o nosuid,noexec,nodev 2>/dev/null || true; \
         mount -t sysfs sys \"$t/sys\" -o nosuid,noexec,nodev,ro 2>/dev/null || true; \
         if [ -d /sys/firmware/efi/efivars ]; then \
             mkdir -p \"$t/sys/firmware/efi/efivars\" 2>/dev/null; \
             mount -t efivarfs efivarfs \"$t/sys/firmware/efi/efivars\" -o nosuid,noexec,nodev 2>/dev/null || true; \
         fi; \
         mount -t devtmpfs udev \"$t/dev\" -o mode=0755,nosuid 2>/dev/null || \
             mount --rbind /dev \"$t/dev\" 2>/dev/null || true; \
         mkdir -p \"$t/dev/pts\" \"$t/dev/shm\" 2>/dev/null; \
         mount -t devpts devpts \"$t/dev/pts\" -o mode=0620,gid=5,nosuid,noexec 2>/dev/null || true; \
         mount -t tmpfs shm \"$t/dev/shm\" -o mode=1777,nosuid,nodev 2>/dev/null || true; \
         mount --bind /run \"$t/run\" 2>/dev/null || true; \
         mount -t tmpfs tmp \"$t/tmp\" -o mode=1777,strictatime,nodev,nosuid 2>/dev/null || true; \
         true",
        t = chroot_path,
    )
}

/// Shell that releases whatever [`chroot_api_setup_cmd`] mounted, innermost
/// first. Lazy unmount is the fallback so a busy mount never wedges the target.
pub fn chroot_api_teardown_cmd(chroot_path: &str) -> String {
    format!(
        "t={t}; for m in tmp run dev/shm dev/pts dev sys/firmware/efi/efivars sys proc; do \
             umount -R \"$t/$m\" 2>/dev/null || umount -Rl \"$t/$m\" 2>/dev/null || true; \
         done; true",
        t = chroot_path,
    )
}

/// Run a shell snippet, discarding its outcome. Used for best-effort chroot
/// mount setup/teardown, which must never mask the chrooted command's result.
fn run_shell_quietly(script: &str) {
    match Command::new("sh")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) if !out.status.success() => {
            debug!(
                "chroot mount helper returned {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Err(e) => debug!("chroot mount helper failed to run: {}", e),
        _ => {}
    }
}

/// Ensure `<chroot>/etc/mtab` exists, as the `filesystem` package ships it.
///
/// pacman reads it to enumerate mount points before committing a transaction;
/// if a target's `/etc` lacks the link entirely, no amount of mounting `/proc`
/// helps. Best-effort: a read-only or absent `/etc` is not fatal here, the
/// chrooted command reports the real problem.
fn ensure_chroot_mtab(chroot_path: &str) {
    let etc = std::path::Path::new(chroot_path).join("etc");
    if !etc.is_dir() {
        return;
    }
    let mtab = etc.join("mtab");
    // symlink_metadata, not exists(): a dangling symlink is still "present".
    if std::fs::symlink_metadata(&mtab).is_ok() {
        return;
    }
    if let Err(e) = std::os::unix::fs::symlink("../proc/self/mounts", &mtab) {
        debug!("could not create {}: {}", mtab.display(), e);
    }
}

/// Run a command in chroot using artix-chroot (if available) or plain chroot.
///
/// `artix-chroot` mounts the API filesystems itself; the plain-`chroot`
/// fallback does not, so we set them up (and tear them down) around the
/// command — otherwise pacman and mkinitcpio fail inside the chroot.
pub fn run_in_artix_chroot(chroot_path: &str, command: &str) -> Result<Output> {
    if command_exists("artix-chroot") {
        ensure_chroot_mtab(chroot_path);
        run_command("artix-chroot", &[chroot_path, "bash", "-c", command])
    } else {
        // Fallback to plain chroot: provide the mounts artix-chroot would have.
        debug!("artix-chroot not found; using plain chroot with API mounts");
        run_shell_quietly(&chroot_api_setup_cmd(chroot_path));
        ensure_chroot_mtab(chroot_path);
        let result = run_command("chroot", &[chroot_path, "bash", "-c", command]);
        run_shell_quietly(&chroot_api_teardown_cmd(chroot_path));
        result
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

    fn assert_valid_shell(script: &str) {
        if let Ok(status) = Command::new("sh").arg("-n").arg("-c").arg(script).status() {
            assert!(status.success(), "not valid shell:\n{script}");
        }
    }

    #[test]
    fn api_setup_mounts_everything_pacman_and_mkinitcpio_need() {
        let s = chroot_api_setup_cmd("/run/deploytix-update/42");
        // /proc is the one that made pacman fail: /etc/mtab -> ../proc/self/mounts.
        assert!(s.contains("mount -t proc proc \"$t/proc\""));
        assert!(s.contains("mount -t sysfs sys \"$t/sys\""));
        assert!(s.contains("mount -t devtmpfs udev \"$t/dev\""));
        assert!(s.contains("mount -t devpts devpts \"$t/dev/pts\""));
        assert!(s.contains("mount -t tmpfs shm \"$t/dev/shm\""));
        // /run is a plain bind, never rbind: the target lives under /run itself.
        assert!(s.contains("mount --bind /run \"$t/run\""));
        assert!(!s.contains("mount --rbind /run"));
        assert_valid_shell(&s);
    }

    #[test]
    fn api_setup_is_best_effort_and_never_aborts() {
        let s = chroot_api_setup_cmd("/mnt/target");
        assert!(!s.contains("set -e"));
        assert!(s.ends_with("true"));
        assert_valid_shell(&s);
    }

    #[test]
    fn api_teardown_unmounts_innermost_first() {
        let s = chroot_api_teardown_cmd("/run/deploytix-update/42");
        let list: Vec<&str> = s
            .split("for m in ")
            .nth(1)
            .and_then(|r| r.split(';').next())
            .unwrap_or_default()
            .split_whitespace()
            .take_while(|w| *w != "do")
            .collect();
        assert_eq!(
            list,
            vec![
                "tmp",
                "run",
                "dev/shm",
                "dev/pts",
                "dev",
                "sys/firmware/efi/efivars",
                "sys",
                "proc"
            ]
        );
        // Lazy unmount is the fallback so a busy mount cannot wedge the target.
        assert!(s.contains("umount -Rl"));
        assert_valid_shell(&s);
    }

    #[test]
    fn ensure_mtab_creates_link_only_when_absent() {
        let base = std::env::temp_dir().join(format!("deploytix-mtab-{}", std::process::id()));
        let etc = base.join("etc");
        std::fs::create_dir_all(&etc).unwrap();

        ensure_chroot_mtab(base.to_str().unwrap());
        let link = std::fs::read_link(etc.join("mtab")).unwrap();
        assert_eq!(link.to_str().unwrap(), "../proc/self/mounts");

        // Idempotent: an existing (here dangling) link is left alone.
        ensure_chroot_mtab(base.to_str().unwrap());
        assert!(std::fs::symlink_metadata(etc.join("mtab")).is_ok());

        // No /etc at all → no-op, no panic.
        let empty = base.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        ensure_chroot_mtab(empty.to_str().unwrap());
        assert!(std::fs::symlink_metadata(empty.join("etc/mtab")).is_err());

        std::fs::remove_dir_all(&base).ok();
    }
}
