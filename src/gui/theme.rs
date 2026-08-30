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

// ── Layout ─────────────────────────────────────────────────────────────

/// Narrowest a configuration column may get before the grid drops to fewer
/// columns.  Below this, combo boxes and sliders start colliding with their
/// labels and the panel looks "sandwiched".
pub const MIN_COLUMN_WIDTH: f32 = 300.0;

// ── Zoom ───────────────────────────────────────────────────────────────

/// Zoom factor applied on startup.  The layout is fully scrollable and
/// reflows to fewer columns when cramped, so this is only a starting point —
/// the user can change it from the header controls or with Ctrl +/-/0.
pub const DEFAULT_ZOOM: f32 = 0.75;

/// Smallest zoom factor reachable from the header controls.
pub const MIN_ZOOM: f32 = 0.50;

/// Largest zoom factor reachable from the header controls.
pub const MAX_ZOOM: f32 = 2.00;

/// Increment applied by the zoom in/out buttons.
pub const ZOOM_STEP: f32 = 0.05;

/// Nudge the zoom factor by `delta`, clamped to the supported range.
pub fn adjust_zoom(ctx: &egui::Context, delta: f32) {
    let next = (ctx.zoom_factor() + delta).clamp(MIN_ZOOM, MAX_ZOOM);
    ctx.set_zoom_factor(next);
}

/// Restore the startup zoom factor.
pub fn reset_zoom(ctx: &egui::Context) {
    ctx.set_zoom_factor(DEFAULT_ZOOM);
}

// ── Theme application ──────────────────────────────────────────────────

/// Add Hack as a fallback for the proportional font family.
///
/// egui's proportional stack is Ubuntu-Light → NotoEmoji → emoji-icon-font,
/// and none of the three carries the arrows this UI uses, so "\u{2190} Back"
/// and "Next \u{2192}" render as tofu boxes.  Hack is already bundled with
/// epaint's default fonts for the monospace family, so appending it here
/// fills the gaps without adding an asset or a byte to the binary.
fn install_font_fallbacks(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(proportional) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        if !proportional.iter().any(|name| name == "Hack") {
            proportional.push("Hack".to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

/// Apply the Deploytix dark theme to the egui context.
pub fn apply(ctx: &egui::Context) {
    install_font_fallbacks(ctx);

    let mut visuals = Visuals::dark();

    // Panel and window backgrounds
    visuals.panel_fill = BG_BASE;
    visuals.window_fill = BG_PANEL;
    visuals.extreme_bg_color = Color32::from_rgb(20, 30, 50);
    visuals.faint_bg_color = BG_SECTION;

    // Selection
    visuals.selection.bg_fill = ACCENT_BG;
    visuals.selection.stroke = Stroke::new(1.0_f32, ACCENT);

    // Non-interactive widgets (labels, separators)
    visuals.widgets.noninteractive.bg_fill = BG_PANEL;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT_SECONDARY);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(6);

    // Inactive widgets (buttons, checkboxes at rest)
    visuals.widgets.inactive.bg_fill = BG_SECTION;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT_PRIMARY);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    visuals.widgets.inactive.weak_bg_fill = BG_SECTION;

    // Hovered
    visuals.widgets.hovered.bg_fill = BG_HOVER;
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, TEXT_PRIMARY);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    visuals.widgets.hovered.weak_bg_fill = BG_HOVER;

    // Active (pressed)
    visuals.widgets.active.bg_fill = ACCENT_BG;
    visuals.widgets.active.fg_stroke = Stroke::new(2.0_f32, ACCENT);
    visuals.widgets.active.corner_radius = CornerRadius::same(6);
    visuals.widgets.active.weak_bg_fill = ACCENT_BG;

    // Open (e.g., ComboBox dropdown)
    visuals.widgets.open.bg_fill = BG_SECTION;
    visuals.widgets.open.fg_stroke = Stroke::new(1.0_f32, ACCENT);
    visuals.widgets.open.corner_radius = CornerRadius::same(6);

    // Miscellaneous
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = WARNING;
    visuals.error_fg_color = ERROR;

    ctx.set_visuals(visuals);

    // Start scaled down so the configuration grid fits on one screen on a
    // typical display.  Nothing depends on this value: the panel scrolls and
    // reflows, so any zoom the user picks stays usable.
    ctx.set_zoom_factor(DEFAULT_ZOOM);

    // Adjust spacing for a more spacious feel
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    ctx.set_style(style);
}
