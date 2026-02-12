//! Deploytix GUI - Graphical installer for Artix Linux
//!
//! This is the entry point for the GUI version of Deploytix.

use deploytix::gui::DeploytixGui;
use eframe::egui;

fn main() -> eframe::Result<()> {
    // Set up logging
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_target(false)
        .init();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Deploytix - Artix Linux Installer")
            .with_inner_size([800.0, 600.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Deploytix",
        options,
        Box::new(|cc| Ok(Box::new(DeploytixGui::new(cc)))),
    )
}
