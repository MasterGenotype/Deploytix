//! Keep the host awake for the duration of a long-running operation.
//!
//! An install spends most of its wall-clock time in phases with no user
//! input at all — `basestrap` pulling packages over the network, `mkinitcpio`,
//! `grub-mkconfig`.  Left alone, the machine running the installer will blank
//! its console (kernel VT blanking, 10 minutes by default), blank and DPMS-off
//! its X display (X's idle timer counts *input events*, so a repainting GUI
//! does not reset it), and — where elogind is running — take its configured
//! idle action or suspend on a lid close.
//!
//! A blanked screen is the dangerous case in practice: it reads as a hang, and
//! the user power-cycles the machine in the middle of `basestrap`.
//!
//! [`keep_awake`] acquires every inhibitor it can and releases them on drop.
//! Every layer is independent and strictly best-effort: a host without `xset`,
//! without elogind, or without a writable console simply gets the layers it can
//! support.  Nothing here is ever fatal to an install.
//!
//! Deliberately *not* covered:
//!
//! - `shutdown` is excluded from the elogind inhibit mask.  Blocking sleep is
//!   helpful; blocking the user's own shutdown is hostile.
//! - Desktop power managers (powerdevil, xfce4-power-manager) are reachable
//!   only through `org.freedesktop.ScreenSaver`, whose inhibit is scoped to the
//!   *caller's* D-Bus connection.  A one-shot `dbus-send` releases it the
//!   instant the command exits, so it would be pure theatre; a real inhibit
//!   would mean linking a D-Bus client into a static musl build.
//! - `/sys/power/wake_lock` needs `CONFIG_PM_WAKELOCKS` and only guards kernel
//!   autosleep, which desktop distributions do not use.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::process::{Child, Command, Stdio};

use tracing::debug;

use crate::utils::command::command_exists;

/// Console escapes that disable VT blanking: `ESC[9;0]` sets the blank timeout
/// to zero (never) and `ESC[14;0]` disables the VESA powerdown timeout.
const BLANK_OFF: &[u8] = b"\x1b[9;0]\x1b[14;0]";

/// The kernel's own defaults, restored on release: 10 minutes for both.
const BLANK_DEFAULT: &[u8] = b"\x1b[9;10]\x1b[14;10]";

/// Consoles to try, in order.  `/dev/tty0` is the active VT; `/dev/console`
/// is the fallback when it is absent (some containers, some serial setups).
const CONSOLES: [&str; 2] = ["/dev/tty0", "/dev/console"];

/// How long the spawned elogind inhibitor holds its lock, in seconds, if this
/// process dies without reaping it.  A week is far longer than any install and
/// short enough that a leaked helper cannot outlive a live session.
const INHIBIT_HOLD_SECS: u64 = 604_800;

/// An acquired set of idle inhibitors.  Releases them on drop.
///
/// Obtain one from [`keep_awake`] and bind it for the length of the operation
/// (`let _awake = keep_awake(...)`, not `let _ = keep_awake(...)`, which would
/// drop it immediately).
#[derive(Debug, Default)]
pub struct KeepAwake {
    /// The console whose blanking was disabled, held open for the restore.
    console: Option<File>,
    /// Set when `xset` turned the X screensaver and DPMS off.
    xset: bool,
    /// The `elogind-inhibit` / `systemd-inhibit` child holding the lock.
    inhibitor: Option<Child>,
}

/// Acquire every available idle inhibitor for `reason`.
///
/// Never fails: unavailable layers are skipped with a `debug!` note.  `reason`
/// is what elogind shows in `loginctl list-inhibitors`.
pub fn keep_awake(reason: &str) -> KeepAwake {
    let mut guard = KeepAwake::default();
    guard.console = disable_console_blanking();
    guard.xset = disable_x_screensaver();
    guard.inhibitor = spawn_logind_inhibitor(reason);

    if guard.is_empty() {
        debug!("keep-awake: no inhibitor available on this host");
    }
    guard
}

impl KeepAwake {
    /// True when no layer could be acquired.
    fn is_empty(&self) -> bool {
        self.console.is_none() && !self.xset && self.inhibitor.is_none()
    }

    /// Release every acquired inhibitor.  Idempotent, so [`Drop`] and an
    /// explicit call before a hard exit cannot double-restore.
    ///
    /// Call this explicitly on paths that terminate the process without
    /// unwinding — `signal::reraise()` raises with the default handler, so
    /// `Drop` would never run there.
    pub fn release(&mut self) {
        if let Some(mut console) = self.console.take() {
            if let Err(e) = console.write_all(BLANK_DEFAULT) {
                debug!("keep-awake: restoring console blanking failed: {}", e);
            }
        }

        if self.xset {
            self.xset = false;
            run_quietly("xset", &["s", "on", "+dpms"]);
        }

        if let Some(mut child) = self.inhibitor.take() {
            // Killing the helper closes the inhibitor fd it holds, which is
            // what actually releases the lock.  Reap it so no zombie is left
            // behind for the rest of the process's life.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        self.release();
    }
}

/// Turn off kernel VT blanking, returning the console to restore on release.
///
/// Written as raw escapes rather than via `setterm`, which needs `TERM=linux`
/// in the environment and a util-linux binary in `PATH` — neither guaranteed
/// when the installer runs from a service, a GUI session, or a rescue shell.
fn disable_console_blanking() -> Option<File> {
    for path in CONSOLES {
        match OpenOptions::new().write(true).open(path) {
            Ok(mut file) => match file.write_all(BLANK_OFF) {
                Ok(()) => {
                    debug!("keep-awake: disabled console blanking on {}", path);
                    return Some(file);
                }
                Err(e) => debug!("keep-awake: writing to {} failed: {}", path, e),
            },
            Err(e) => debug!("keep-awake: opening {} failed: {}", path, e),
        }
    }
    None
}

/// Turn off the X screensaver and DPMS, reporting whether it took effect.
///
/// Only meaningful for the GUI.  X's idle timer is driven by input events, not
/// by drawing, so a fullscreen installer repainting a progress bar for forty
/// minutes still blanks without this.
fn disable_x_screensaver() -> bool {
    if std::env::var_os("DISPLAY").is_none() {
        return false;
    }
    if !command_exists("xset") {
        debug!("keep-awake: DISPLAY is set but xset is not installed");
        return false;
    }
    if run_quietly("xset", &["s", "off", "-dpms"]) {
        debug!("keep-awake: disabled X screensaver and DPMS");
        return true;
    }
    false
}

/// Hold an elogind (or systemd) inhibitor lock for the caller's lifetime.
///
/// The lock lives as long as the file descriptor logind handed out, so it has
/// to be held by a live process: `elogind-inhibit <cmd>` keeps it for as long
/// as `<cmd>` runs.  Issuing the same `Inhibit` call over `dbus-send` would
/// release it the moment `dbus-send` exits.
fn spawn_logind_inhibitor(reason: &str) -> Option<Child> {
    let tool = ["elogind-inhibit", "systemd-inhibit"]
        .into_iter()
        .find(|t| command_exists(t))?;

    let child = Command::new(tool)
        .arg("--what=sleep:idle:handle-lid-switch")
        .arg("--who=Deploytix")
        .arg(format!("--why={reason}"))
        .arg("--mode=block")
        .arg("sleep")
        .arg(INHIBIT_HOLD_SECS.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match child {
        Ok(child) => {
            debug!("keep-awake: holding {} lock (pid {})", tool, child.id());
            Some(child)
        }
        Err(e) => {
            debug!("keep-awake: spawning {} failed: {}", tool, e);
            None
        }
    }
}

/// Run a command purely for its side effect, reporting success.
fn run_quietly(program: &str, args: &[&str]) -> bool {
    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => true,
        Ok(status) => {
            debug!("keep-awake: {} {:?} exited {}", program, args, status);
            false
        }
        Err(e) => {
            debug!("keep-awake: running {} failed: {}", program, e);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_guard_holds_nothing_and_releases_cleanly() {
        let mut guard = KeepAwake::default();
        assert!(guard.is_empty());
        guard.release();
        assert!(guard.is_empty());
    }

    #[test]
    fn release_is_idempotent() {
        // Releasing twice must not double-restore or panic; `run()` calls it
        // explicitly before re-raising a signal, and `Drop` calls it again.
        let mut guard = KeepAwake::default();
        guard.release();
        guard.release();
    }

    #[test]
    fn blank_escapes_disable_then_restore_both_timers() {
        // ESC[9;N] is the blank timeout, ESC[14;N] the VESA powerdown timeout.
        assert_eq!(BLANK_OFF, b"\x1b[9;0]\x1b[14;0]");
        assert_eq!(BLANK_DEFAULT, b"\x1b[9;10]\x1b[14;10]");
    }

    #[test]
    fn x_screensaver_skipped_without_display() {
        // Guards the ordering in `disable_x_screensaver`: the DISPLAY check
        // must come first so a CLI install never shells out to xset.
        if std::env::var_os("DISPLAY").is_none() {
            assert!(!disable_x_screensaver());
        }
    }

    #[test]
    fn keep_awake_never_fails() {
        // Whatever the host supports, acquiring must return a guard rather
        // than erroring — an install is never blocked on power management.
        let _guard = keep_awake("deploytix test");
    }
}
