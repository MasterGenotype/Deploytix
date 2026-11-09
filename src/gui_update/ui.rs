//! The updater's building blocks: panes laid side by side, cards, badges,
//! callouts and buttons, all drawn from [`super::style`].

use super::style;
use eframe::egui;
use egui::{
    Align, Color32, CornerRadius, Layout, Margin, Rect, Response, RichText, Sense, Stroke,
    StrokeKind, Ui, UiBuilder,
};

/// Lay panes out side by side across the rest of `ui`, widths in proportion
/// to `weights`, each the full remaining height. `add` fills pane `i`.
///
/// Each tab is a row of these rather than one long column, so a landscape
/// window shows a whole tab at once and only a pane that overflows scrolls.
pub fn panes(ui: &mut Ui, weights: &[f32], mut add: impl FnMut(usize, &mut Ui)) {
    let area = ui.available_rect_before_wrap();
    let gaps = style::GAP * weights.len().saturating_sub(1) as f32;
    let total: f32 = weights.iter().sum();
    let mut left = area.left();
    for (i, weight) in weights.iter().enumerate() {
        let width = (area.width() - gaps) * weight / total;
        let rect = Rect::from_min_size(
            egui::pos2(left, area.top()),
            egui::vec2(width, area.height()),
        );
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min))
                .id_salt(("pane", i)),
            |ui| {
                // A pane never draws over its neighbour, whatever is in it.
                ui.set_clip_rect(rect.expand(1.0).intersect(ui.clip_rect()));
                ui.set_min_size(rect.size());
                add(i, ui)
            },
        );
        left += width + style::GAP;
    }
    ui.advance_cursor_after_rect(area);
}

/// A pane whose cards scroll together when they outgrow it.
pub fn scrolling(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show(ui, add);
}

fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(style::CARD)
        .stroke(Stroke::new(1.0_f32, style::BORDER))
        .corner_radius(CornerRadius::same(style::CARD_RADIUS))
        .inner_margin(Margin::same(style::PAD))
}

/// A titled card the width of its pane.
pub fn card(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    card_with(ui, title, |_| {}, body);
}

/// A card with controls on the right of its title row. Returns what
/// `trailing` did, so it can report a click without sharing state with `body`.
pub fn card_with<T>(
    ui: &mut Ui,
    title: &str,
    trailing: impl FnOnce(&mut Ui) -> T,
    body: impl FnOnce(&mut Ui),
) -> T {
    let out = card_frame()
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let out = card_title(ui, title, trailing);
            body(ui);
            out
        })
        .inner;
    ui.add_space(style::GAP);
    out
}

/// A card that takes the rest of its pane's height, for a list or a log
/// that should use whatever room there is. `body` gets the height left
/// under the title.
pub fn card_fill<T>(
    ui: &mut Ui,
    title: &str,
    trailing: impl FnOnce(&mut Ui) -> T,
    body: impl FnOnce(&mut Ui, f32),
) -> T {
    let outer = ui.available_height();
    card_frame()
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let inner = (outer - 2.0 * f32::from(style::PAD) - 2.0).max(80.0);
            ui.set_min_height(inner);
            let top = ui.cursor().top();
            let out = card_title(ui, title, trailing);
            let used = ui.cursor().top() - top;
            body(ui, (inner - used).max(40.0));
            out
        })
        .inner
}

fn card_title<T>(ui: &mut Ui, title: &str, trailing: impl FnOnce(&mut Ui) -> T) -> T {
    let out = ui
        .horizontal(|ui| {
            ui.set_min_height(22.0);
            ui.label(
                RichText::new(title.to_uppercase())
                    .font(style::medium(11.0))
                    .extra_letter_spacing(0.8)
                    .color(style::TEXT_FAINT),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), trailing)
                .inner
        })
        .inner;
    ui.add_space(6.0);
    out
}

/// A hairline across the card.
pub fn rule(ui: &mut Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, style::BORDER);
}

/// The colour family of a badge or callout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Error,
}

impl Tone {
    pub fn fg(self) -> Color32 {
        match self {
            Self::Neutral => style::TEXT_DIM,
            Self::Accent => style::ACCENT,
            Self::Success => style::SUCCESS,
            Self::Warning => style::WARNING,
            Self::Error => style::ERROR,
        }
    }

    pub fn bg(self) -> Color32 {
        match self {
            Self::Neutral => style::RAISED,
            Self::Accent => style::ACCENT_SOFT,
            Self::Success => style::SUCCESS_SOFT,
            Self::Warning => style::WARNING_SOFT,
            Self::Error => style::ERROR_SOFT,
        }
    }
}

/// A small status tag: "running", "held", "out of date".
pub fn badge(ui: &mut Ui, text: &str, tone: Tone) -> Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_string(),
        style::medium(style::SMALL - 0.5),
        tone.fg(),
    );
    let size = galley.size() + egui::vec2(12.0, 4.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect(
        rect,
        CornerRadius::same(2),
        tone.bg(),
        Stroke::new(1.0_f32, tone.fg().gamma_multiply(0.35)),
        StrokeKind::Inside,
    );
    painter.galley(rect.center() - galley.size() / 2.0, galley, tone.fg());
    response
}

/// A message set off from the card: a tinted box with a bar down its left.
pub fn callout(ui: &mut Ui, tone: Tone, text: &str) {
    callout_with(ui, tone, |ui| {
        ui.label(RichText::new(text).color(tone_text(tone)));
    });
}

pub fn callout_with(ui: &mut Ui, tone: Tone, body: impl FnOnce(&mut Ui)) {
    let response = egui::Frame::new()
        .fill(tone.bg())
        .corner_radius(CornerRadius::same(style::RADIUS))
        .inner_margin(Margin {
            left: 12,
            right: 10,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui)
        })
        .response;
    let bar = Rect::from_min_size(response.rect.min, egui::vec2(2.0, response.rect.height()));
    ui.painter().rect_filled(bar, 0.0, tone.fg());
}

/// Body text on a tinted callout: the tone's colour for a warning or an
/// error, plain text otherwise.
fn tone_text(tone: Tone) -> Color32 {
    match tone {
        Tone::Warning | Tone::Error => tone.fg(),
        _ => style::TEXT,
    }
}

/// Secondary explanatory text.
pub fn note(ui: &mut Ui, text: impl Into<String>) {
    ui.label(
        RichText::new(text.into())
            .size(style::SMALL)
            .color(style::TEXT_FAINT),
    );
}

/// A small heading inside a card.
pub fn label(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(style::medium(12.0))
            .color(style::TEXT_DIM),
    );
}

/// Key/value rows, keys in a fixed column and values wrapping beside them.
pub fn facts(ui: &mut Ui, rows: &[(&str, String)]) {
    const KEY_WIDTH: f32 = 96.0;
    for (key, value) in rows {
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(KEY_WIDTH, 18.0),
                Layout::left_to_right(Align::Min),
                |ui| {
                    ui.set_min_width(KEY_WIDTH);
                    ui.label(RichText::new(*key).color(style::TEXT_FAINT));
                },
            );
            ui.add(egui::Label::new(RichText::new(value).color(style::TEXT)).wrap());
        });
        ui.add_space(2.0);
    }
}

/// A row with `right` pinned to the right edge and `left` in whatever width
/// that leaves, so a long name truncates instead of running under the badges.
/// `left` should use truncating labels.
pub fn row(ui: &mut Ui, right: impl FnOnce(&mut Ui), left: impl FnOnce(&mut Ui)) {
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, Layout::right_to_left(Align::Center), |ui| {
        right(ui);
        let rest = egui::vec2(ui.available_width().max(0.0), size.y);
        ui.allocate_ui_with_layout(rest, Layout::left_to_right(Align::Center), |ui| {
            ui.set_max_width(rest.x);
            left(ui);
        });
    });
}

/// A label cut to one line with an ellipsis, the whole text on hover.
pub fn truncated(ui: &mut Ui, text: RichText) -> Response {
    let full = text.text().to_string();
    let response = ui.add(egui::Label::new(text).truncate());
    response.on_hover_text(full)
}

/// Text wrapped to at most `rows` lines, ending in an ellipsis when cut, with
/// the whole text on hover. For details that can run to a screenful, like a
/// failed command line.
pub fn clamped(ui: &mut Ui, text: &str, rows: usize) -> Response {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat {
            font_id: egui::FontId::proportional(style::BODY),
            color: style::TEXT_DIM,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: ui.available_width(),
        max_rows: rows,
        break_anywhere: false,
        overflow_character: Some('\u{2026}'),
    };
    let galley = ui.fonts(|f| f.layout_job(job));
    let elided = galley.elided;
    let response = ui.label(galley);
    if elided {
        response.on_hover_text(text)
    } else {
        response
    }
}

/// Text centred in the space left, for an empty pane or card.
pub fn empty(ui: &mut Ui, title: &str, text: &str) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.label(
            RichText::new(title)
                .font(style::medium(14.0))
                .color(style::TEXT_DIM),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(text)
                .size(style::SMALL)
                .color(style::TEXT_FAINT),
        );
    });
    ui.add_space(24.0);
}

/// A single-line text field, inset, full width.
pub fn field(ui: &mut Ui, text: &mut String, hint: &str) -> Response {
    ui.add(
        egui::TextEdit::singleline(text)
            .hint_text(RichText::new(hint).color(style::TEXT_FAINT))
            .margin(Margin::symmetric(8, 5))
            .desired_width(f32::INFINITY),
    )
}

fn filled(label: &str, fill: Color32, text: Color32) -> egui::Button<'static> {
    egui::Button::new(RichText::new(label.to_string()).color(text))
        .fill(fill)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(style::RADIUS))
        .min_size(egui::vec2(0.0, 30.0))
}

/// The one action a card is for.
pub fn primary(ui: &mut Ui, enabled: bool, label: &str) -> Response {
    let (fill, text) = if enabled {
        (style::ACCENT_STRONG, Color32::WHITE)
    } else {
        (style::RAISED, style::TEXT_FAINT)
    };
    ui.add_enabled(enabled, filled(label, fill, text))
}

/// An action that takes something away.
pub fn danger(ui: &mut Ui, enabled: bool, label: &str) -> Response {
    let (fill, text) = if enabled {
        (style::DANGER, Color32::WHITE)
    } else {
        (style::RAISED, style::TEXT_FAINT)
    };
    ui.add_enabled(enabled, filled(label, fill, text))
}

/// An ordinary bordered button.
pub fn button(ui: &mut Ui, enabled: bool, label: &str) -> Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(label).min_size(egui::vec2(0.0, 30.0)),
    )
}

/// A borderless text button for a card's title row: "Check again", "‹".
pub fn quiet(ui: &mut Ui, enabled: bool, label: &str) -> Response {
    ui.add_enabled(
        enabled,
        egui::Button::new(
            RichText::new(label)
                .font(style::medium(12.0))
                .color(style::ACCENT),
        )
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::NONE)
        .small(),
    )
}

/// What a drawn icon shows. Drawn rather than typed: the system faces the
/// updater picks up do not all have the check mark or the cross.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Check,
    Cross,
}

fn paint_icon(ui: &Ui, rect: Rect, icon: Icon, colour: Color32) {
    let stroke = Stroke::new(1.6_f32, colour);
    let c = rect.center();
    let r = rect.width().min(rect.height()) * 0.28;
    let p = |x: f32, y: f32| egui::pos2(c.x + x * r, c.y + y * r);
    match icon {
        Icon::Check => {
            ui.painter()
                .line(vec![p(-1.0, 0.0), p(-0.3, 0.75), p(1.0, -0.8)], stroke);
        }
        Icon::Cross => {
            ui.painter()
                .line_segment([p(-0.8, -0.8), p(0.8, 0.8)], stroke);
            ui.painter()
                .line_segment([p(-0.8, 0.8), p(0.8, -0.8)], stroke);
        }
    }
}

/// A small square button with a drawn icon, for taking an item off a list.
pub fn icon_button(ui: &mut Ui, icon: Icon, hint: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), Sense::click());
    let colour = if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(2), style::HOVER);
        style::TEXT
    } else {
        style::TEXT_DIM
    };
    paint_icon(ui, rect, icon, colour);
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(hint)
}

/// A button that stays lit while `on`: "Queue" / "Queued".
pub fn toggle(ui: &mut Ui, on: bool, label_off: &str, label_on: &str) -> Response {
    let label = if on { label_on } else { label_off };
    let font = style::medium(style::BODY);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font, style::TEXT);
    let icon_room = if on { 18.0 } else { 0.0 };
    let size = egui::vec2(galley.size().x + 24.0 + icon_room, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let (fill, stroke, text) = if on {
        (style::ACCENT_SOFT, style::ACCENT, style::ACCENT)
    } else if response.hovered() {
        (style::HOVER, style::BORDER_STRONG, style::TEXT)
    } else {
        (style::RAISED, style::BORDER_STRONG, style::TEXT)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(style::RADIUS),
        fill,
        Stroke::new(1.0_f32, stroke),
        StrokeKind::Inside,
    );
    let text_x = rect.left() + 12.0 + icon_room;
    if on {
        let icon = Rect::from_center_size(
            egui::pos2(rect.left() + 18.0, rect.center().y),
            egui::vec2(16.0, 16.0),
        );
        paint_icon(ui, icon, Icon::Check, text);
    }
    ui.painter().galley(
        egui::pos2(text_x, rect.center().y - galley.size().y / 2.0),
        galley,
        text,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small square marker, the calendar's and the legend's.
pub fn swatch(ui: &mut Ui, colour: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(1), colour);
}

/// A tab in the strip: label, and an optional dot for "needs a look".
pub struct TabItem<'a> {
    pub label: &'a str,
    pub selected: bool,
    pub dot: Option<Color32>,
}

/// Text tabs with an accent rule under the selected one. Returns the index
/// clicked, if any.
pub fn tab_strip(ui: &mut Ui, tabs: &[TabItem<'_>]) -> Option<usize> {
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (i, tab) in tabs.iter().enumerate() {
            let font = style::medium(13.5);
            let galley = ui
                .painter()
                .layout_no_wrap(tab.label.to_string(), font, style::TEXT);
            let dot_room = if tab.dot.is_some() { 12.0 } else { 0.0 };
            let size = egui::vec2(galley.size().x + 28.0 + dot_room, 36.0);
            let (rect, response) = ui.allocate_exact_size(size, Sense::click());
            let colour = if tab.selected || response.hovered() {
                style::TEXT
            } else {
                style::TEXT_DIM
            };
            let painter = ui.painter();
            if response.hovered() && !tab.selected {
                painter.rect_filled(rect.shrink2(egui::vec2(0.0, 4.0)), 2.0, style::RAISED);
            }
            let text_pos = egui::pos2(rect.left() + 14.0, rect.center().y - galley.size().y / 2.0);
            painter.galley(text_pos, galley.clone(), colour);
            if let Some(dot) = tab.dot {
                painter.circle_filled(
                    egui::pos2(text_pos.x + galley.size().x + 8.0, rect.center().y - 4.0),
                    3.0,
                    dot,
                );
            }
            if tab.selected {
                let underline = Rect::from_min_max(
                    egui::pos2(rect.left() + 8.0, rect.bottom() - 2.0),
                    egui::pos2(rect.right() - 8.0, rect.bottom()),
                );
                painter.rect_filled(underline, 0.0, style::ACCENT);
            }
            if response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                clicked = Some(i);
            }
        }
    });
    clicked
}
