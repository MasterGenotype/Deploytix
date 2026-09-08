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

    // Zoom is not set here: it depends on the display and is recomputed every
    // frame by [`fit_to_screen`], which the app calls before drawing.

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);

    // Anything the user has to hit is deliberately larger than the text around
    // it. Zoom scales everything uniformly, so "bigger controls" has to be a
    // change in proportion, not in absolute size: these are all above egui's
    // defaults (button_padding 4x1, interact_size 40x18, icon_width 14,
    // scroll bar 6) while Body text stays where it was. On a handheld driven by
    // a trackpad or a thumb, that difference is the whole point.
    style.spacing.button_padding = egui::vec2(14.0, 9.0);
    style.spacing.interact_size = egui::vec2(48.0, 26.0);
    style.spacing.icon_width = 20.0;
    style.spacing.icon_width_inner = 12.0;
    style.spacing.scroll.bar_width = 11.0;

    // Button text one step above body text, for the same reason.
    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(22.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(16.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();

    ctx.set_style(style);
}

// ── Display scaling ────────────────────────────────────────────────────

/// Logical size the install wizard is laid out for.
///
/// The panels were designed to fit a 1920x1080 screen at the old hardcoded
/// 0.75 zoom, which is the same as saying they need this many points. Keeping
/// the number explicit is what lets the zoom be derived instead of guessed.
pub const WIZARD_REFERENCE: egui::Vec2 = egui::vec2(2560.0, 1440.0);

/// Logical size the transactional updater is laid out for. It is a windowed
/// utility, not a fullscreen wizard, so its reference is its own design size.
pub const UPDATER_REFERENCE: egui::Vec2 = egui::vec2(1200.0, 900.0);

/// Below this the text stops being readable; above it a large display just
/// wastes space on enormous controls.
const MIN_ZOOM: f32 = 0.4;
const MAX_ZOOM: f32 = 2.0;

/// Don't re-apply for changes too small to see; every change triggers a
/// relayout.
const ZOOM_EPSILON: f32 = 0.005;

/// The zoom factor that makes `reference` fit in the current screen.
///
/// `screen_points` is the screen size in points, which already depends on
/// `current_zoom` — so the calculation first recovers the zoom-independent
/// size (`screen_points * current_zoom`) and works from that. Without this the
/// result would feed back into its own input and oscillate.
///
/// The smaller of the two axes wins, so nothing is ever cut off: fitting the
/// wider axis alone would push the taller one off-screen.
pub fn fit_zoom_factor(screen_points: egui::Vec2, current_zoom: f32, reference: egui::Vec2) -> f32 {
    if screen_points.x <= 0.0 || screen_points.y <= 0.0 || current_zoom <= 0.0 {
        return current_zoom.clamp(MIN_ZOOM, MAX_ZOOM);
    }
    let absolute = screen_points * current_zoom;
    let fit = (absolute.x / reference.x).min(absolute.y / reference.y);
    fit.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// Rescale the UI so `reference` fits the current display.
///
/// Only acts when the display size actually changes. That is deliberate: the
/// zoom is also the user's, reachable with Ctrl+`+`/`-`, and re-deriving it
/// every frame silently reverted their choice on the next repaint. The
/// remembered size is zoom-independent, so a manual zoom does not look like a
/// display change and does not trigger a refit.
pub fn fit_to_screen(ctx: &egui::Context, reference: egui::Vec2) {
    let current = ctx.zoom_factor();
    // Zoom-independent, so this is stable across our own changes and across
    // anything the user does with Ctrl+ +/-.
    let absolute = ctx.screen_rect().size() * current;

    let id = egui::Id::new("deploytix-fitted-screen");
    let previous: Option<egui::Vec2> = ctx.data(|d| d.get_temp(id));
    if let Some(previous) = previous {
        if (previous - absolute).length() < 1.0 {
            return; // same display; leave the zoom alone, including theirs
        }
    }
    ctx.data_mut(|d| d.insert_temp(id, absolute));

    let target = fit_zoom_factor(ctx.screen_rect().size(), current, reference);
    if (current - target).abs() > ZOOM_EPSILON {
        ctx.set_zoom_factor(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    /// Screen size in points for a physical display, at a given zoom.
    /// This is the relationship `fit_zoom_factor` has to invert.
    fn points_for(physical: egui::Vec2, zoom: f32) -> egui::Vec2 {
        physical / zoom
    }

    /// The old code hardcoded 0.75, chosen so the configuration grid fits a
    /// 1080p screen. Deriving it must reproduce that, or every existing panel
    /// layout changes underfoot.
    #[test]
    fn a_1080p_screen_still_gets_the_zoom_the_layout_was_built_for() {
        let physical = vec2(1920.0, 1080.0);
        let z = fit_zoom_factor(points_for(physical, 1.0), 1.0, WIZARD_REFERENCE);
        assert!((z - 0.75).abs() < 0.001, "expected 0.75, got {z}");
    }

    /// The whole point: a smaller panel scales down so nothing is cut off, and
    /// a larger one scales up instead of leaving the wizard marooned in a
    /// corner.
    #[test]
    fn smaller_screens_scale_down_and_larger_ones_scale_up() {
        let at =
            |w: f32, h: f32| fit_zoom_factor(points_for(vec2(w, h), 1.0), 1.0, WIZARD_REFERENCE);

        let handheld = at(1280.0, 800.0);
        let fullhd = at(1920.0, 1080.0);
        let uhd = at(3840.0, 2160.0);

        assert!(handheld < fullhd, "1280x800 must scale down: {handheld}");
        assert!(uhd > fullhd, "3840x2160 must scale up: {uhd}");
    }

    /// Fitting the wider axis alone would push the taller one off-screen, which
    /// is exactly the "everything must be visible" requirement failing. The
    /// tighter axis has to win.
    #[test]
    fn the_tighter_axis_decides() {
        // Very wide and short: height is the constraint.
        let wide = fit_zoom_factor(points_for(vec2(3840.0, 900.0), 1.0), 1.0, WIZARD_REFERENCE);
        assert!(
            (wide - 900.0 / WIZARD_REFERENCE.y).abs() < 0.001,
            "height should decide, got {wide}"
        );

        // Tall and narrow: width is the constraint.
        let tall = fit_zoom_factor(points_for(vec2(1000.0, 2160.0), 1.0), 1.0, WIZARD_REFERENCE);
        assert!(
            (tall - (1000.0 / WIZARD_REFERENCE.x).max(MIN_ZOOM)).abs() < 0.001,
            "width should decide, got {tall}"
        );
    }

    /// The hazard in doing this per frame: the screen size in points is itself
    /// a function of the zoom, so a naive calculation feeds its own output back
    /// into its input and oscillates forever, repainting every frame.
    ///
    /// Applying the result and recomputing must land on the same number.
    #[test]
    fn the_result_is_a_fixed_point_and_does_not_oscillate() {
        for physical in [
            vec2(1280.0, 800.0),
            vec2(1920.0, 1080.0),
            vec2(2560.0, 1600.0),
            vec2(3840.0, 2160.0),
        ] {
            let mut zoom = 1.0_f32;
            for _ in 0..10 {
                zoom = fit_zoom_factor(points_for(physical, zoom), zoom, WIZARD_REFERENCE);
            }
            let settled = fit_zoom_factor(points_for(physical, zoom), zoom, WIZARD_REFERENCE);
            assert!(
                (settled - zoom).abs() < 1e-4,
                "{physical:?} oscillates: {zoom} -> {settled}"
            );
        }
    }

    /// The zoom is the user's too: egui binds Ctrl+`+`/`-` to it. Re-deriving
    /// it every frame silently reverted their choice on the next repaint, so
    /// the UI could not be zoomed out at all.
    ///
    /// `fit_to_screen` guards on a remembered screen size that is
    /// zoom-independent, so a manual zoom must not look like a display change.
    /// This checks that invariant directly: the value the guard compares stays
    /// put when only the zoom moves.
    #[test]
    fn a_manual_zoom_does_not_look_like_a_display_change() {
        let physical = vec2(1920.0, 1080.0);

        // What fit_to_screen remembers: screen_rect * zoom_factor.
        let remembered = |zoom: f32| points_for(physical, zoom) * zoom;

        let fitted = remembered(0.75);
        for user_zoom in [0.4_f32, 0.5, 0.75, 1.0, 1.6] {
            let after = remembered(user_zoom);
            assert!(
                (fitted - after).length() < 1.0,
                "zooming to {user_zoom} looked like a display change \
                 ({fitted:?} vs {after:?}), which would snap the zoom back"
            );
        }
    }

    /// ...but an actual change of display must still refit.
    #[test]
    fn a_real_display_change_is_still_detected() {
        let remembered = |physical: egui::Vec2, zoom: f32| points_for(physical, zoom) * zoom;
        let before = remembered(vec2(1920.0, 1080.0), 0.75);
        let after = remembered(vec2(1280.0, 800.0), 0.75);
        assert!(
            (before - after).length() >= 1.0,
            "docking to a different panel must trigger a refit"
        );
    }

    /// Text has to stay readable on a tiny panel, and a large display should
    /// not turn into enormous controls.
    #[test]
    fn zoom_is_clamped_at_both_ends() {
        let tiny = fit_zoom_factor(points_for(vec2(320.0, 240.0), 1.0), 1.0, WIZARD_REFERENCE);
        assert!((tiny - MIN_ZOOM).abs() < 1e-6, "got {tiny}");

        let huge = fit_zoom_factor(
            points_for(vec2(15360.0, 8640.0), 1.0),
            1.0,
            WIZARD_REFERENCE,
        );
        assert!((huge - MAX_ZOOM).abs() < 1e-6, "got {huge}");
    }

    /// A zero-sized screen is what the first frame can report before the window
    /// is mapped. It must not produce a NaN or a zero zoom.
    #[test]
    fn a_degenerate_screen_size_is_survivable() {
        for bad in [vec2(0.0, 0.0), vec2(1920.0, 0.0), vec2(-1.0, -1.0)] {
            let z = fit_zoom_factor(bad, 1.0, WIZARD_REFERENCE);
            assert!(z.is_finite() && z > 0.0, "{bad:?} gave {z}");
        }
        let z = fit_zoom_factor(vec2(1920.0, 1080.0), 0.0, WIZARD_REFERENCE);
        assert!(z.is_finite() && z > 0.0, "zero current zoom gave {z}");
    }

    /// The updater is a window, not a fullscreen wizard, so it must not be
    /// scaled against the wizard's much larger reference.
    #[test]
    fn the_updater_has_its_own_reference() {
        assert!(UPDATER_REFERENCE.x < WIZARD_REFERENCE.x);
        assert!(UPDATER_REFERENCE.y < WIZARD_REFERENCE.y);

        // Its default 900x700 window should not be driven to the floor.
        let z = fit_zoom_factor(points_for(vec2(900.0, 700.0), 1.0), 1.0, UPDATER_REFERENCE);
        assert!(z > MIN_ZOOM, "updater window pinned at the floor: {z}");
    }
}
