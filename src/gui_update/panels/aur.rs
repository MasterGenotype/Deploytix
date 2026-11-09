//! The AUR tab: everything to do with AUR packages, in one place. What this
//! machine can build with, which installed packages have updates and the
//! upgrade that rebuilds them, searching for and building new ones, and
//! removing installed ones.
//!
//! Separate from Update because the two are not the same operation. A repo
//! package is downloaded and unpacked in seconds; an AUR package is compiled on
//! the machine, needs a helper and a toolchain, and can take an hour. Putting
//! them behind one button would mean a control whose cost varies by three
//! orders of magnitude depending on what was typed into it.
//!
//! The upgrade here is `deploytix update --aur`: the full repository upgrade,
//! then the AUR rebuilds in the same set, because a build has to link against
//! the libraries that set ends up with.
//!
//! Search results are ranked locally (see [`crate::aur::search`]) because the
//! AUR's own search is an unordered substring match.

use super::update::{remove_form, show_remove_confirm};
use crate::aur::search::RankedHit;
use crate::aur::upgrade::UpgradePlan;
use crate::gui_update::state::{AppState, RemoveFormId};
use crate::gui_update::style;
use crate::gui_update::ui::{self, Tone};
use egui::{RichText, Ui};

/// Results shown at once. The AUR returns hundreds for a broad term, and a
/// ranked list stops being useful long before that.
const MAX_RESULTS: usize = 40;

pub fn show(ui: &mut Ui, state: &mut AppState) {
    // Once per fresh plan: on first visit, and again after a transaction has
    // made the last answer stale. Never while one runs, and not again after a
    // failure until the user asks.
    if !state.busy
        && state.capability.is_some()
        && !state.aur_installed.is_empty()
        && state.aur_plan.is_none()
        && !state.aur_checking
        && state.aur_check_error.is_none()
    {
        state.start_aur_check();
    }

    if state.busy {
        ui::callout(
            ui,
            Tone::Accent,
            "An operation is running. These controls come back when it finishes.",
        );
        ui.add_space(style::GAP);
    }
    // The refusal comes from the same function the transaction uses, so the
    // greyed-out buttons and the CLI cannot disagree.
    let refusal = state.aur_refusal();
    let busy = state.busy;
    ui.add_enabled_ui(!busy, |ui| {
        ui::panes(ui, &[1.15, 1.25, 1.1], |i, ui| match i {
            0 => ui::scrolling(ui, "aur-status", |ui| {
                environment_card(ui, state, refusal.as_deref());
                updates_card(ui, state, refusal.is_none());
            }),
            1 => search_card(ui, state),
            _ => ui::scrolling(ui, "aur-installed", |ui| {
                queue_card(ui, state, refusal.is_none());
                installed_card(ui, state);
            }),
        });
    });

    if state.aur_remove.confirm {
        show_remove_confirm(ui, state, RemoveFormId::Aur);
    }
}

/// The helper and the account builds run as, or why there are none.
fn environment_card(ui: &mut Ui, state: &AppState, refusal: Option<&str>) {
    ui::card(ui, "Build environment", |ui| {
        let Some(cap) = &state.capability else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui::note(ui, "Probing for an AUR helper…");
            });
            return;
        };
        if let Some(reason) = refusal {
            ui::callout(ui, Tone::Warning, reason);
            ui.add_space(6.0);
            ui::note(
                ui,
                "Searching still works, so you can see what is available before setting \
                 the machine up to build it.",
            );
        }
        if cap.is_ready() {
            let mut facts = vec![(
                "Helper",
                cap.helper.map(|h| h.to_string()).unwrap_or_default(),
            )];
            if let Some(user) = &cap.build_user {
                facts.push((
                    "Builds as",
                    format!("{} ({})", user.name, user.source.label()),
                ));
            }
            let others: Vec<String> = cap
                .helpers
                .iter()
                .filter(|h| Some(**h) != cap.helper)
                .map(|h| h.to_string())
                .collect();
            if !others.is_empty() {
                facts.push(("Also installed", others.join(", ")));
            }
            ui::facts(ui, &facts);
            if cap.build_user.as_ref().is_some_and(|u| u.source.is_guess()) {
                ui.add_space(8.0);
                ui::callout(
                    ui,
                    Tone::Warning,
                    "That account was guessed from /etc/passwd. makepkg cannot run as \
                     root, so builds run as this user.",
                );
            }
        } else if refusal.is_none() {
            for blocker in cap.blockers() {
                ui.label(format!("\u{2022} {blocker}"));
            }
        }
        ui.add_space(8.0);
        ui::note(
            ui,
            "Builds use a disk-backed scratch directory on /var, not the chroot's /tmp, \
             which is capped at half of RAM.",
        );
    });
}

/// Which installed packages have updates, and the upgrade that rebuilds them.
fn updates_card(ui: &mut Ui, state: &mut AppState, can_build: bool) {
    let installed = state.aur_installed.len();
    let checking = state.aur_checking;
    let check = ui::card_with(
        ui,
        "Updates",
        |ui| installed > 0 && ui::quiet(ui, !checking, "Check again").clicked(),
        |ui| {
            if installed == 0 {
                ui::empty(
                    ui,
                    "No AUR packages installed",
                    "Packages you build from the AUR show up here.",
                );
                return;
            }
            if let Some(err) = &state.aur_check_error {
                ui::callout(ui, Tone::Error, err);
                ui.add_space(8.0);
            }
            match &state.aur_plan {
                Some(plan) => show_plan(ui, plan),
                None if checking => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui::note(
                            ui,
                            format!("Checking {installed} packages against the AUR…"),
                        );
                    });
                }
                None => ui::note(ui, "Not checked yet."),
            }

            ui.add_space(12.0);
            ui::rule(ui);
            ui.add_space(12.0);
            let outdated = state.aur_plan.as_ref().map(|p| p.outdated.len());
            // Unchecked or failed to check: the transaction checks for itself,
            // so the upgrade is still offered.
            let worth_running = outdated != Some(0);
            let label = match outdated {
                Some(n) if n > 0 => format!("Upgrade system and rebuild {n}"),
                _ => "Upgrade system and rebuild AUR packages".to_string(),
            };
            if ui::primary(ui, can_build && worth_running, &label).clicked() {
                state.start_aur_upgrade();
            }
            ui.add_space(6.0);
            ui::note(
                ui,
                "Runs the full repository upgrade first, then builds in the same snapshot so \
                 the builds link against the upgraded libraries. This can take a long time.",
            );
        },
    );
    if check {
        state.aur_check_error = None;
        state.start_aur_check();
    }
}

fn show_plan(ui: &mut Ui, plan: &UpgradePlan) {
    let headline = match plan.outdated.len() {
        0 => "Everything is up to date".to_string(),
        1 => "1 update available".to_string(),
        n => format!("{n} updates available"),
    };
    ui.label(
        RichText::new(headline)
            .font(style::medium(15.0))
            .color(style::TEXT),
    );
    ui::note(
        ui,
        format!(
            "{} up to date, {} not in the AUR",
            plan.current.len(),
            plan.not_in_aur.len()
        ),
    );

    if !plan.outdated.is_empty() {
        ui.add_space(8.0);
        inset(ui, |ui| {
            for (i, o) in plan.outdated.iter().enumerate() {
                if i > 0 {
                    ui.add_space(4.0);
                }
                ui::truncated(ui, RichText::new(&o.name).monospace().color(style::TEXT));
                ui::truncated(
                    ui,
                    RichText::new(format!("{}  \u{2192}  {}", o.installed, o.available))
                        .monospace()
                        .size(style::SMALL)
                        .color(style::ACCENT),
                );
            }
        });
    }

    if !plan.held.is_empty() {
        ui.add_space(10.0);
        ui::label(ui, "Held back");
        for h in &plan.held {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&h.name).monospace().color(style::WARNING));
                ui::note(ui, h.reason.as_str());
            });
        }
    }
}

/// A sunken box inside a card, for lists.
fn inset<R>(ui: &mut Ui, body: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(style::INSET)
        .corner_radius(style::RADIUS)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui)
        })
        .inner
}

fn search_card(ui: &mut Ui, state: &mut AppState) {
    ui::card_fill(
        ui,
        "Search the AUR",
        |_| {},
        |ui, height| {
            let top = ui.cursor().top();
            ui.horizontal(|ui| {
                let button_width = 84.0;
                let width = ui.available_width() - button_width - 8.0;
                let field = ui.add_sized(
                    [width, 30.0],
                    egui::TextEdit::singleline(&mut state.aur_query)
                        .hint_text(RichText::new("hhd, decky, paru…").color(style::TEXT_FAINT))
                        .margin(egui::Margin::symmetric(8, 5)),
                );
                // On Enter or the button, not per keystroke: each search is a
                // network request.
                let submitted = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui
                    .add_sized([button_width, 30.0], egui::Button::new("Search"))
                    .clicked()
                    || submitted
                {
                    state.start_search();
                }
            });

            if state.aur_query.trim().len() == 1 {
                ui::note(ui, "Type at least two characters.");
            }
            if state.aur_searching {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui::note(ui, "Searching…");
                });
            }
            if let Some(err) = &state.aur_error {
                ui::callout(ui, Tone::Error, err);
            }
            ui.add_space(6.0);
            let used = ui.cursor().top() - top;
            show_results(ui, state, (height - used).max(60.0));
        },
    );
}

fn show_results(ui: &mut Ui, state: &mut AppState, height: f32) {
    if !state.search_results_current() || state.aur_results_term.is_empty() {
        ui::empty(
            ui,
            "Find a package",
            "Results are ranked by how closely they match, so a typo still finds it.",
        );
        return;
    }
    if state.aur_results.is_empty() && !state.aur_searching {
        ui::empty(ui, "Nothing matched", "Try a shorter or different term.");
        return;
    }

    let shown: Vec<RankedHit> = state
        .aur_results
        .iter()
        .take(MAX_RESULTS)
        .cloned()
        .collect();
    let total = state.aur_results.len();
    let footer = if total > MAX_RESULTS { 22.0 } else { 0.0 };

    egui::ScrollArea::vertical()
        .id_salt("aur-results")
        .max_height(height - footer)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, result) in shown.iter().enumerate() {
                if i > 0 {
                    ui::rule(ui);
                }
                ui.add_space(6.0);
                result_row(ui, state, result);
                ui.add_space(6.0);
            }
        });

    if total > MAX_RESULTS {
        ui::note(
            ui,
            format!(
                "Showing the {MAX_RESULTS} best of {total} matches — narrow the search for more."
            ),
        );
    }
}

fn result_row(ui: &mut Ui, state: &mut AppState, result: &RankedHit) {
    let name = &result.hit.name;
    let queued = state.aur_selected.contains(name);
    let mut toggled = false;
    ui::row(
        ui,
        |ui| toggled = ui::toggle(ui, queued, "Queue", "Queued").clicked(),
        |ui| {
            ui.label(
                RichText::new(name)
                    .font(style::medium(13.5))
                    .color(style::TEXT),
            );
            ui::truncated(
                ui,
                RichText::new(&result.hit.version)
                    .monospace()
                    .color(style::TEXT_FAINT),
            );
        },
    );
    if toggled {
        if queued {
            state.deselect_aur(name);
        } else {
            state.select_aur(name);
        }
    }
    let installed = state.aur_installed.get(name);
    if installed.is_some() || result.hit.out_of_date || !result.name_match {
        ui.horizontal(|ui| {
            if let Some(version) = installed {
                ui::badge(ui, "installed", Tone::Neutral).on_hover_text(version);
            }
            if result.hit.out_of_date {
                // Flagged packages often fail to build, so this is worth
                // knowing before committing an hour to one.
                ui::badge(ui, "flagged out of date", Tone::Warning);
            }
            if !result.name_match {
                ui::badge(ui, "description match", Tone::Neutral);
            }
        });
    }
    if !result.hit.description.is_empty() {
        ui.label(RichText::new(&result.hit.description).color(style::TEXT_DIM));
    }
}

fn queue_card(ui: &mut Ui, state: &mut AppState, can_build: bool) {
    let count = state.aur_selected.len();
    ui::card_with(
        ui,
        "Build queue",
        |ui| {
            if count > 0 {
                ui.label(RichText::new(count.to_string()).color(style::TEXT_FAINT));
            }
        },
        |ui| {
            if count == 0 {
                ui::note(ui, "Nothing queued. Search for a package and press Queue.");
                return;
            }
            let selected = state.aur_selected.clone();
            inset(ui, |ui| {
                for name in &selected {
                    ui::row(
                        ui,
                        |ui| {
                            if ui::icon_button(ui, ui::Icon::Cross, "Take it out of the queue")
                                .clicked()
                            {
                                state.deselect_aur(name);
                            }
                        },
                        |ui| {
                            ui::truncated(ui, RichText::new(name).monospace().color(style::TEXT));
                        },
                    );
                }
            });
            ui.add_space(8.0);
            ui::note(
                ui,
                "Compiled on this machine into a new snapshot, which takes effect on the next \
                 reboot. The running system is not touched, and a failed build changes nothing.",
            );
            ui.add_space(10.0);
            if ui::primary(ui, can_build, "Build and stage").clicked() {
                state.start_aur_install();
            }
        },
    );
}

/// The installed AUR packages, as a checklist for the removal form under it.
fn installed_card(ui: &mut Ui, state: &mut AppState) {
    let installed: Vec<(String, String)> = state
        .aur_installed
        .iter()
        .map(|(n, v)| (n.clone(), v.clone()))
        .collect();
    ui::card_with(
        ui,
        "Installed AUR packages",
        |ui| {
            ui.label(RichText::new(installed.len().to_string()).color(style::TEXT_FAINT));
        },
        |ui| {
            if installed.is_empty() {
                ui::empty(ui, "None installed", "Build one from the search.");
                return;
            }
            inset(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("aur-installed-list")
                    .max_height(300.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (name, version) in &installed {
                            installed_row(ui, state, name, version);
                        }
                    });
            });
            ui.add_space(10.0);
            remove_form(ui, state, RemoveFormId::Aur, "");
        },
    );
}

fn installed_row(ui: &mut Ui, state: &mut AppState, name: &str, version: &str) {
    let mut toggled = false;
    let ticked = state.aur_remove.contains(name);
    ui::row(
        ui,
        |ui| {
            if let Some(plan) = &state.aur_plan {
                if let Some(o) = plan.outdated.iter().find(|o| o.name == name) {
                    ui::badge(ui, "update", Tone::Accent).on_hover_text(format!(
                        "{} \u{2192} {}\nRebuilt by the upgrade on the left.",
                        o.installed, o.available
                    ));
                } else if let Some(h) = plan.held.iter().find(|h| h.name == name) {
                    ui::badge(ui, "held", Tone::Warning).on_hover_text(&h.reason);
                } else if plan.not_in_aur.iter().any(|n| n == name) {
                    ui::badge(ui, "local", Tone::Neutral).on_hover_text(
                        "Not in the AUR: built locally or dropped from it. Left as installed.",
                    );
                }
            }
            ui.label(
                RichText::new(short(version, 14))
                    .monospace()
                    .size(style::SMALL)
                    .color(style::TEXT_FAINT),
            )
            .on_hover_text(version);
        },
        |ui| {
            let mut on = ticked;
            toggled = ui
                .checkbox(&mut on, "")
                .on_hover_text("Tick to remove")
                .changed();
            ui.spacing_mut().item_spacing.x = 4.0;
            ui::truncated(ui, RichText::new(name).monospace().color(style::TEXT));
        },
    );
    if toggled {
        state.aur_remove.toggle(name);
    }
}

/// `text` cut to `max` characters with an ellipsis. VCS versions run to forty.
fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let head: String = text.chars().take(max - 1).collect();
        format!("{head}\u{2026}")
    }
}
