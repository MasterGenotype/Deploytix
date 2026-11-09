//! Repository packages: the full upgrade, installing packages and local
//! package files, and removing packages. AUR packages have their own tab.

use crate::gui_update::model;
use crate::gui_update::state::{AppState, RemoveFormId};
use crate::gui_update::style;
use crate::gui_update::ui::{self, Tone};
use crate::immutable::remove::RemovalPlan;
use egui::{RichText, Ui};
use std::path::PathBuf;

pub fn show(ui: &mut Ui, state: &mut AppState) {
    if state.busy {
        ui::callout(
            ui,
            Tone::Accent,
            "An operation is running. These controls come back when it finishes.",
        );
        ui.add_space(style::GAP);
    }
    let busy = state.busy;
    ui.add_enabled_ui(!busy, |ui| {
        ui::panes(ui, &[1.0, 1.3, 1.3], |i, ui| match i {
            0 => ui::scrolling(ui, "update-upgrade", |ui| upgrade_card(ui, state)),
            1 => ui::scrolling(ui, "update-install", |ui| install_card(ui, state)),
            _ => ui::scrolling(ui, "update-remove", |ui| {
                remove_card(
                    ui,
                    state,
                    RemoveFormId::Repo,
                    "Remove packages",
                    "AUR packages are removed from the AUR tab.",
                );
            }),
        });
    });

    if state.browser_open {
        show_file_browser(ui, state);
    }
    if state.remove.confirm {
        show_remove_confirm(ui, state, RemoveFormId::Repo);
    }
}

fn upgrade_card(ui: &mut Ui, state: &mut AppState) {
    ui::card(ui, "Full system upgrade", |ui| {
        ui.label(
            RichText::new("Upgrade every repository package")
                .font(style::medium(15.0))
                .color(style::TEXT),
        );
        ui.add_space(4.0);
        ui.label(
            "Builds a new snapshot with everything upgraded. The running system is not \
             touched; the upgrade takes effect when you reboot into it.",
        );
        ui.add_space(12.0);
        if ui::primary(ui, true, "Upgrade system").clicked() {
            // No arguments is what `deploytix update` treats as a full upgrade.
            state.start_update(Vec::new(), false);
        }
        ui.add_space(8.0);
        ui::note(
            ui,
            "To rebuild AUR packages in the same snapshot, upgrade from the AUR tab instead.",
        );
    });
}

fn install_card(ui: &mut Ui, state: &mut AppState) {
    ui::card(ui, "Install packages", |ui| {
        ui::label(ui, "Repository packages");
        let field = ui::field(ui, &mut state.repo_packages, "vim git neovim");
        // Resolve when editing stops, not per keystroke: each resolve spawns
        // pacman, and doing that on every character would be a subprocess per
        // letter typed.
        if field.lost_focus() || (field.changed() && !state.repo_packages.ends_with(' ')) {
            state.start_resolve();
        }
        ui::note(ui, "Separate names with spaces.");
        show_preview(ui, state);

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui::label(ui, "Local package files");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui::quiet(ui, true, "Add file…").clicked() {
                    state.browser_open = true;
                }
            });
        });
        if state.selected_files.is_empty() {
            ui::note(ui, "None selected.");
        } else {
            let mut remove: Option<usize> = None;
            for (i, path) in state.selected_files.iter().enumerate() {
                ui::row(
                    ui,
                    |ui| {
                        if ui::icon_button(ui, ui::Icon::Cross, "Take this file off").clicked() {
                            remove = Some(i);
                        }
                    },
                    |ui| {
                        ui::truncated(
                            ui,
                            RichText::new(path.to_string_lossy())
                                .monospace()
                                .color(style::TEXT),
                        );
                    },
                );
            }
            if let Some(i) = remove {
                state.selected_files.remove(i);
            }
        }

        ui.add_space(12.0);
        ui::rule(ui);
        ui.add_space(12.0);
        let ready = state.has_selection();
        if ui::primary(ui, ready, "Install selected").clicked() {
            let args = state.update_args();
            state.start_update(args, false);
        }
        if !ready {
            ui::note(ui, "Enter a package name or add a file to enable this.");
        }
    });
}

/// A removal form: the names, the two pacman flags, a preview of what `-Rs`
/// would actually take out, and the button. The AUR tab draws the same form
/// around its checklist of installed AUR packages.
///
/// Same transaction as the CLI's `deploytix remove`: a new snapshot set with
/// the packages gone, activated on reboot. The package database lives inside
/// the set, so it records the removal and becomes the live database when that
/// set boots; the running system's read-only copy is left alone until then.
pub fn remove_card(ui: &mut Ui, state: &mut AppState, id: RemoveFormId, title: &str, hint: &str) {
    ui::card(ui, title, |ui| remove_form(ui, state, id, hint));
}

/// The body of [`remove_card`], for a caller with a card of its own.
pub fn remove_form(ui: &mut Ui, state: &mut AppState, id: RemoveFormId, hint: &str) {
    if let Some(why) = state.remove_refusal() {
        ui::callout(ui, Tone::Warning, why);
        return;
    }

    let form = state.remove_form_mut(id);
    let mut edited = false;
    if id == RemoveFormId::Repo {
        ui::label(ui, "Installed packages");
        let field = ui::field(ui, &mut form.packages, "linux-lts nano");
        // As for installs: resolve when editing pauses, not per keystroke.
        // Each resolve is three pacman queries.
        edited = field.lost_focus() || (field.changed() && !form.packages.ends_with(' '));
        ui::note(ui, hint);
        ui.add_space(6.0);
    }
    let flags_changed = ui
        .checkbox(
            &mut form.cascade,
            "Also remove packages that depend on these (-Rc)",
        )
        .changed();
    ui.checkbox(
        &mut form.purge,
        "Delete configuration files instead of keeping .pacsave copies (-Rn)",
    );
    if edited || flags_changed || form.needs_resolve() {
        state.start_remove_resolve(id);
    }

    show_remove_preview(ui, state, id);

    ui.add_space(12.0);
    ui::rule(ui);
    ui.add_space(12.0);
    let form = state.remove_form(id);
    let ready = form.plan().is_some();
    let label = match form.targets().len() {
        0 | 1 => "Remove selected".to_string(),
        n => format!("Remove {n} packages"),
    };
    if ui::danger(ui, ready, &label).clicked() {
        state.remove_form_mut(id).confirm = true;
    }
    if !ready {
        let form = state.remove_form(id);
        let hint = if form.targets().is_empty() {
            match id {
                RemoveFormId::Repo => "Enter a package name to enable this.",
                RemoveFormId::Aur => "Tick the packages to remove.",
            }
        } else if form.resolving {
            "Checking what would be removed…"
        } else {
            "Fix the problems above to enable this."
        };
        ui::note(ui, hint);
    }
}

/// What `pacman -Rs --print` says, vetted by the same guard the transaction
/// uses, so the greyed-out button and the CLI can never disagree about why.
fn show_remove_preview(ui: &mut Ui, state: &AppState, id: RemoveFormId) {
    let form = state.remove_form(id);
    if form.targets().is_empty() {
        return;
    }
    ui.add_space(8.0);
    if form.resolving && form.current_preview().is_none() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui::note(ui, "Checking what would be removed…");
        });
        return;
    }
    let Some(preview) = form.current_preview() else {
        return;
    };

    match preview.plan() {
        Some(plan) => {
            ui::label(ui, &format!("{} to remove", plan.resolved.len()));
            show_plan_details(ui, plan);
            ui::note(
                ui,
                "Resolved against the running system; the removal re-checks inside the \
                 snapshot it builds.",
            );
        }
        None => ui::callout_with(ui, Tone::Error, |ui| {
            ui.label(
                RichText::new("Cannot remove")
                    .font(style::medium(13.0))
                    .color(style::ERROR),
            );
            for reason in preview.blockers() {
                ui.label(RichText::new(reason).color(style::ERROR));
            }
        }),
    }
}

/// The resolved list and the kernel notes, shared by the preview and the
/// confirm dialog so both describe the same thing.
fn show_plan_details(ui: &mut Ui, plan: &RemovalPlan) {
    egui::Frame::new()
        .fill(style::INSET)
        .corner_radius(style::RADIUS)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt(ui.id().with("remove-plan"))
                .max_height(160.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for name in &plan.resolved {
                        let colour = if plan.kernels_removed.contains(name) {
                            style::WARNING
                        } else {
                            style::ERROR
                        };
                        ui.label(RichText::new(format!("− {name}")).monospace().color(colour));
                    }
                });
        });
    // Kernel removals are the loud case: they touch the shared /boot, which
    // is the one thing a snapshot cannot roll back.
    for note in &plan.notes {
        ui.add_space(4.0);
        ui::callout(ui, Tone::Warning, note);
    }
}

/// The confirmation the CLI asks for on the terminal, asked here instead.
pub fn show_remove_confirm(ui: &mut Ui, state: &mut AppState, id: RemoveFormId) {
    // A preview that went stale while the dialog was open must not be run.
    let Some(plan) = state.remove_form(id).plan().cloned() else {
        state.remove_form_mut(id).confirm = false;
        return;
    };

    let title = match id {
        RemoveFormId::Repo => "Remove packages?",
        RemoveFormId::Aur => "Remove AUR packages?",
    };
    let mut open = true;
    egui::Window::new(title)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(480.0)
        .show(ui.ctx(), |ui| {
            ui.label(
                RichText::new(format!(
                    "Remove {} package(s) into a new snapshot",
                    plan.resolved.len()
                ))
                .font(style::medium(15.0))
                .color(style::TEXT),
            );
            ui.add_space(8.0);
            show_plan_details(ui, &plan);
            ui.add_space(8.0);
            ui.label(
                "The running system is untouched until you reboot. Rolling back to the \
                 current snapshot restores the packages.",
            );
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui::danger(ui, true, "Remove").clicked() {
                    state.start_remove(id);
                }
                if ui::button(ui, true, "Cancel").clicked() {
                    state.remove_form_mut(id).confirm = false;
                }
            });
        });
    if !open {
        state.remove_form_mut(id).confirm = false;
    }
}

/// A minimal package-file picker.
///
/// Deliberately not a native dialog: adding one would mean a new dependency and
/// a portal round-trip, and under `pkexec` it would run as root with root's
/// `$HOME` anyway. This browses the invoking user's home instead (see
/// `AppState::default`).
fn show_file_browser(ui: &mut Ui, state: &mut AppState) {
    let mut open = true;
    egui::Window::new("Select a package file")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .default_size([560.0, 420.0])
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                if ui::button(ui, true, "\u{2191} Up").clicked() {
                    if let Some(parent) = state.browser_dir.parent() {
                        state.browser_dir = parent.to_path_buf();
                    }
                }
                ui.label(
                    RichText::new(state.browser_dir.to_string_lossy())
                        .monospace()
                        .color(style::TEXT_DIM),
                );
            });
            ui.separator();

            let (dirs, files) = read_dir_split(&state.browser_dir);
            if dirs.is_empty() && files.is_empty() {
                ui::note(ui, "No packages or subdirectories here.");
            }

            egui::ScrollArea::vertical()
                .max_height(300.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for dir in dirs {
                        let name = file_name(&dir);
                        if ui.button(format!("\u{1f4c1} {name}")).clicked() {
                            state.browser_dir = dir.clone();
                        }
                    }
                    for file in files {
                        let name = file_name(&file);
                        if ui.button(format!("\u{1f4e6} {name}")).clicked() {
                            if !state.selected_files.contains(&file) {
                                state.selected_files.push(file.clone());
                            }
                            state.browser_open = false;
                        }
                    }
                });
        });
    if !open {
        state.browser_open = false;
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string())
}

/// Readable subdirectories and installable package files in `dir`, each sorted.
///
/// Unreadable entries are skipped rather than surfaced as errors — a browser
/// that refuses to list a directory because one entry is unreadable is worse
/// than one that shows the rest.
fn read_dir_split(dir: &std::path::Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = file_name(&path);
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                dirs.push(path);
            } else if model::is_package_file(&name) {
                files.push(path);
            }
        }
    }
    dirs.sort();
    files.sort();
    (dirs, files)
}

/// Render the dependency preview for whatever is currently typed.
///
/// Deliberately quiet when there is nothing to say: an empty field, or a
/// resolve that has not landed, shows nothing rather than a spinner that
/// flickers on every keystroke.
fn show_preview(ui: &mut Ui, state: &AppState) {
    if state.resolving && state.current_preview().is_none() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spinner();
            ui::note(ui, "Resolving dependencies…");
        });
        return;
    }

    let Some(preview) = state.current_preview() else {
        return;
    };

    ui.add_space(6.0);
    let tone = if preview.needs_attention() {
        Tone::Warning
    } else {
        Tone::Neutral
    };
    ui::callout_with(ui, tone, |ui| {
        let colour = if tone == Tone::Warning {
            style::WARNING
        } else {
            style::TEXT
        };
        ui.label(RichText::new(preview.summary()).color(colour));

        // AUR packages are built rather than downloaded, so they need a helper
        // and take far longer. Saying so before staging is the whole point.
        if preview.needs_helper() {
            let ready = state
                .capability
                .as_ref()
                .map(|c| c.is_ready())
                .unwrap_or(false);
            ui::note(ui, format!("Built from source: {}", preview.aur.join(", ")));
            if !ready {
                let why = state
                    .capability
                    .as_ref()
                    .and_then(|c| c.blockers().first().map(|b| b.to_string()))
                    .unwrap_or_else(|| "AUR support is unavailable.".to_string());
                ui.label(RichText::new(why).color(style::WARNING));
            }
        }

        // Removals are the loud case: on a read-only root, a package removed
        // to satisfy a conflict is the hardest outcome to walk back.
        if let Some(plan) = &preview.plan {
            if !plan.to_remove.is_empty() {
                ui.label(
                    RichText::new(format!("Would remove: {}", plan.to_remove.join(", ")))
                        .color(style::WARNING),
                );
            }
        }

        for note in &preview.notes {
            ui::note(ui, note.as_str());
        }
    });
}
