//! Snapshot history: what each update changed, and rolling back to one.

use crate::gui::{theme, widgets};
use crate::gui_update::model::SnapshotRow;
use crate::gui_update::state::AppState;
use crate::immutable::history::Outcome;
use egui::{RichText, Ui};

pub fn show(ui: &mut Ui, state: &mut AppState) {
    widgets::page_heading(ui, "Snapshots");

    if let Some(err) = state.refresh_error.clone() {
        // Distinct from "there are none": claiming an empty history when the
        // read failed would hide a broken system behind a reassuring message.
        widgets::section(ui, "Could not read snapshots", |ui| {
            ui.label(RichText::new(err).color(theme::ERROR));
            ui.add_space(theme::SPACING_SM);
            if ui.button("Try again").clicked() {
                state.start_refresh();
            }
        });
        return;
    }

    if state.rows.is_empty() {
        if state.refreshing {
            widgets::info_text(ui, "Reading snapshots...");
        } else {
            widgets::info_text(ui, "No snapshots found yet. Run an update to create one.");
        }
        return;
    }

    ui.label(
        RichText::new(
            "Every update builds a snapshot. Expand one to see exactly which packages it changed.",
        )
        .color(theme::TEXT_SECONDARY),
    );
    ui.add_space(theme::SPACING_SM);

    let rows = state.rows.clone();
    let busy = state.busy;

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for row in &rows {
                show_row(ui, state, row, busy);
                ui.add_space(theme::SPACING_XS);
            }
        });

    if let Some(target) = state.confirm_rollback.clone() {
        show_rollback_confirm(ui, state, &target);
    }
}

fn show_row(ui: &mut Ui, state: &mut AppState, row: &SnapshotRow, busy: bool) {
    let title = row.title();
    widgets::section(ui, &title, |ui| {
        ui.horizontal(|ui| {
            match row.badge() {
                "running" => {
                    ui.label(
                        RichText::new("\u{25cf} running")
                            .color(theme::SUCCESS)
                            .strong(),
                    );
                }
                "staged for next boot" => {
                    ui.label(
                        RichText::new("\u{25cf} staged for next boot")
                            .color(theme::WARNING)
                            .strong(),
                    );
                }
                _ => {}
            }
            if let Some(r) = &row.record {
                if !r.outcome.succeeded() {
                    ui.label(
                        RichText::new("\u{274c} failed")
                            .color(theme::ERROR)
                            .strong(),
                    );
                }
            }
        });

        ui.label(RichText::new(row.detail()).color(theme::TEXT_SECONDARY));

        if let Some(record) = &row.record {
            if !record.changes.is_empty() {
                let expanded = state.expanded.as_deref() == Some(row.target.as_str());
                let label = if expanded {
                    "Hide packages"
                } else {
                    "Show packages"
                };
                if ui.small_button(label).clicked() {
                    state.expanded = if expanded {
                        None
                    } else {
                        Some(row.target.clone())
                    };
                }
                if expanded {
                    show_changes(ui, record);
                }
            }
            if let Outcome::Failed(_) = record.outcome {
                ui.label(
                    RichText::new("This update did not complete; nothing was activated.")
                        .color(theme::TEXT_MUTED)
                        .size(11.0),
                );
            }
        }

        // Rolling back to what is already booted is a no-op; the CLI rejects it
        // and there is nothing useful to offer.
        if !row.is_running {
            ui.add_space(theme::SPACING_XS);
            if ui
                .add_enabled(!busy, egui::Button::new("Roll back to this"))
                .clicked()
            {
                state.confirm_rollback = Some(row.clone());
            }
        }
    });
}

fn show_changes(ui: &mut Ui, record: &crate::immutable::history::UpdateRecord) {
    let changes = &record.changes;
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for pkg in &changes.added {
                line(
                    ui,
                    theme::SUCCESS,
                    &format!("+ {} {}", pkg.name, pkg.version),
                );
            }
            for pkg in &changes.upgraded {
                line(
                    ui,
                    theme::ACCENT,
                    &format!("~ {} {} \u{2192} {}", pkg.name, pkg.from, pkg.to),
                );
            }
            for pkg in &changes.removed {
                line(ui, theme::ERROR, &format!("- {} {}", pkg.name, pkg.version));
            }
        });
}

fn line(ui: &mut Ui, color: egui::Color32, text: &str) {
    ui.label(RichText::new(text).monospace().size(11.0).color(color));
}

fn show_rollback_confirm(ui: &mut Ui, state: &mut AppState, row: &SnapshotRow) {
    let mut open = true;
    egui::Window::new("Roll back?")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ui.ctx(), |ui| {
            ui.label(
                RichText::new(format!("Roll back to {}?", row.title()))
                    .strong()
                    .size(14.0),
            );
            ui.add_space(theme::SPACING_XS);
            ui.label(
                RichText::new(
                    "This moves the boot pointer only. Nothing is deleted, and it takes effect \
                     on the next reboot.",
                )
                .color(theme::TEXT_SECONDARY),
            );

            // The documented consequence of /var being shared: the package
            // database is not rolled back with the files.
            if row.has_newer_updates {
                ui.add_space(theme::SPACING_SM);
                ui.label(
                    RichText::new(
                        "\u{26a0} Newer updates exist than this snapshot. The package database \
                         lives on the shared /var and is not rolled back, so afterwards pacman \
                         will still report the newer versions even though the older files are \
                         what is installed.",
                    )
                    .color(theme::WARNING),
                );
            }

            ui.add_space(theme::SPACING_MD);
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, "Roll back").clicked() {
                    let target = row.target.clone();
                    state.confirm_rollback = None;
                    state.start_rollback(target);
                }
                if ui.button("Cancel").clicked() {
                    state.confirm_rollback = None;
                }
            });
        });
    if !open {
        state.confirm_rollback = None;
    }
}
