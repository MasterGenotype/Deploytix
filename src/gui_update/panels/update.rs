//! Update actions: full upgrade, repo packages, local package files.

use crate::gui::{theme, widgets};
use crate::gui_update::model;
use crate::gui_update::state::AppState;
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
        ui.add(
            egui::TextEdit::singleline(&mut state.repo_packages)
                .hint_text("vim git neovim")
                .desired_width(f32::INFINITY),
        );

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
