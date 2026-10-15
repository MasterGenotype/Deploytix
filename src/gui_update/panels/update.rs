//! Update actions: full upgrade, repo packages, local package files, and
//! package removal.

use crate::gui::{theme, widgets};
use crate::gui_update::model;
use crate::gui_update::state::AppState;
use crate::immutable::remove::RemovalPlan;
use egui::{RichText, Ui};
use std::path::PathBuf;

pub fn show(ui: &mut Ui, state: &mut AppState) {
    widgets::page_heading(ui, "Update");

    if state.busy {
        widgets::info_text(
            ui,
            "An operation is already running — see the Progress tab.",
        );
        return;
    }

    widgets::section(ui, "Full system upgrade", |ui| {
        ui.label(
            RichText::new("Upgrade every package, into a new snapshot.")
                .color(theme::TEXT_SECONDARY),
        );
        ui.add_space(theme::SPACING_SM);
        if widgets::primary_button(ui, "Upgrade everything").clicked() {
            // No arguments is what `deploytix update` treats as a full upgrade.
            state.start_update(Vec::new());
        }
    });

    ui.add_space(theme::SPACING_SM);

    widgets::section(ui, "Install packages", |ui| {
        ui.label(
            RichText::new("Repository packages, separated by spaces:").color(theme::TEXT_SECONDARY),
        );
        let field = ui.add(
            egui::TextEdit::singleline(&mut state.repo_packages)
                .hint_text("vim git neovim")
                .desired_width(f32::INFINITY),
        );
        // Resolve when editing stops, not per keystroke: each resolve spawns
        // pacman, and doing that on every character would be a subprocess per
        // letter typed.
        if field.lost_focus() || (field.changed() && !state.repo_packages.ends_with(' ')) {
            state.start_resolve();
        }

        show_preview(ui, state);

        ui.add_space(theme::SPACING_SM);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Local package files:").color(theme::TEXT_SECONDARY));
            if ui.button("Add file...").clicked() {
                state.browser_open = true;
            }
        });

        if state.selected_files.is_empty() {
            ui.label(
                RichText::new("none selected")
                    .color(theme::TEXT_MUTED)
                    .size(11.0),
            );
        } else {
            let mut remove: Option<usize> = None;
            for (i, path) in state.selected_files.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.small_button("\u{2715}").clicked() {
                        remove = Some(i);
                    }
                    ui.label(RichText::new(path.to_string_lossy()).monospace().size(11.0));
                });
            }
            if let Some(i) = remove {
                state.selected_files.remove(i);
            }
        }

        ui.add_space(theme::SPACING_SM);
        let ready = state.has_selection();
        if widgets::primary_button_enabled(ui, ready, "Install selected").clicked() {
            let args = state.update_args();
            state.start_update(args);
        }
        if !ready {
            ui.label(
                RichText::new("Enter a package name or add a file to enable this.")
                    .color(theme::TEXT_MUTED)
                    .size(11.0),
            );
        }
    });

    ui.add_space(theme::SPACING_SM);

    show_remove_section(ui, state);

    ui.add_space(theme::SPACING_SM);

    widgets::section(ui, "Options", |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Snapshots to keep:").color(theme::TEXT_SECONDARY));
            ui.add(egui::DragValue::new(&mut state.keep_sets).range(1..=20));
        });
        ui.checkbox(
            &mut state.reboot_after,
            "Reboot automatically when finished",
        );
    });

    if state.browser_open {
        show_file_browser(ui, state);
    }
    if state.confirm_remove {
        show_remove_confirm(ui, state);
    }
}

/// The removal form: names, the two pacman flags, a preview of what `-Rs`
/// would actually take out, and the button.
///
/// Same transaction as the CLI's `deploytix remove`: a new snapshot set with
/// the packages gone, activated on reboot. The package database lives inside
/// the set, so it records the removal and becomes the live database when that
/// set boots; the running system's read-only copy is left alone until then.
fn show_remove_section(ui: &mut Ui, state: &mut AppState) {
    widgets::section(ui, "Remove packages", |ui| {
        if let Some(why) = state.remove_refusal() {
            ui.label(RichText::new(why).color(theme::WARNING));
            return;
        }

        ui.label(
            RichText::new("Installed packages to remove, separated by spaces:")
                .color(theme::TEXT_SECONDARY),
        );
        let field = ui.add(
            egui::TextEdit::singleline(&mut state.remove_packages)
                .hint_text("linux-lts nano")
                .desired_width(f32::INFINITY),
        );
        // As for installs: resolve when editing pauses, not per keystroke.
        // Each resolve is three pacman queries.
        let flags_changed = ui
            .checkbox(
                &mut state.remove_cascade,
                "Also remove packages that depend on these (pacman -Rc)",
            )
            .changed();
        ui.checkbox(
            &mut state.remove_purge,
            "Also delete configuration files instead of keeping .pacsave copies (pacman -Rn)",
        );
        if field.lost_focus()
            || (field.changed() && !state.remove_packages.ends_with(' '))
            || flags_changed
        {
            state.start_remove_resolve();
        }

        show_remove_preview(ui, state);

        ui.add_space(theme::SPACING_SM);
        let ready = state.remove_plan().is_some();
        if widgets::primary_button_enabled(ui, ready, "Remove selected").clicked() {
            state.confirm_remove = true;
        }
        if !ready {
            let hint = if state.remove_targets().is_empty() {
                "Enter a package name to enable this."
            } else if state.remove_resolving {
                "Checking what would be removed..."
            } else {
                "Fix the problems above to enable this."
            };
            ui.label(RichText::new(hint).color(theme::TEXT_MUTED).size(11.0));
        }
    });
}

/// What `pacman -Rs --print` says, vetted by the same guard the transaction
/// uses, so the greyed-out button and the CLI can never disagree about why.
fn show_remove_preview(ui: &mut Ui, state: &AppState) {
    if state.remove_targets().is_empty() {
        return;
    }
    if state.remove_resolving && state.current_remove_preview().is_none() {
        ui.label(
            RichText::new("Checking what would be removed...")
                .color(theme::TEXT_MUTED)
                .size(11.0),
        );
        return;
    }
    let Some(preview) = state.current_remove_preview() else {
        return;
    };

    ui.add_space(theme::SPACING_XS);
    match preview.plan() {
        Some(plan) => {
            ui.label(
                RichText::new(format!("{} to remove", plan.resolved.len()))
                    .color(theme::TEXT_SECONDARY),
            );
            show_plan_details(ui, plan);
            ui.label(
                RichText::new(
                    "Resolved against the running system; the removal re-checks inside the \
                     snapshot it builds.",
                )
                .color(theme::TEXT_MUTED)
                .size(11.0),
            );
        }
        None => {
            ui.label(
                RichText::new("\u{26a0} Cannot remove")
                    .color(theme::ERROR)
                    .strong(),
            );
            for reason in preview.blockers() {
                ui.label(RichText::new(reason).color(theme::ERROR).size(11.0));
            }
        }
    }
}

/// The resolved list and the kernel notes, shared by the preview and the
/// confirm dialog so both describe the same thing.
fn show_plan_details(ui: &mut Ui, plan: &RemovalPlan) {
    egui::ScrollArea::vertical()
        .id_salt(ui.id().with("remove-plan"))
        .max_height(160.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for name in &plan.resolved {
                let colour = if plan.kernels_removed.contains(name) {
                    theme::WARNING
                } else {
                    theme::ERROR
                };
                ui.label(
                    RichText::new(format!("- {name}"))
                        .monospace()
                        .size(11.0)
                        .color(colour),
                );
            }
        });
    // Kernel removals are the loud case: they touch the shared /boot, which
    // is the one thing a snapshot cannot roll back.
    for note in &plan.notes {
        ui.label(
            RichText::new(format!("\u{26a0} {note}"))
                .color(theme::WARNING)
                .size(11.0),
        );
    }
}

/// The confirmation the CLI asks for on the terminal, asked here instead.
fn show_remove_confirm(ui: &mut Ui, state: &mut AppState) {
    // A preview that went stale while the dialog was open must not be run.
    let Some(plan) = state.remove_plan().cloned() else {
        state.confirm_remove = false;
        return;
    };

    let mut open = true;
    egui::Window::new("Remove packages?")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .show(ui.ctx(), |ui| {
            ui.label(
                RichText::new(format!(
                    "Remove {} package(s) into a new snapshot?",
                    plan.resolved.len()
                ))
                .strong()
                .size(14.0),
            );
            ui.add_space(theme::SPACING_XS);
            show_plan_details(ui, &plan);
            ui.add_space(theme::SPACING_XS);
            ui.label(
                RichText::new(
                    "The running system is untouched until you reboot. Rolling back to the \
                     current snapshot restores the packages.",
                )
                .color(theme::TEXT_SECONDARY),
            );

            ui.add_space(theme::SPACING_MD);
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, "Remove").clicked() {
                    state.start_remove();
                }
                if ui.button("Cancel").clicked() {
                    state.confirm_remove = false;
                }
            });
        });
    if !open {
        state.confirm_remove = false;
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
        .default_size([560.0, 420.0])
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                if ui.button("\u{2191} Up").clicked() {
                    if let Some(parent) = state.browser_dir.parent() {
                        state.browser_dir = parent.to_path_buf();
                    }
                }
                ui.label(
                    RichText::new(state.browser_dir.to_string_lossy())
                        .monospace()
                        .size(11.0)
                        .color(theme::TEXT_SECONDARY),
                );
            });
            ui.separator();

            let (dirs, files) = read_dir_split(&state.browser_dir);
            if dirs.is_empty() && files.is_empty() {
                widgets::info_text(ui, "No packages or subdirectories here.");
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
        ui.label(
            RichText::new("Resolving dependencies...")
                .color(theme::TEXT_MUTED)
                .size(11.0),
        );
        return;
    }

    let Some(preview) = state.current_preview() else {
        return;
    };

    ui.add_space(theme::SPACING_XS);
    let colour = if preview.needs_attention() {
        theme::WARNING
    } else {
        theme::TEXT_SECONDARY
    };
    ui.label(RichText::new(preview.summary()).color(colour));

    // AUR packages are built rather than downloaded, so they need a helper and
    // take far longer. Saying so before staging is the whole point.
    if preview.needs_helper() {
        let ready = state
            .capability
            .as_ref()
            .map(|c| c.is_ready())
            .unwrap_or(false);
        let text = format!("Built from source: {}", preview.aur.join(", "));
        ui.label(RichText::new(text).color(theme::TEXT_SECONDARY).size(11.0));
        if !ready {
            let why = state
                .capability
                .as_ref()
                .and_then(|c| c.blockers().first().map(|b| b.to_string()))
                .unwrap_or_else(|| "AUR support is unavailable.".to_string());
            ui.label(
                RichText::new(format!("\u{26a0} {why}"))
                    .color(theme::WARNING)
                    .size(11.0),
            );
        }
    }

    // Removals are the loud case: on a read-only root, a package removed to
    // satisfy a conflict is the hardest outcome to walk back.
    if let Some(plan) = &preview.plan {
        if !plan.to_remove.is_empty() {
            ui.label(
                RichText::new(format!(
                    "\u{26a0} Would remove: {}",
                    plan.to_remove.join(", ")
                ))
                .color(theme::WARNING)
                .size(11.0),
            );
        }
    }

    for note in &preview.notes {
        ui.label(RichText::new(note).color(theme::TEXT_MUTED).size(11.0));
    }
}
