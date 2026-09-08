//! Deploytix Update - graphical transactional updater for immutable installs.

use deploytix::gui_update::UpdateGui;
use deploytix::utils::single_instance::{InstanceLock, LockError};
use eframe::egui;

/// Lock file enforcing a single running instance.
///
/// Two concurrent updates would interleave their `pacman -Q` brackets against
/// the shared `/var` database and record each other's changes, so this is
/// correctness, not just tidiness.
const LOCK_PATH: &str = "/tmp/deploytix-update-gui.lock";

fn main() -> eframe::Result<()> {
    // Held for the life of the process. The lock is an flock on the file, not
    // the file's existence, so a previous instance that was killed rather than
    // closed leaves at most a stray file — never a lock that blocks startup.
    // Removed on a clean exit.
    // Terminating mode: eframe's event loop never polls `is_interrupted`, so
    // the installer's two-stage handling would make the first Ctrl+C appear to
    // hang. The handler still unlinks the lock file registered just below.
    // `Installer::run` switches back to two-stage mode if an install starts.
    deploytix::utils::signal::install_terminating_handlers();

    let _lock = match InstanceLock::acquire(LOCK_PATH) {
        Ok(l) => l,
        Err(LockError::AlreadyRunning) => {
            eprintln!("Deploytix Update is already running.");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to take the single-instance lock {LOCK_PATH}: {e}");
            std::process::exit(1);
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .init();

    // A windowed utility, not the installer's fullscreen wizard.
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Deploytix Update")
            .with_inner_size([900.0, 700.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Deploytix Update",
        options,
        Box::new(|cc| Ok(Box::new(UpdateGui::new(cc)))),
    )
}
