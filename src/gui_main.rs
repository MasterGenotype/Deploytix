//! Deploytix GUI - Graphical installer for Artix Linux
//!
//! This is the entry point for the GUI version of Deploytix.

use deploytix::gui::DeploytixGui;
use eframe::egui;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;

/// Lock file path used to enforce a single running instance.
const LOCK_PATH: &str = "/tmp/deploytix-gui.lock";

fn main() -> eframe::Result<()> {
    // Enforce single instance via an exclusive lock file.
    // O_CREAT | O_EXCL fails if the file already exists.
    let lock_result = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(LOCK_PATH);

    let _lock_file: File = match lock_result {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            eprintln!("Deploytix GUI is already running (lock file {LOCK_PATH} exists).");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to create lock file {LOCK_PATH}: {e}");
            std::process::exit(1);
        }
    };

    // Ensure the lock file is removed on exit (normal or panic).
    struct LockGuard;
    impl Drop for LockGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(LOCK_PATH);
        }
    }
    let _guard = LockGuard;

    // Set up logging
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Deploytix - Artix Linux Installer")
            .with_min_inner_size([640.0, 480.0])
            .with_maximized(true),
        ..Default::default()
    };

    eframe::run_native(
        "Deploytix",
        options,
        Box::new(|cc| Ok(Box::new(DeploytixGui::new(cc)))),
    )
}
