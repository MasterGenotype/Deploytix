//! Deploytix Update - graphical transactional updater for immutable installs.

use deploytix::gui_update::UpdateGui;
use eframe::egui;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;

/// Lock file enforcing a single running instance.
///
/// Two concurrent updates would interleave their `pacman -Q` brackets against
/// the shared `/var` database and record each other's changes, so this is
/// correctness, not just tidiness.
const LOCK_PATH: &str = "/tmp/deploytix-update-gui.lock";

fn main() -> eframe::Result<()> {
    let lock_result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(LOCK_PATH);

    let _lock_file: File = match lock_result {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            eprintln!("Deploytix Update is already running (lock file {LOCK_PATH} exists).");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to create lock file {LOCK_PATH}: {e}");
            std::process::exit(1);
        }
    };

    struct LockGuard;
    impl Drop for LockGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(LOCK_PATH);
        }
    }
    let _guard = LockGuard;

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
