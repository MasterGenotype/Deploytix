//! Signal handling for graceful cleanup on interruption.
//!
//! Installs SIGINT/SIGTERM handlers so that the installer can perform
//! emergency cleanup (unmount, close LUKS, deactivate LVM) before exiting.
//!
//! - First signal: sets the interrupted flag; the running command finishes
//!   or fails, then emergency cleanup runs.
//! - Second signal: restores the default handler and re-raises, forcing
//!   immediate termination.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Set to `true` by the signal handler on the first SIGINT/SIGTERM.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Counts how many signals have been received.
static SIGNAL_COUNT: AtomicUsize = AtomicUsize::new(0);

/// The signal number that triggered the first interruption.
static CAUGHT_SIGNAL: AtomicUsize = AtomicUsize::new(0);

/// Signal handler (async-signal-safe).
///
/// First invocation: sets the `INTERRUPTED` flag and writes a short message
/// to stderr using raw `write(2, …)` (which is async-signal-safe).
///
/// Second invocation: restores `SIG_DFL` and re-raises, so the process
/// terminates immediately with the correct signal status.
extern "C" fn handle_signal(sig: libc::c_int) {
    let prev = SIGNAL_COUNT.fetch_add(1, Ordering::SeqCst);

    if prev == 0 {
        // First signal — flag interruption and let the main thread clean up.
        INTERRUPTED.store(true, Ordering::SeqCst);
        CAUGHT_SIGNAL.store(sig as usize, Ordering::SeqCst);

        let msg = b"\nInterrupt received, cleaning up...\n";
        unsafe {
            libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
        }
    } else {
        // Second (or later) signal — force-exit.
        let msg = b"\nForced exit - cleanup may be incomplete. Run: deploytix cleanup\n";
        unsafe {
            libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }
}

/// Install signal handlers for SIGINT and SIGTERM.
///
/// Safe to call more than once (idempotent).
pub fn install_signal_handlers() {
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
