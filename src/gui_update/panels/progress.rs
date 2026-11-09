//! Progress and log output for a running operation.

use crate::gui_update::console::Phase;
use crate::gui_update::preview::human_size;
use crate::gui_update::state::AppState;
use crate::gui_update::style;
use crate::gui_update::ui::{self, Tone};
use crate::utils::console::{Answer, QuestionKind};
use egui::{RichText, Ui};

/// The share of the right pane pacman's console takes when there is one; the
/// Log has the rest, which then holds only the steps around pacman.
const CONSOLE_SHARE: f32 = 0.62;

pub fn show(ui: &mut Ui, state: &mut AppState) {
    state.refresh_download();

    ui::panes(ui, &[0.85, 1.6], |i, ui| match i {
        0 => ui::scrolling(ui, "progress-status", |ui| {
            if state.question.is_some() {
                question(ui, state);
            }
            status_card(ui, state);
        }),
        _ => {
            if state.console.is_empty() {
                log_card(ui, state);
            } else {
                let height = ui.available_height() * CONSOLE_SHARE;
                ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                    console(ui, state);
                });
                ui.add_space(style::GAP);
                log_card(ui, state);
            }
        }
    });
}

fn status_card(ui: &mut Ui, state: &mut AppState) {
    if let Some(err) = state.error.clone() {
        ui::card(ui, "Failed", |ui| {
            ui::callout(ui, Tone::Error, &err);
        });
    } else if let Some(done) = state.finished.clone() {
        ui::card(ui, "Done", |ui| {
            ui.label(
                RichText::new(done)
                    .font(style::medium(15.0))
                    .color(style::SUCCESS),
            );
            ui.add_space(12.0);
            if ui::primary(ui, true, "Reboot now").clicked() {
                // Best-effort: if this fails the user is told to reboot anyway.
                let _ = std::process::Command::new("reboot").status();
            }
        });
    } else if state.busy {
        ui::card(ui, "Working", |ui| {
            ui.horizontal(|ui| {
                ui.spinner();
                let status = match state.phase {
                    Some(phase) => phase.label(),
                    None => state.status.as_str(),
                };
                ui.label(
                    RichText::new(status)
                        .font(style::medium(14.0))
                        .color(style::TEXT),
                );
            });
            if state.phase == Some(Phase::Downloading) {
                let got = human_size(state.download.bytes);
                ui.add_space(4.0);
                ui.label(
                    RichText::new(match state.download.total {
                        Some(total) => format!("{got} of {} downloaded", human_size(total)),
                        None => format!("{got} downloaded"),
                    })
                    .monospace()
                    .color(style::TEXT_DIM),
                );
            }
            ui.add_space(8.0);
            // An update's pacman streams into the console; everything else —
            // a removal, an AUR build, and the steps around pacman — is
            // captured and reported when it finishes, so it is genuinely
            // silent. Say so rather than looking hung.
            let hint = if state.console.is_empty() {
                "Output appears as each step completes, so a long step can sit quiet for \
                 several minutes."
            } else {
                "pacman's output streams into the console. Other steps report in the log \
                 when they finish."
            };
            ui::note(ui, hint);
        });
    } else {
        ui::card(ui, "Idle", |ui| {
            ui::empty(
                ui,
                "Nothing running",
                "Updates, removals, builds and rollbacks report here.",
            );
        });
    }
}

fn log_card(ui: &mut Ui, state: &AppState) {
    ui::card_fill(
        ui,
        "Log",
        |_| {},
        |ui, height| {
            if state.logs.is_empty() {
                ui::note(ui, "No output yet.");
                return;
            }
            terminal(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("progress-log")
                    .max_height(height - 16.0)
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for entry in &state.logs {
                            ui.label(RichText::new(entry).monospace().color(style::TEXT_DIM));
                        }
                    });
            });
        },
    );
}

/// The sunken, monospace box pacman's output and the log sit in.
fn terminal(ui: &mut Ui, body: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(style::INSET)
        .corner_radius(style::RADIUS)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui)
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
    ui::card(ui, "pacman is waiting for an answer", |ui| {
        ui::callout_with(ui, Tone::Warning, |ui| {
            ui.label(
                RichText::new(&q.question.text)
                    .monospace()
                    .color(style::WARNING),
            );
        });
        ui.add_space(6.0);
        ui::note(
            ui,
            "Your answer only changes the snapshot being built. The running system stays as \
             it is until you reboot into the new one, and a rollback undoes it.",
        );
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            match q.question.kind {
                QuestionKind::YesNo { default_yes } => {
                    let label = |name: &str, default: bool| {
                        if default {
                            format!("{name} (default)")
                        } else {
                            name.to_string()
                        }
                    };
                    if ui::primary(ui, true, &label("Yes", default_yes)).clicked() {
                        action = Some(Action::Answer(Answer::Reply("y".into())));
                    }
                    if ui::button(ui, true, &label("No", !default_yes)).clicked() {
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
                    if ui::primary(ui, true, "Answer").clicked() || entered {
                        action = Some(Action::Submit);
                    }
                    if matches!(q.question.kind, QuestionKind::Selection) {
                        ui::note(ui, "Empty selects all.");
                    }
                }
            }
            ui.add_space(12.0);
            if ui::danger(ui, true, "Cancel update")
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
    ui::card_fill(
        ui,
        "pacman",
        |_| {},
        |ui, height| {
            terminal(ui, |ui| {
                let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
                let rows = state.console.row_count();
                egui::ScrollArea::both()
                    .id_salt("pacman-console")
                    .max_height(height - 16.0)
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show_rows(ui, row_height, rows, |ui, range| {
                        for i in range {
                            // The unfinished last line is the question while
                            // one is pending; pick it out so it is not lost in
                            // the scroll.
                            let asking = state.question.is_some() && i + 1 == rows;
                            let text = RichText::new(state.console.row(i)).monospace();
                            ui.label(if asking {
                                text.color(style::WARNING)
                            } else {
                                text.color(style::TEXT_DIM)
                            });
                        }
                    });
            });
        },
    );
}
