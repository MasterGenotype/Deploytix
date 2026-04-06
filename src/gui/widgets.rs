//! Reusable UI widgets and components

use super::{state::WizardStep, theme};
use egui::{Color32, CornerRadius, FontId, Margin, RichText, Stroke, Ui};

// ── Step indicator ─────────────────────────────────────────────────────

/// Render the wizard step indicator with circles and connecting lines.
pub fn step_indicator(ui: &mut Ui, current: WizardStep) {
    let steps = WizardStep::ALL;
    let current_idx = current.index();

    let desired_size = egui::vec2(ui.available_width(), 58.0);
    let (response, painter) = ui.allocate_painter(desired_size, egui::Sense::hover());
    let rect = response.rect;

    let radius = 14.0;
    let y_circle = rect.top() + 20.0;
    let y_label = y_circle + radius + 10.0;
    let margin_x = 45.0;
    let usable = rect.width() - margin_x * 2.0;
    let step_dx = if steps.len() > 1 {
        usable / (steps.len() - 1) as f32
    } else {
        0.0
    };

    for (i, step) in steps.iter().enumerate() {
        let x = rect.left() + margin_x + i as f32 * step_dx;
        let center = egui::pos2(x, y_circle);
        let is_current = i == current_idx;
        let is_past = i < current_idx;

        // Connecting line to previous step
        if i > 0 {
            let prev_x = rect.left() + margin_x + (i - 1) as f32 * step_dx;
            let color = if i <= current_idx {
                theme::ACCENT_DIM
            } else {
                theme::BG_HOVER
            };
            painter.line_segment(
                [
                    egui::pos2(prev_x + radius + 3.0, y_circle),
                    egui::pos2(x - radius - 3.0, y_circle),
                ],
                Stroke::new(2.0, color),
            );
        }

        // Circle
        if is_current {
            painter.circle_filled(center, radius + 3.0, theme::ACCENT_BG);
            painter.circle_filled(center, radius, theme::ACCENT);
        } else if is_past {
            painter.circle_filled(center, radius, theme::SUCCESS);
        } else {
            painter.circle_stroke(center, radius, Stroke::new(2.0, theme::TEXT_MUTED));
        }

        // Number / checkmark
        let icon_color = if is_current || is_past {
            Color32::WHITE
        } else {
            theme::TEXT_MUTED
        };
        let icon = if is_past {
            "\u{2713}".to_string()
        } else {
            format!("{}", i + 1)
        };
        painter.text(
            center,
            egui::Align2::CENTER_CENTER,
            icon,
            FontId::proportional(if is_past { 14.0 } else { 12.0 }),
            icon_color,
        );

        // Label
        let label_color = if is_current {
            Color32::WHITE
        } else if is_past {
            theme::TEXT_SECONDARY
        } else {
            theme::TEXT_MUTED
        };
        painter.text(
            egui::pos2(x, y_label),
            egui::Align2::CENTER_TOP,
            step.label(),
            FontId::proportional(11.0),
            label_color,
        );
    }
}

// ── Section card ───────────────────────────────────────────────────────

/// Render a framed section card with a title and body content.
pub fn section(ui: &mut Ui, title: &str, add_body: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::BG_SECTION)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(14))
        .stroke(Stroke::new(1.0, theme::BG_HOVER))
        .show(ui, |ui| {
            ui.label(
                RichText::new(title)
                    .strong()
                    .size(14.0)
                    .color(theme::ACCENT),
            );
            ui.add_space(theme::SPACING_SM);
            add_body(ui);
        });
    ui.add_space(theme::SPACING_SM);
}

// ── Validation messages ────────────────────────────────────────────────

/// Render a validation error message.
pub fn validation_error(ui: &mut Ui, msg: &str) {
    ui.label(RichText::new(format!("\u{26a0} {}", msg)).color(theme::ERROR));
}

/// Render a validation warning message.
pub fn validation_warning(ui: &mut Ui, msg: &str) {
    ui.label(RichText::new(format!("\u{26a0} {}", msg)).color(theme::WARNING));
}

/// Render muted informational text.
pub fn info_text(ui: &mut Ui, msg: &str) {
    ui.label(RichText::new(msg).color(theme::TEXT_MUTED).size(12.0));
}

// ── Styled buttons ─────────────────────────────────────────────────────

/// Primary accent-colored button.
pub fn primary_button(ui: &mut Ui, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
            .fill(theme::ACCENT_DIM)
            .corner_radius(CornerRadius::same(6)),
    )
}

/// Primary button that can be enabled/disabled.
pub fn primary_button_enabled(ui: &mut Ui, enabled: bool, label: &str) -> egui::Response {
    let fill = if enabled {
        theme::ACCENT_DIM
    } else {
        theme::BG_HOVER
    };
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
            .fill(fill)
            .corner_radius(CornerRadius::same(6)),
    )
}

// ── Headings ───────────────────────────────────────────────────────────

/// Render a page heading.
pub fn page_heading(ui: &mut Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .strong()
            .size(20.0)
            .color(Color32::WHITE),
    );
    ui.add_space(theme::SPACING_SM);
}
