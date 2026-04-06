//! Installation progress panel

use crate::gui::{state::InstallState, theme, widgets};
use egui::{RichText, Ui};

/// Render the installation progress panel.
pub fn show(ui: &mut Ui, install: &InstallState) {
    if install.finished {
        widgets::page_heading(ui, "Installation Complete");
    } else {
        widgets::page_heading(ui, "Installing...");
    }

    if let Some(ref err) = install.error {
        widgets::section(ui, "Error", |ui| {
            ui.label(
                RichText::new(format!("\u{274c} {}", err))
                    .color(theme::ERROR)
                    .strong(),
            );
        });
    } else if install.finished {
        widgets::section(ui, "Success", |ui| {
            ui.label(
                RichText::new("\u{2713} Installation completed successfully!")
                    .color(theme::SUCCESS)
                    .strong(),
            );
            ui.add_space(theme::SPACING_XS);
            ui.label("You can now reboot into your new Artix Linux system.");
        });
    } else {
        widgets::section(ui, "Progress", |ui| {
            ui.label(&install.status);
            ui.add_space(theme::SPACING_SM);
            ui.add(egui::ProgressBar::new(install.progress).show_percentage());
        });
    }

    ui.add_space(theme::SPACING_SM);

    widgets::section(ui, "Log", |ui| {
        let scroll = egui::ScrollArea::vertical()
            .max_height(280.0)
            .auto_shrink([false, false])
            .stick_to_bottom(true);

        scroll.show(ui, |ui| {
            for msg in &install.logs {
                ui.label(
                    RichText::new(msg)
                        .monospace()
                        .size(11.0)
                        .color(theme::TEXT_SECONDARY),
                );
            }
            if !install.logs.is_empty() {
                ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
            }
        });
    });
}
