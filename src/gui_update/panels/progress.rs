//! Progress and log output for a running operation.

use crate::gui::{theme, widgets};
use crate::gui_update::console::Phase;
use crate::gui_update::preview::human_size;
use crate::gui_update::state::AppState;
use crate::utils::console::{Answer, QuestionKind};
use egui::{RichText, Ui};

/// Console and Log heights. Fixed rather than filling the window so the page
/// fits the updater's default size without scrolling, which otherwise slides
/// the heading under the tab bar.
const CONSOLE_HEIGHT: f32 = 300.0;
const LOG_HEIGHT: f32 = 320.0;
/// The Log beside a console: what it holds then is the steps around pacman.
const LOG_HEIGHT_WITH_CONSOLE: f32 = 120.0;

pub fn show(ui: &mut Ui, state: &mut AppState) {
    widgets::page_heading(ui, "Progress");
    state.refresh_download();

    if state.question.is_some() {
        question(ui, state);
        ui.add_space(theme::SPACING_SM);
    }

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
                let status = match state.phase {
                    Some(phase) => phase.label(),
                    None => state.status.as_str(),
                };
                ui.label(status);
            });
            if state.phase == Some(Phase::Downloading) {
                let got = human_size(state.download.bytes);
                ui.label(
                    RichText::new(match state.download.total {
                        Some(total) => format!("{got} of {} downloaded", human_size(total)),
                        None => format!("{got} downloaded"),
                    })
                    .monospace()
                    .color(theme::TEXT_SECONDARY),
                );
            }
            ui.add_space(theme::SPACING_XS);
            // An update's pacman streams into the console; everything else —
            // a removal, an AUR build, and the steps around pacman — is
            // captured and reported when it finishes, so it is genuinely
            // silent. Say so rather than looking hung.
            let hint = if state.console.is_empty() {
                "Output appears as each step completes, so a long step can sit quiet for \
                 several minutes."
            } else {
                "pacman's output streams into the console below. Other steps report in the \
                 log when they finish."
            };
            ui.label(RichText::new(hint).color(theme::TEXT_MUTED).size(11.0));
        });
    } else {
        widgets::info_text(ui, "Nothing running.");
    }

    ui.add_space(theme::SPACING_SM);

    if !state.console.is_empty() {
        console(ui, state);
        ui.add_space(theme::SPACING_SM);
    }

    widgets::section(ui, "Log", |ui| {
        if state.logs.is_empty() {
            ui.label(
                RichText::new("No output yet.")
                    .color(theme::TEXT_MUTED)
                    .size(11.0),
            );
            return;
        }
        let height = if state.console.is_empty() {
            LOG_HEIGHT
        } else {
            LOG_HEIGHT_WITH_CONSOLE
        };
        egui::ScrollArea::vertical()
            .max_height(height)
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

/// What the user did with the pending question this frame.
enum Action {
    Answer(Answer),
    Submit,
}

/// The question pacman is blocked on, with the controls to answer it.
fn question(ui: &mut Ui, state: &mut AppState) {
    let Some(q) = state.question.as_mut() else {
        return;
    };
    let mut action = None;
    widgets::section(ui, "pacman is waiting for an answer", |ui| {
        ui.label(
            RichText::new(&q.question.text)
                .monospace()
                .strong()
                .color(theme::WARNING),
        );
        ui.add_space(theme::SPACING_XS);
        ui.label(
            RichText::new(
                "Your answer only changes the snapshot being built. The running system stays \
                 as it is until you reboot into the new one, and a rollback undoes it.",
            )
            .color(theme::TEXT_MUTED)
            .size(11.0),
        );
        ui.add_space(theme::SPACING_SM);
        ui.horizontal(|ui| {
            match q.question.kind {
                QuestionKind::YesNo { default_yes } => {
                    let label = |name: &str, default: bool| {
                        if default {
                            format!("{name} (default)")
                        } else {
                            name.to_string()
                        }
                    };
                    if ui.button(label("Yes", default_yes)).clicked() {
                        action = Some(Action::Answer(Answer::Reply("y".into())));
                    }
                    if ui.button(label("No", !default_yes)).clicked() {
                        action = Some(Action::Answer(Answer::Reply("n".into())));
                    }
                }
                QuestionKind::Number { .. } | QuestionKind::Selection => {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut q.input)
                            .desired_width(120.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    let entered =
                        field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Answer").clicked() || entered {
                        action = Some(Action::Submit);
                    }
                    if matches!(q.question.kind, QuestionKind::Selection) {
                        ui.label(
                            RichText::new("Empty selects all.")
                                .color(theme::TEXT_MUTED)
                                .size(11.0),
                        );
                    }
                }
            }
            ui.add_space(theme::SPACING_MD);
            if ui
                .button(RichText::new("Cancel update").color(theme::ERROR))
                .on_hover_text("pacman declines and the half-built snapshot is discarded.")
                .clicked()
            {
                action = Some(Action::Answer(Answer::Cancel));
            }
        });
    });
    match action {
        Some(Action::Answer(a)) => state.answer_question(a),
        Some(Action::Submit) => state.submit_question(),
        None => {}
    }
}

/// pacman's output as it is written. Virtualised: a full upgrade prints
/// thousands of lines and only the visible ones are laid out.
fn console(ui: &mut Ui, state: &AppState) {
    widgets::section(ui, "pacman", |ui| {
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        let rows = state.console.row_count();
        egui::ScrollArea::both()
            .id_salt("pacman-console")
            .max_height(CONSOLE_HEIGHT)
            .auto_shrink([false, true])
            .stick_to_bottom(true)
            .show_rows(ui, row_height, rows, |ui, range| {
                for i in range {
                    // The unfinished last line is the question while one is
                    // pending; pick it out so it is not lost in the scroll.
                    let asking = state.question.is_some() && i + 1 == rows;
                    let text = RichText::new(state.console.row(i)).monospace().size(11.0);
                    ui.label(if asking {
                        text.color(theme::WARNING).strong()
                    } else {
                        text.color(theme::TEXT_SECONDARY)
                    });
                }
            });
    });
}
