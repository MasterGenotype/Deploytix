//! System overview: which backend, what is booted, what is staged.

use crate::gui::{theme, widgets};
use crate::gui_update::state::AppState;
use egui::{RichText, Ui};

pub fn show(ui: &mut Ui, state: &mut AppState) {
    widgets::page_heading(ui, "System");

    if let Some(err) = state.refresh_error.clone() {
        widgets::section(ui, "Could not read system state", |ui| {
            ui.label(RichText::new(err).color(theme::ERROR));
            ui.add_space(theme::SPACING_SM);
            if ui.button("Try again").clicked() {
                state.start_refresh();
            }
        });
        return;
    }

    let Some(info) = state.info.clone() else {
        widgets::info_text(ui, "Reading system state...");
        return;
    };

    widgets::section(ui, "Transactional backend", |ui| {
        row(ui, "Backend", info.backend.label());
        row(
            ui,
            "Currently booted",
            &describe_target(&info.running, info.backend.target_noun()),
        );
        row(
            ui,
            "Next boot",
            &describe_target(&info.pointer, info.backend.target_noun()),
        );
    });

    ui.add_space(theme::SPACING_SM);

    if info.has_staged_update() {
        widgets::section(ui, "Pending", |ui| {
            ui.label(
                RichText::new("\u{26a0} An update is staged and takes effect on the next reboot.")
                    .color(theme::WARNING)
                    .strong(),
            );
        });
        ui.add_space(theme::SPACING_SM);
    }

    widgets::section(ui, "How updates work here", |ui| {
        ui.label(
            RichText::new(
                "/ and /usr are read-only, so packages are never installed into the running \
                 system. Each update builds a new snapshot and switches to it on reboot; if \
                 something breaks, roll back from the Snapshots tab.",
            )
            .color(theme::TEXT_SECONDARY),
        );
        ui.add_space(theme::SPACING_XS);
        ui.label(
            RichText::new(
                "The package database lives on the shared /var and is not snapshotted, so after \
                 a rollback pacman still lists the newer versions even though the older files \
                 are back.",
            )
            .color(theme::TEXT_MUTED)
            .size(11.0),
        );
    });

    ui.add_space(theme::SPACING_MD);
    if ui.button("Refresh").clicked() {
        state.start_refresh();
    }
}

/// `@` and slot letters need expanding; a set id is already dated by its row.
fn describe_target(target: &str, noun: &str) -> String {
    match target {
        "@" => "Base install".to_string(),
        "A" | "B" => format!("Slot {target}"),
        id => format!("{noun} {id}"),
    }
}

fn row(ui: &mut Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{label}:")).color(theme::TEXT_SECONDARY));
        ui.label(RichText::new(value).strong());
    });
}
