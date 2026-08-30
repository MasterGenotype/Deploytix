//! Progress and log output for a running operation.

use crate::gui::{theme, widgets};
use crate::gui_update::state::AppState;
use egui::{RichText, Ui};

pub fn show(ui: &mut Ui, state: &mut AppState) {
    widgets::page_heading(ui, "Progress");

    if let Some(err) = state.error.clone() {
        widgets::section(ui, "Failed", |ui| {
            ui.label(
                RichText::new(format!("\u{274c} {err}"))
                    .color(theme::ERROR)
                    .strong(),
            );
        });
    } else if let Some(done) = state.finished.clone() {
        widgets::section(ui, "Done", |ui| {
            ui.label(
                RichText::new(format!("\u{2713} {done}"))
                    .color(theme::SUCCESS)
                    .strong(),
            );
            ui.add_space(theme::SPACING_SM);
            if ui.button("Reboot now").clicked() {
                // Best-effort: if this fails the user is told to reboot anyway.
                let _ = std::process::Command::new("reboot").status();
            }
        });
    } else if state.busy {
        widgets::section(ui, "Working", |ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(&state.status);
            });
            ui.add_space(theme::SPACING_XS);
            // Commands are captured and reported when each finishes, so a long
            // pacman step is genuinely silent. Say so rather than looking hung.
            ui.label(
                RichText::new(
                    "Output appears as each step completes, so a large upgrade can sit quiet \
                     for several minutes.",
                )
                .color(theme::TEXT_MUTED)
                .size(11.0),
            );
        });
    } else {
        widgets::info_text(ui, "Nothing running.");
    }

    ui.add_space(theme::SPACING_SM);

    widgets::section(ui, "Log", |ui| {
        if state.logs.is_empty() {
            ui.label(
                RichText::new("No output yet.")
                    .color(theme::TEXT_MUTED)
                    .size(11.0),
            );
            return;
        }
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for entry in &state.logs {
                    ui.label(
                        RichText::new(entry)
                            .monospace()
                            .size(11.0)
                            .color(theme::TEXT_SECONDARY),
                    );
                }
            });
    });
}
