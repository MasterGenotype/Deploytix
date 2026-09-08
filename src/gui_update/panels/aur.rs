//! The AUR tab: search, select, and build.
//!
//! Separate from Update because the two are not the same operation. A repo
//! package is downloaded and unpacked in seconds; an AUR package is compiled on
//! the machine, needs a helper and a toolchain, and can take an hour. Putting
//! them behind one button would mean a control whose cost varies by three
//! orders of magnitude depending on what was typed into it.
//!
//! Search results are ranked locally (see [`crate::aur::search`]) because the
//! AUR's own search is an unordered substring match.

use crate::aur::search::RankedHit;
use crate::gui::{theme, widgets};
use crate::gui_update::state::AppState;
use egui::{RichText, Ui};

/// Results shown at once. The AUR returns hundreds for a broad term, and a
/// ranked list stops being useful long before that.
const MAX_RESULTS: usize = 40;

pub fn show(ui: &mut Ui, state: &mut AppState) {
    widgets::page_heading(ui, "AUR");

    if state.busy {
        widgets::info_text(
            ui,
            "An operation is already running — see the Progress tab.",
        );
        return;
    }

    // The refusal, if any, comes from the same function the transaction uses,
    // so the greyed-out button and the CLI cannot disagree.
    let refusal = state.aur_refusal();
    if let Some(reason) = &refusal {
        widgets::section(ui, "Not available on this system", |ui| {
            ui.label(RichText::new(reason).color(theme::WARNING));
            ui.add_space(theme::SPACING_XS);
            ui.label(
                RichText::new(
                    "Searching still works, so you can see what is available before \
                     setting the machine up to build it.",
                )
                .color(theme::TEXT_MUTED)
                .size(11.0),
            );
        });
        ui.add_space(theme::SPACING_SM);
    }

    widgets::section(ui, "Search the AUR", |ui| {
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut state.aur_query)
                    .hint_text("hhd, decky, paru...")
                    .desired_width(320.0),
            );
            // On Enter or focus loss, not per keystroke: each search is a
            // network request.
            let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if submitted || ui.button("Search").clicked() {
                state.start_search();
            }
        });

        if state.aur_query.trim().len() == 1 {
            ui.label(
                RichText::new("Type at least two characters.")
                    .color(theme::TEXT_MUTED)
                    .size(11.0),
            );
        }
        if state.aur_searching {
            ui.label(
                RichText::new("Searching...")
                    .color(theme::TEXT_MUTED)
                    .size(11.0),
            );
        }
        if let Some(err) = &state.aur_error {
            ui.label(RichText::new(err).color(theme::ERROR).size(11.0));
        }

        show_results(ui, state);
    });

    ui.add_space(theme::SPACING_SM);
    show_selection(ui, state, refusal.is_none());
}

fn show_results(ui: &mut Ui, state: &mut AppState) {
    if !state.search_results_current() {
        return;
    }
    if state.aur_results.is_empty() && !state.aur_searching {
        if !state.aur_results_term.is_empty() {
            ui.label(
                RichText::new("Nothing matched.")
                    .color(theme::TEXT_MUTED)
                    .size(11.0),
            );
        }
        return;
    }

    ui.add_space(theme::SPACING_XS);
    let shown: Vec<RankedHit> = state
        .aur_results
        .iter()
        .take(MAX_RESULTS)
        .cloned()
        .collect();
    let total = state.aur_results.len();

    egui::ScrollArea::vertical()
        .max_height(320.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for result in &shown {
                let selected = state.aur_selected.contains(&result.hit.name);
                ui.horizontal(|ui| {
                    let label = if selected { "\u{2713}" } else { "+" };
                    if ui.small_button(label).clicked() {
                        if selected {
                            state.deselect_aur(&result.hit.name);
                        } else {
                            state.select_aur(&result.hit.name);
                        }
                    }
                    ui.label(RichText::new(&result.hit.name).strong());
                    ui.label(
                        RichText::new(&result.hit.version)
                            .color(theme::TEXT_MUTED)
                            .size(11.0),
                    );
                    if result.hit.out_of_date {
                        // Flagged packages often fail to build, so this is
                        // worth knowing before committing an hour to one.
                        ui.label(
                            RichText::new("out of date")
                                .color(theme::WARNING)
                                .size(11.0),
                        );
                    }
                    if !result.name_match {
                        ui.label(
                            RichText::new("(description match)")
                                .color(theme::TEXT_MUTED)
                                .size(10.0),
                        );
                    }
                });
                if !result.hit.description.is_empty() {
                    ui.label(
                        RichText::new(&result.hit.description)
                            .color(theme::TEXT_SECONDARY)
                            .size(11.0),
                    );
                }
                ui.add_space(theme::SPACING_XS);
            }
        });

    if total > MAX_RESULTS {
        ui.label(
            RichText::new(format!(
                "Showing the {MAX_RESULTS} best of {total} matches — narrow the search for more."
            ))
            .color(theme::TEXT_MUTED)
            .size(11.0),
        );
    }
}

fn show_selection(ui: &mut Ui, state: &mut AppState, can_build: bool) {
    widgets::section(ui, "Selected for building", |ui| {
        if state.aur_selected.is_empty() {
            ui.label(
                RichText::new("Nothing selected. Search above and press + to add a package.")
                    .color(theme::TEXT_SECONDARY),
            );
            return;
        }

        let selected = state.aur_selected.clone();
        for name in &selected {
            ui.horizontal(|ui| {
                if ui.small_button("\u{2715}").clicked() {
                    state.deselect_aur(name);
                }
                ui.label(RichText::new(name).monospace().size(12.0));
            });
        }

        ui.add_space(theme::SPACING_SM);
        ui.label(
            RichText::new(
                "These are compiled on this machine into a new snapshot, which takes \
                 effect on the next reboot. The running system is not touched, and a \
                 failed build changes nothing.",
            )
            .color(theme::TEXT_MUTED)
            .size(11.0),
        );
        ui.add_space(theme::SPACING_SM);

        if widgets::primary_button_enabled(ui, can_build, "Build and stage").clicked() {
            state.start_aur_install();
        }
    });
}
