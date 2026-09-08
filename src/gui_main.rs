//! Deploytix GUI - Graphical installer for Artix Linux
//!
//! This is the entry point for the GUI version of Deploytix.

use deploytix::gui::DeploytixGui;
use deploytix::utils::single_instance::{InstanceLock, LockError};
use eframe::egui;

/// Lock file path used to enforce a single running instance.
const LOCK_PATH: &str = "/tmp/deploytix-gui.lock";

fn main() -> eframe::Result<()> {
    // Enforce single instance via an exclusive lock file.
    // O_CREAT | O_EXCL fails if the file already exists.
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
            eprintln!("Deploytix GUI is already running.");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to take the single-instance lock {LOCK_PATH}: {e}");
            std::process::exit(1);
        }
    };

    // Set up logging before audio so warnings are visible
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .init();

    // Start looping theme music (runs in background; stops when handle drops)
    let _audio = deploytix::resources::audio::play_theme_loop();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Deploytix - Artix Linux Installer")
            .with_min_inner_size([640.0, 480.0])
            .with_fullscreen(true),
        ..Default::default()
    };

    eframe::run_native(
        "Deploytix",
        options,
        Box::new(|cc| Ok(Box::new(DeploytixGui::new(cc)))),
    )
}
