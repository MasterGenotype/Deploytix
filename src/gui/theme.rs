//! Visual theme for the Deploytix GUI
//!
//! Centralizes all colors, spacing, and style configuration.

use egui::{Color32, CornerRadius, Stroke, Visuals};

// ── Color palette ──────────────────────────────────────────────────────

// Backgrounds
pub const BG_BASE: Color32 = Color32::from_rgb(15, 23, 42);
pub const BG_PANEL: Color32 = Color32::from_rgb(22, 34, 55);
pub const BG_SECTION: Color32 = Color32::from_rgb(30, 41, 62);
pub const BG_HOVER: Color32 = Color32::from_rgb(45, 58, 82);

// Accent (Artix-inspired cyan)
pub const ACCENT: Color32 = Color32::from_rgb(56, 189, 248);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(14, 165, 233);
pub const ACCENT_BG: Color32 = Color32::from_rgb(12, 74, 110);

// Semantic
pub const SUCCESS: Color32 = Color32::from_rgb(74, 222, 128);
pub const WARNING: Color32 = Color32::from_rgb(251, 191, 36);
pub const ERROR: Color32 = Color32::from_rgb(248, 113, 113);

// Text
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(226, 232, 240);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(148, 163, 184);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(100, 116, 139);

// ── Spacing ────────────────────────────────────────────────────────────

pub const SPACING_XS: f32 = 4.0;
pub const SPACING_SM: f32 = 8.0;
pub const SPACING_MD: f32 = 16.0;

// ── Theme application ──────────────────────────────────────────────────

/// Apply the Deploytix dark theme to the egui context.
pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();

    // Panel and window backgrounds
    visuals.panel_fill = BG_BASE;
    visuals.window_fill = BG_PANEL;
    visuals.extreme_bg_color = Color32::from_rgb(20, 30, 50);
    visuals.faint_bg_color = BG_SECTION;

    // Selection
    visuals.selection.bg_fill = ACCENT_BG;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);

    // Non-interactive widgets (labels, separators)
    visuals.widgets.noninteractive.bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);

    // Inactive widgets (buttons, checkboxes at rest)
    visuals.widgets.inactive.bg_fill = BG_SECTION;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    visuals.widgets.inactive.weak_bg_fill = BG_SECTION;

    // Hovered
    visuals.widgets.hovered.bg_fill = BG_HOVER;
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, TEXT_PRIMARY);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    visuals.widgets.hovered.weak_bg_fill = BG_HOVER;

    // Active (pressed)
    visuals.widgets.active.bg_fill = ACCENT_BG;
    visuals.widgets.active.fg_stroke = Stroke::new(2.0, ACCENT);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);
    visuals.widgets.active.weak_bg_fill = ACCENT_BG;

    // Open (e.g., ComboBox dropdown)
    visuals.widgets.open.bg_fill = BG_SECTION;
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.open.corner_radius = CornerRadius::same(6);

    // Miscellaneous
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = WARNING;
    visuals.error_fg_color = ERROR;

    ctx.set_visuals(visuals);

    // Adjust spacing for a more spacious feel
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    ctx.set_style(style);
}
