//! System overview: which backend, what is booted, what is staged, and the
//! latest snapshot.

use crate::gui_update::app::describe_target;
use crate::gui_update::calendar::{self, Selection};
use crate::gui_update::state::{AppState, Tab};
use crate::gui_update::style;
use crate::gui_update::ui::{self, Tone};
use egui::{RichText, Ui};

pub fn show(ui: &mut Ui, state: &mut AppState) {
    if let Some(err) = state.refresh_error.clone() {
        // Distinct from "there is nothing": a read that failed must not look
        // like an empty, healthy system.
        ui::panes(ui, &[1.0, 1.0], |i, ui| {
            if i == 0 {
                ui::card(ui, "Could not read system state", |ui| {
                    ui::callout(ui, Tone::Error, &err);
                    ui.add_space(8.0);
                    if ui::button(ui, true, "Try again").clicked() {
                        state.start_refresh();
                    }
                });
            }
        });
        return;
    }

    let Some(info) = state.info.clone() else {
        ui::empty(ui, "Reading system state", "This takes a moment.");
        return;
    };

    ui::panes(ui, &[1.0, 1.1, 1.0], |i, ui| match i {
        0 => ui::scrolling(ui, "system-boot", |ui| {
            let noun = info.backend.target_noun();
            ui::card(ui, "Boot", |ui| {
                ui::facts(
                    ui,
                    &[
                        ("Backend", info.backend.label().to_string()),
                        ("Running", describe_target(&info.running, noun)),
                        ("Next boot", describe_target(&info.pointer, noun)),
                    ],
                );
                ui.add_space(10.0);
                if info.has_staged_update() {
                    ui::callout(
                        ui,
                        Tone::Warning,
                        "An update is staged and takes effect on the next reboot.",
                    );
                } else {
                    ui::callout(
                        ui,
                        Tone::Success,
                        "Nothing is staged. The next boot is the system running now.",
                    );
                }
            });
        }),
        1 => ui::scrolling(ui, "system-latest", |ui| latest(ui, state)),
        _ => ui::scrolling(ui, "system-how", |ui| {
            ui::card(ui, "How updates work here", |ui| {
                ui.label(
                    "/ and /usr are read-only, so packages are never installed into the \
                     running system. Each update builds a new snapshot and switches to it on \
                     reboot; if something breaks, roll back from the Snapshots tab.",
                );
                ui.add_space(8.0);
                ui::note(
                    ui,
                    "The package database lives inside each snapshot, so it rolls back \
                     with the files it describes.",
                );
                ui.add_space(4.0);
                ui::note(
                    ui,
                    "AUR packages are built on this machine, from the AUR tab. Repository \
                     packages are installed and removed from the Update tab.",
                );
            });
        }),
    });
}

/// The newest restore point and how many there are, with a way into the
/// calendar.
fn latest(ui: &mut Ui, state: &mut AppState) {
    let Some(row) = state.rows.first().cloned() else {
        ui::card(ui, "Latest snapshot", |ui| {
            ui::empty(ui, "No snapshots yet", "Run an update to create one.");
        });
        return;
    };
    ui::card(ui, "Latest snapshot", |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(row.title())
                    .font(style::medium(15.0))
                    .color(style::TEXT),
            );
            if row.is_running {
                ui::badge(ui, "running", Tone::Success);
            } else if row.is_staged {
                ui::badge(ui, "next boot", Tone::Warning);
            }
            if row.record.as_ref().is_some_and(|r| !r.outcome.succeeded()) {
                ui::badge(ui, "failed", Tone::Error);
            }
        });
        ui.add_space(4.0);
        ui::clamped(ui, &row.detail(), 4);
        ui.add_space(10.0);
        if ui::button(ui, true, "Open in Snapshots").clicked() {
            let selected = match row.created_at() {
                Some(secs) => Selection::Day(calendar::local_day(secs)),
                None => Selection::Undated,
            };
            state.snapshot_view = calendar::View {
                month: None,
                selected: Some(selected),
            };
            state.tab = Tab::Snapshots;
        }
    });

    let total = state.rows.len();
    let failed = state
        .rows
        .iter()
        .filter(|r| r.record.as_ref().is_some_and(|r| !r.outcome.succeeded()))
        .count();
    ui::card(ui, "Restore points", |ui| {
        let mut facts = vec![("Kept", total.to_string())];
        if failed > 0 {
            facts.push(("Failed builds", failed.to_string()));
        }
        ui::facts(ui, &facts);
        ui.add_space(6.0);
        ui::note(
            ui,
            format!(
                "Each transaction keeps the newest {} and prunes the rest; change that in \
                 the status bar.",
                state.keep_sets
            ),
        );
    });
}
