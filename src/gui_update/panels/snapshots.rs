//! Snapshot history as a calendar: a month of days on the left, each marked
//! with the restore points made on it, and the selected day's snapshots on the
//! right with what each update changed and the way back to it.

use crate::gui_update::calendar::{self, Month, Selection, WEEKDAYS};
use crate::gui_update::model::SnapshotRow;
use crate::gui_update::state::AppState;
use crate::gui_update::style;
use crate::gui_update::ui::{self, Tone};
use crate::immutable::history::{Outcome, UpdateRecord};
use egui::{Align, Align2, Color32, CornerRadius, Layout, RichText, Sense, Stroke, StrokeKind, Ui};

/// Markers drawn in one day before the rest are counted instead.
const MAX_MARKERS: usize = 5;

pub fn show(ui: &mut Ui, state: &mut AppState) {
    if let Some(err) = state.refresh_error.clone() {
        // Distinct from "there are none": claiming an empty history when the
        // read failed would hide a broken system behind a reassuring message.
        ui::panes(ui, &[1.0, 1.0], |i, ui| {
            if i == 0 {
                ui::card(ui, "Could not read snapshots", |ui| {
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
    if state.rows.is_empty() {
        if state.refreshing {
            ui::empty(ui, "Reading snapshots", "This takes a moment.");
        } else {
            ui::empty(
                ui,
                "No snapshots yet",
                "Every update builds one. Run an update to create the first.",
            );
        }
        return;
    }

    let rows = state.rows.clone();
    let selected = settle_view(state, &rows);

    ui::panes(ui, &[1.0, 1.1], |i, ui| match i {
        0 => ui::scrolling(ui, "snapshots-calendar", |ui| {
            calendar_card(ui, state, &rows, selected);
            undated_card(ui, state, &rows, selected);
        }),
        _ => ui::scrolling(ui, "snapshots-day", |ui| {
            day_pane(ui, state, &rows, selected);
        }),
    });

    if let Some(target) = state.confirm_rollback.clone() {
        show_rollback_confirm(ui, state, &target);
    }
}

/// The local day a row was made on, or `None` for the undated ones.
fn day_of(row: &SnapshotRow) -> Option<i64> {
    row.created_at().map(calendar::local_day)
}

fn today() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    calendar::local_day(now)
}

/// Fill in what the user has not chosen: the newest snapshot's day, and the
/// month it is in. Returns the selection.
fn settle_view(state: &mut AppState, rows: &[SnapshotRow]) -> Selection {
    let view = &mut state.snapshot_view;
    let selected = *view.selected.get_or_insert_with(|| {
        // Rows come newest first.
        rows.iter()
            .find_map(day_of)
            .map(Selection::Day)
            .unwrap_or(Selection::Undated)
    });
    if view.month.is_none() {
        let day = match selected {
            Selection::Day(day) => day,
            Selection::Undated => rows.iter().find_map(day_of).unwrap_or_else(today),
        };
        view.month = Some(Month::of_day(day));
    }
    selected
}

/// The colour that sums up a row: what it is to the boot, or how it went.
fn marker(row: &SnapshotRow) -> Color32 {
    if row.is_running {
        style::SUCCESS
    } else if row.is_staged {
        style::WARNING
    } else if failed(row) {
        style::ERROR
    } else {
        style::ACCENT_STRONG
    }
}

fn failed(row: &SnapshotRow) -> bool {
    row.record.as_ref().is_some_and(|r| !r.outcome.succeeded())
}

fn calendar_card(ui: &mut Ui, state: &mut AppState, rows: &[SnapshotRow], selected: Selection) {
    let month = state
        .snapshot_view
        .month
        .unwrap_or_else(|| Month::of_day(today()));
    let newest = rows.iter().find_map(day_of);
    let mut go: Option<Month> = None;

    // What the title row asked for: the latest snapshot's day, or today's.
    let jump = ui::card_with(
        ui,
        "Calendar",
        |ui| {
            let latest = newest.filter(|_| ui::quiet(ui, true, "Latest").clicked());
            let today = ui::quiet(ui, true, "Today").clicked().then(today);
            latest.map(Selection::Day).or(today.map(Selection::Day))
        },
        |ui| {
            ui.horizontal(|ui| {
                if ui::button(ui, true, "\u{2039}").clicked() {
                    go = Some(month.prev());
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(month.label())
                        .font(style::medium(16.0))
                        .color(style::TEXT),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui::button(ui, true, "\u{203a}").clicked() {
                        go = Some(month.next());
                    }
                    let count = rows
                        .iter()
                        .filter_map(day_of)
                        .filter(|d| month.contains(*d))
                        .count();
                    let text = match count {
                        0 => "no snapshots".to_string(),
                        1 => "1 snapshot".to_string(),
                        n => format!("{n} snapshots"),
                    };
                    ui.label(RichText::new(text).color(style::TEXT_FAINT));
                });
            });
            ui.add_space(10.0);
            if let Some(day) = grid(ui, month, rows, selected) {
                state.snapshot_view.selected = Some(Selection::Day(day));
                if !month.contains(day) {
                    go = Some(Month::of_day(day));
                }
            }
            ui.add_space(10.0);
            legend(ui);
        },
    );

    if let Some(selection @ Selection::Day(day)) = jump {
        go = Some(Month::of_day(day));
        state.snapshot_view.selected = Some(selection);
    }
    if let Some(month) = go {
        state.snapshot_view.month = Some(month);
    }
}

/// The month grid. Returns the day clicked, if any.
fn grid(ui: &mut Ui, month: Month, rows: &[SnapshotRow], selected: Selection) -> Option<i64> {
    let width = ui.available_width();
    let cell_w = (width / 7.0).floor();
    let cell_h = (cell_w * 0.78).clamp(44.0, 72.0);
    let today = today();
    let mut clicked = None;

    // Weekday header.
    let (header, _) = ui.allocate_exact_size(egui::vec2(cell_w * 7.0, 20.0), Sense::hover());
    for (i, name) in WEEKDAYS.iter().enumerate() {
        let x = header.left() + cell_w * (i as f32 + 0.5);
        let colour = if i >= 5 {
            style::TEXT_FAINT
        } else {
            style::TEXT_DIM
        };
        ui.painter().text(
            egui::pos2(x, header.center().y),
            Align2::CENTER_CENTER,
            name.to_uppercase(),
            style::medium(10.5),
            colour,
        );
    }
    ui.add_space(4.0);

    let (area, _) = ui.allocate_exact_size(egui::vec2(cell_w * 7.0, cell_h * 6.0), Sense::hover());
    let days = month.grid();
    for (i, day) in days.iter().copied().enumerate() {
        let (row, col) = (i / 7, i % 7);
        let rect = egui::Rect::from_min_size(
            egui::pos2(
                area.left() + col as f32 * cell_w,
                area.top() + row as f32 * cell_h,
            ),
            egui::vec2(cell_w, cell_h),
        )
        .shrink(1.5);
        let response = ui.interact(rect, ui.id().with(("day", day)), Sense::click());
        let on_day: Vec<&SnapshotRow> = rows.iter().filter(|r| day_of(r) == Some(day)).collect();
        let in_month = month.contains(day);
        let is_selected = selected == Selection::Day(day);

        let painter = ui.painter();
        let fill = if is_selected {
            style::ACCENT_SOFT
        } else if response.hovered() {
            style::HOVER
        } else if in_month {
            style::INSET
        } else {
            style::CARD
        };
        let stroke = if is_selected {
            Stroke::new(1.0_f32, style::ACCENT)
        } else if in_month {
            Stroke::new(1.0_f32, style::BORDER)
        } else {
            Stroke::NONE
        };
        painter.rect(
            rect,
            CornerRadius::same(2),
            fill,
            stroke,
            StrokeKind::Inside,
        );

        let number_colour = if day == today {
            style::ACCENT
        } else if !in_month {
            style::BORDER_STRONG
        } else if on_day.is_empty() {
            style::TEXT_DIM
        } else {
            style::TEXT
        };
        let number_font = if day == today || !on_day.is_empty() {
            style::medium(12.5)
        } else {
            egui::FontId::proportional(12.5)
        };
        painter.text(
            rect.left_top() + egui::vec2(7.0, 5.0),
            Align2::LEFT_TOP,
            calendar::day_of_month(day).to_string(),
            number_font,
            number_colour,
        );
        if day == today {
            let under = egui::Rect::from_min_size(
                rect.left_top() + egui::vec2(7.0, 20.0),
                egui::vec2(12.0, 2.0),
            );
            painter.rect_filled(under, 0.0, style::ACCENT);
        }

        // One square per snapshot, oldest first, along the bottom.
        let mut x = rect.left() + 7.0;
        let y = rect.bottom() - 13.0;
        for row in on_day.iter().rev().take(MAX_MARKERS) {
            let square = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(7.0, 7.0));
            painter.rect_filled(square, CornerRadius::same(1), marker(row));
            x += 10.0;
        }
        if on_day.len() > MAX_MARKERS {
            painter.text(
                egui::pos2(x, y + 3.5),
                Align2::LEFT_CENTER,
                format!("+{}", on_day.len() - MAX_MARKERS),
                egui::FontId::proportional(10.0),
                style::TEXT_DIM,
            );
        }

        let response = if on_day.is_empty() {
            response
        } else {
            response.on_hover_text(match on_day.len() {
                1 => "1 snapshot".to_string(),
                n => format!("{n} snapshots"),
            })
        };
        if response.clicked() {
            clicked = Some(day);
        }
    }
    clicked
}

fn legend(ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        for (colour, text) in [
            (style::SUCCESS, "running"),
            (style::WARNING, "next boot"),
            (style::ERROR, "failed"),
            (style::ACCENT_STRONG, "snapshot"),
        ] {
            ui::swatch(ui, colour);
            ui.label(
                RichText::new(text)
                    .size(style::SMALL)
                    .color(style::TEXT_FAINT),
            );
            ui.add_space(8.0);
        }
    });
}

/// The restore points with no date, which the grid has no square for.
fn undated_card(ui: &mut Ui, state: &mut AppState, rows: &[SnapshotRow], selected: Selection) {
    let undated: Vec<&SnapshotRow> = rows.iter().filter(|r| day_of(r).is_none()).collect();
    if undated.is_empty() {
        return;
    }
    ui::card(ui, "Without a date", |ui| {
        let is_selected = selected == Selection::Undated;
        let names: Vec<String> = undated.iter().map(|r| r.title()).collect();
        let text = RichText::new(names.join(", ")).color(if is_selected {
            style::TEXT
        } else {
            style::TEXT_DIM
        });
        if ui.selectable_label(is_selected, text).clicked() {
            state.snapshot_view.selected = Some(Selection::Undated);
        }
        ui::note(
            ui,
            "The original install, and any slot no update has been recorded against.",
        );
    });
}

/// The selected day's snapshots, newest first.
fn day_pane(ui: &mut Ui, state: &mut AppState, rows: &[SnapshotRow], selected: Selection) {
    let on_day: Vec<&SnapshotRow> = rows
        .iter()
        .filter(|r| match selected {
            Selection::Day(day) => day_of(r) == Some(day),
            Selection::Undated => day_of(r).is_none(),
        })
        .collect();
    let title = match selected {
        Selection::Day(day) => calendar::long_date(day),
        Selection::Undated => "Without a date".to_string(),
    };

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .font(style::medium(17.0))
                .color(style::TEXT),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let text = match on_day.len() {
                0 => String::new(),
                1 => "1 snapshot".to_string(),
                n => format!("{n} snapshots"),
            };
            ui.label(RichText::new(text).color(style::TEXT_FAINT));
        });
    });
    ui.add_space(10.0);

    if on_day.is_empty() {
        ui::card(ui, "Nothing on this day", |ui| {
            ui::note(
                ui,
                "No update built a snapshot on this day. Days with a marker have one.",
            );
        });
        return;
    }
    for row in on_day {
        snapshot_card(ui, state, row);
    }
}

fn snapshot_card(ui: &mut Ui, state: &mut AppState, row: &SnapshotRow) {
    let busy = state.busy;
    let heading = match row.created_at() {
        Some(secs) if day_of(row).is_some() => calendar::local_time(secs),
        _ => row.title(),
    };
    let card = egui::Frame::new()
        .fill(style::CARD)
        .stroke(Stroke::new(1.0_f32, style::BORDER))
        .corner_radius(CornerRadius::same(style::CARD_RADIUS))
        .inner_margin(egui::Margin::same(style::PAD))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(heading)
                        .font(style::medium(16.0))
                        .color(style::TEXT),
                );
                if row.is_running {
                    ui::badge(ui, "running", Tone::Success);
                } else if row.is_staged {
                    ui::badge(ui, "next boot", Tone::Warning);
                }
                if failed(row) {
                    ui::badge(ui, "failed", Tone::Error);
                }
                // Rolling back to what is already booted is a no-op; the CLI
                // rejects it and there is nothing useful to offer.
                if !row.is_running {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui::button(ui, !busy, "Roll back to this").clicked() {
                            state.confirm_rollback = Some(row.clone());
                        }
                    });
                }
            });
            ui.add_space(4.0);
            ui::clamped(ui, &row.detail(), 3);
            ui::note(ui, describe_identity(row));

            if let Some(record) = &row.record {
                if let Outcome::Failed(_) = record.outcome {
                    ui.add_space(6.0);
                    ui::callout(
                        ui,
                        Tone::Error,
                        "This update did not complete; nothing was activated.",
                    );
                }
                if !record.changes.is_empty() {
                    ui.add_space(8.0);
                    let expanded = state.expanded.as_deref() == Some(row.target.as_str());
                    let count = record.changes.added.len()
                        + record.changes.upgraded.len()
                        + record.changes.removed.len();
                    let label = if expanded {
                        "Hide packages".to_string()
                    } else {
                        format!("Show {count} packages")
                    };
                    if ui::quiet(ui, true, &label).clicked() {
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
            }
        });
    // A strip down the left in the row's colour ties it to its marker.
    let rect = card.response.rect;
    let strip = egui::Rect::from_min_size(
        rect.left_top() + egui::vec2(0.0, 6.0),
        egui::vec2(2.0, rect.height() - 12.0),
    );
    ui.painter().rect_filled(strip, 0.0, marker(row));
    ui.add_space(style::GAP);
}

/// What the restore point is on disk, for anyone matching it up with the CLI.
fn describe_identity(row: &SnapshotRow) -> String {
    match row.target.as_str() {
        "@" => "The original install".to_string(),
        "A" | "B" => format!("Slot {}", row.target),
        id => format!("Snapshot set {id}"),
    }
}

fn show_changes(ui: &mut Ui, record: &UpdateRecord) {
    let changes = &record.changes;
    egui::Frame::new()
        .fill(style::INSET)
        .corner_radius(style::RADIUS)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical()
                .id_salt(("changes", record.started_at))
                .max_height(260.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for pkg in &changes.added {
                        line(
                            ui,
                            style::SUCCESS,
                            &format!("+ {} {}", pkg.name, pkg.version),
                        );
                    }
                    for pkg in &changes.upgraded {
                        line(
                            ui,
                            style::ACCENT,
                            &format!("~ {} {} \u{2192} {}", pkg.name, pkg.from, pkg.to),
                        );
                    }
                    for pkg in &changes.removed {
                        line(ui, style::ERROR, &format!("− {} {}", pkg.name, pkg.version));
                    }
                });
        });
}

fn line(ui: &mut Ui, color: Color32, text: &str) {
    ui.label(RichText::new(text).monospace().color(color));
}

fn show_rollback_confirm(ui: &mut Ui, state: &mut AppState, row: &SnapshotRow) {
    let mut open = true;
    egui::Window::new("Roll back?")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(440.0)
        .show(ui.ctx(), |ui| {
            ui.label(
                RichText::new(format!("Roll back to {}", row.title()))
                    .font(style::medium(15.0))
                    .color(style::TEXT),
            );
            ui.add_space(6.0);
            ui.label(
                "This moves the boot pointer only. Nothing is deleted, and it takes effect on \
                 the next reboot.",
            );
            if row.has_newer_updates {
                ui.add_space(8.0);
                ui::callout(
                    ui,
                    Tone::Neutral,
                    "Newer snapshots exist. Rolling back does not delete them; you can roll \
                     forward to one from this tab.",
                );
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui::primary(ui, true, "Roll back").clicked() {
                    let target = row.target.clone();
                    state.confirm_rollback = None;
                    state.start_rollback(target);
                }
                if ui::button(ui, true, "Cancel").clicked() {
                    state.confirm_rollback = None;
                }
            });
        });
    if !open {
        state.confirm_rollback = None;
    }
}
