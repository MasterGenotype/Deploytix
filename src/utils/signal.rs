//! Signal handling for graceful cleanup on interruption.
//!
//! Installs SIGINT/SIGTERM handlers so that the installer can perform
//! emergency cleanup (unmount, close LUKS, deactivate LVM) before exiting.
//!
//! - First signal: sets the interrupted flag; the running command finishes
//!   or fails, then emergency cleanup runs.
//! - Second signal: restores the default handler and re-raises, forcing
//!   immediate termination.
//!
//! # Two modes
//!
//! The two-stage behaviour above suits the installer. Its main loop polls
//! [`is_interrupted`], so it gets a chance to unmount and close LUKS.
//!
//! It does not suit the GUI binaries. eframe's event loop never polls the flag,
//! so the first Ctrl+C would print "cleaning up" and then nothing would happen
//! until a second one. [`install_terminating_handlers`] installs the same
//! handler with the first signal fatal, so Ctrl+C still kills the GUI.
//!
//! If the GUI then starts an install, `Installer::run` calls
//! [`install_signal_handlers`], which switches back to two-stage mode. That is
//! the right way round: unmounting disks matters more than the GUI exiting
//! promptly.
//!
//! # Registered cleanup path
//!
//! [`register_cleanup_path`] records one path. The handler calls `unlink(2)` on
//! it before anything else, on every signal.
//!
//! This is how the GUIs' lock file in `/tmp` gets removed when the process is
//! killed, since `Drop` does not run then. `unlink` is one of the few calls
//! that is safe to make from a signal handler, which is what makes this
//! possible.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

/// Set to `true` by the signal handler on the first SIGINT/SIGTERM.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Counts how many signals have been received.
static SIGNAL_COUNT: AtomicUsize = AtomicUsize::new(0);

/// The signal number that triggered the first interruption.
static CAUGHT_SIGNAL: AtomicUsize = AtomicUsize::new(0);

/// When set, the first signal terminates instead of flagging for cleanup.
static TERMINATE_ON_FIRST: AtomicBool = AtomicBool::new(false);

/// NUL-terminated path the handler unlinks, or null. Read from a signal
/// handler, so it must be a plain pointer to memory that is never freed.
static CLEANUP_PATH: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

/// Record a file to `unlink(2)` from the signal handler, replacing any previous
/// registration. Returns `false` if the path cannot be represented as a C
/// string (an interior NUL), in which case nothing is registered.
///
/// The allocation is deliberately leaked: a handler can fire at any instant,
/// including while this function runs, so freeing the previous pointer would
/// risk the handler reading freed memory. One small leak per registration —
/// and registration happens once per process — is the price of being callable
/// from a signal handler at all.
pub fn register_cleanup_path(path: &Path) -> bool {
    match CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => {
            CLEANUP_PATH.store(c.into_raw(), Ordering::SeqCst);
            true
        }
        Err(_) => false,
    }
}

/// Stop unlinking a previously registered path.
///
/// Called when a lock is released normally, so a later signal cannot delete a
/// file that by then belongs to a different process.
pub fn unregister_cleanup_path() {
    CLEANUP_PATH.store(std::ptr::null_mut(), Ordering::SeqCst);
}

/// `unlink(2)` the registered path, if any. Async-signal-safe: an atomic load
/// and one syscall, no allocation and no locking.
fn unlink_registered_path() {
    let p = CLEANUP_PATH.load(Ordering::SeqCst);
    if !p.is_null() {
        unsafe {
            libc::unlink(p);
        }
    }
}

/// Signal handler (async-signal-safe).
///
/// First invocation: sets the `INTERRUPTED` flag and writes a short message
/// to stderr using raw `write(2, …)` (which is async-signal-safe).
///
/// Second invocation: restores `SIG_DFL` and re-raises, so the process
/// terminates immediately with the correct signal status.
/// Whether this signal is the one the process dies on.
///
/// `prev` is how many signals arrived before this one. In two-stage mode the
/// first signal only flags interruption and the process keeps running; every
/// other case ends it. Split out from [`handle_signal`] so it can be tested
/// without raising a real signal or touching the global flags.
const fn is_fatal_signal(prev: usize, terminate_on_first: bool) -> bool {
    prev != 0 || terminate_on_first
}

extern "C" fn handle_signal(sig: libc::c_int) {
    let prev = SIGNAL_COUNT.fetch_add(1, Ordering::SeqCst);

    if !is_fatal_signal(prev, TERMINATE_ON_FIRST.load(Ordering::SeqCst)) {
        // First signal in two-stage mode. The process keeps running, so the
        // registered file must stay: deleting it here would release the name
        // while this process still holds the lock, and a second instance could
        // start alongside the one that is cleaning up. Normal exit removes it
        // through Drop; a second signal removes it below.
        INTERRUPTED.store(true, Ordering::SeqCst);
        CAUGHT_SIGNAL.store(sig as usize, Ordering::SeqCst);

        let msg = b"\nInterrupt received, cleaning up...\n";
        unsafe {
            libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
        }
    } else {
        // The process is about to die and no destructor will run, so this is
        // the last chance to remove the registered file.
        unlink_registered_path();

        // Second (or later) signal — force-exit.
        let msg = b"\nForced exit - cleanup may be incomplete. Run: deploytix cleanup\n";
        unsafe {
            libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }
}

/// Restores the signal mode that was in force before it was created.
///
/// Without this, a GUI that runs one install spends the rest of its life in
/// two-stage mode: Ctrl+C prints "cleaning up" and nothing happens, because
/// eframe never polls [`is_interrupted`].
#[must_use = "dropping this immediately restores the previous signal mode"]
pub struct ModeGuard(bool);

impl Drop for ModeGuard {
    fn drop(&mut self) {
        TERMINATE_ON_FIRST.store(self.0, Ordering::SeqCst);
    }
}

/// Install signal handlers for SIGINT and SIGTERM in two-stage mode: the first
/// signal flags interruption for the main loop to act on, the second forces
/// exit. Callers must poll [`is_interrupted`].
///
/// Hold the returned guard for as long as two-stage handling is wanted. When it
/// drops, the previous mode comes back.
///
/// Safe to call more than once (idempotent).
pub fn install_signal_handlers() -> ModeGuard {
    let previous = TERMINATE_ON_FIRST.swap(false, Ordering::SeqCst);
    install_handlers();
    ModeGuard(previous)
}

/// Install the same handlers in terminating mode: the first signal unlinks the
/// registered cleanup path and then kills the process with the signal's own
/// default action.
///
/// For programs with an event loop that does not poll [`is_interrupted`] — the
/// GUI binaries — where two-stage mode would make Ctrl+C appear to do nothing.
pub fn install_terminating_handlers() {
    TERMINATE_ON_FIRST.store(true, Ordering::SeqCst);
    install_handlers();
}

fn install_handlers() {
    unsafe {
        libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
    }
}

/// Returns `true` if an interrupt signal has been received.
pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// Re-raise the caught signal with the default handler so the process exits
/// with the correct signal status (visible to the parent shell).
///
/// Does nothing if no signal was caught.
pub fn reraise() {
    let sig = CAUGHT_SIGNAL.load(Ordering::SeqCst);
    if sig != 0 {
        unsafe {
            libc::signal(sig as libc::c_int, libc::SIG_DFL);
            libc::raise(sig as libc::c_int);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registered file may only be deleted when the process is actually
    /// about to die. Deleting it on a first signal in two-stage mode would
    /// release the name while this process still holds the lock, letting a
    /// second instance start alongside the one that is cleaning up.
    #[test]
    fn the_file_is_only_removed_when_the_process_is_dying() {
        // Two-stage mode: the first signal flags and returns, so the process
        // lives on and must keep its lock file.
        assert!(!is_fatal_signal(0, false), "first signal only flags");
        // A second signal force-exits, so this is the last chance to clean up.
        assert!(is_fatal_signal(1, false));
        assert!(is_fatal_signal(2, false));
        // Terminating mode: the first signal is already fatal.
        assert!(is_fatal_signal(0, true));
        assert!(is_fatal_signal(1, true));
    }

    /// A path with an interior NUL cannot become a C string, and registering
    /// one must fail loudly rather than leave a half-set pointer the handler
    /// would dereference.
    #[test]
    fn a_path_with_an_interior_nul_is_rejected() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;
        let bad = OsStr::from_bytes(b"/tmp/bad\0name.lock");
        assert!(!register_cleanup_path(Path::new(bad)));
    }

    #[test]
    fn an_ordinary_path_registers() {
        assert!(register_cleanup_path(Path::new(
            "/tmp/deploytix-signal-test.lock"
        )));
        // Leave no registration behind for other tests in this process.
        unregister_cleanup_path();
    }

    /// The handler reads this pointer with a bare atomic load and one syscall,
    /// which is what makes it safe to call from a signal context at all.
    /// Unlinking a null registration must be a no-op rather than a crash.
    #[test]
    fn unlinking_with_nothing_registered_is_harmless() {
        unregister_cleanup_path();
        unlink_registered_path();
    }

    /// The registered file is actually removed — the whole point of the
    /// mechanism. Calls the unlink directly rather than raising a real signal,
    /// which would kill the test runner.
    #[test]
    fn the_registered_file_is_unlinked() {
        let p = std::env::temp_dir().join(format!(
            "deploytix-signal-unlink-{}.lock",
            std::process::id()
        ));
        std::fs::write(&p, b"").unwrap();
        assert!(p.exists());

        assert!(register_cleanup_path(&p));
        unlink_registered_path();
        assert!(!p.exists(), "the handler's unlink must remove the file");

        unregister_cleanup_path();
    }

    /// The installer's two-stage handling must outrank the GUI's terminating
    /// mode: `Installer::run` calls `install_signal_handlers` after the GUI has
    /// already installed its own, and unmounting filesystems and closing LUKS
    /// on the first Ctrl+C matters more than the GUI dying promptly.
    ///
    /// Installs real handlers in the test process, which is harmless: nothing
    /// in the suite raises SIGINT or SIGTERM.
    #[test]
    fn an_install_switches_to_two_stage_and_back() {
        install_terminating_handlers();
        assert!(
            TERMINATE_ON_FIRST.load(Ordering::SeqCst),
            "the GUI asks for the first signal to be fatal"
        );

        {
            let _guard = install_signal_handlers();
            assert!(
                !TERMINATE_ON_FIRST.load(Ordering::SeqCst),
                "an install must get its chance to clean up first"
            );
        }

        assert!(
            TERMINATE_ON_FIRST.load(Ordering::SeqCst),
            "and the GUI gets its Ctrl+C back when the install is done"
        );
    }

    /// After a lock is released the path may belong to another process, so a
    /// later signal must not delete it.
    #[test]
    fn a_deregistered_path_is_left_alone() {
        let p =
            std::env::temp_dir().join(format!("deploytix-signal-keep-{}.lock", std::process::id()));
        std::fs::write(&p, b"").unwrap();

        assert!(register_cleanup_path(&p));
        unregister_cleanup_path();
        unlink_registered_path();

        assert!(p.exists(), "a deregistered path must survive");
        let _ = std::fs::remove_file(&p);
    }
}
