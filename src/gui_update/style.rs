//! The updater's look.
//!
//! A flat graphite palette with the installer's cyan as its one accent,
//! near-square corners and hairline borders. The installer wizard keeps
//! [`crate::gui::theme`]; the updater is a dense desktop utility rather than a
//! fullscreen walk-through, so it is styled on its own and shares only the
//! accent and the display scaling.

use crate::gui::theme;
use eframe::egui;
use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke};
use std::sync::Arc;

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

// ── Surfaces ──
/// The window behind everything.
pub const BG: Color32 = rgb(13, 15, 19);
/// Header, tab strip and status bar.
pub const BAR: Color32 = rgb(17, 20, 25);
pub const CARD: Color32 = rgb(21, 24, 30);
/// Text fields, consoles, anything sunk into a card.
pub const INSET: Color32 = rgb(11, 13, 16);
/// Buttons at rest.
pub const RAISED: Color32 = rgb(30, 34, 42);
pub const HOVER: Color32 = rgb(40, 45, 55);
pub const BORDER: Color32 = rgb(35, 40, 49);
pub const BORDER_STRONG: Color32 = rgb(54, 61, 74);

// ── Text ──
pub const TEXT: Color32 = rgb(230, 233, 238);
pub const TEXT_DIM: Color32 = rgb(152, 160, 172);
pub const TEXT_FAINT: Color32 = rgb(104, 112, 125);

// ── Accent and status ──
pub const ACCENT: Color32 = theme::ACCENT;
/// Filled controls: the primary button, a selected day.
pub const ACCENT_STRONG: Color32 = rgb(14, 140, 200);
pub const ACCENT_SOFT: Color32 = rgb(12, 42, 58);
pub const SUCCESS: Color32 = rgb(74, 210, 125);
pub const SUCCESS_SOFT: Color32 = rgb(16, 44, 30);
pub const WARNING: Color32 = rgb(240, 182, 60);
pub const WARNING_SOFT: Color32 = rgb(50, 38, 12);
pub const ERROR: Color32 = rgb(240, 104, 104);
pub const ERROR_SOFT: Color32 = rgb(54, 22, 24);
pub const DANGER: Color32 = rgb(176, 52, 52);

// ── Geometry ──
pub const RADIUS: u8 = 3;
pub const CARD_RADIUS: u8 = 4;
/// Between panes, and between the cards in one.
pub const GAP: f32 = 12.0;
/// A card's inner margin.
pub const PAD: i8 = 14;

// ── Type ──
pub const BODY: f32 = 13.5;
pub const SMALL: f32 = 11.5;
pub const MONO: f32 = 12.0;

/// The font family for titles, tabs and buttons: a medium weight when the
/// system has one, the regular face otherwise. egui has no weights of its
/// own, so this is a family of its own.
pub const MEDIUM: &str = "medium";

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MEDIUM.into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// System faces tried in order, per role. The installer bundles none, and an
/// immutable install usually has Noto from its desktop.
const REGULAR_FACES: &[&str] = &[
    "/usr/share/fonts/inter/Inter-Regular.otf",
    "/usr/share/fonts/TTF/Inter-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
];
const MEDIUM_FACES: &[&str] = &[
    "/usr/share/fonts/inter/Inter-Medium.otf",
    "/usr/share/fonts/TTF/Inter-Medium.ttf",
    "/usr/share/fonts/noto/NotoSans-Medium.ttf",
    "/usr/share/fonts/noto/NotoSans-SemiBold.ttf",
];
const MONO_FACES: &[&str] = &[
    "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
];

/// Apply the updater's style to `ctx`.
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_visuals(visuals());

    let mut style = (*ctx.style()).clone();
    use egui::TextStyle;
    style.text_styles = [
        (TextStyle::Heading, medium(17.0)),
        (TextStyle::Body, FontId::new(BODY, FontFamily::Proportional)),
        (TextStyle::Button, medium(BODY)),
        (
            TextStyle::Small,
            FontId::new(SMALL, FontFamily::Proportional),
        ),
        (TextStyle::Monospace, mono(MONO)),
    ]
    .into();

    let spacing = &mut style.spacing;
    spacing.item_spacing = egui::vec2(8.0, 6.0);
    // Still comfortably above egui's defaults: the updater runs on handhelds
    // too, driven by a thumb or a trackpad.
    spacing.button_padding = egui::vec2(12.0, 5.0);
    spacing.interact_size = egui::vec2(40.0, 26.0);
    spacing.icon_width = 16.0;
    spacing.icon_width_inner = 9.0;
    spacing.icon_spacing = 6.0;
    spacing.scroll = egui::style::ScrollStyle::solid();
    spacing.scroll.bar_width = 8.0;
    spacing.scroll.bar_inner_margin = 2.0;
    spacing.window_margin = egui::Margin::same(18);
    ctx.set_style(style);
}

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let mut load = |key: &str, faces: &[&str]| -> Option<String> {
        let bytes = faces.iter().find_map(|path| std::fs::read(path).ok())?;
        fonts
            .font_data
            .insert(key.to_string(), Arc::new(FontData::from_owned(bytes)));
        Some(key.to_string())
    };
    let regular = load("updater-regular", REGULAR_FACES);
    let medium = load("updater-medium", MEDIUM_FACES);
    let monospace = load("updater-mono", MONO_FACES);

    // Each found face goes in front of egui's own, which stay behind it as the
    // fallback for anything the face lacks (emoji, symbols).
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    if let Some(name) = &regular {
        proportional.insert(0, name.clone());
    }
    let mut medium_chain = proportional.clone();
    if let Some(name) = medium {
        medium_chain.insert(0, name);
    }
    fonts
        .families
        .insert(FontFamily::Name(MEDIUM.into()), medium_chain);
    if let Some(name) = &monospace {
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .insert(0, name.clone());
    }
    // The monospace face last in the text chains: Noto Sans has no arrows,
    // and its mono sibling (or egui's Hack) does.
    let fallback = monospace.unwrap_or_else(|| "Hack".to_string());
    for family in [FontFamily::Proportional, FontFamily::Name(MEDIUM.into())] {
        if let Some(chain) = fonts.families.get_mut(&family) {
            if fonts.font_data.contains_key(&fallback) && !chain.contains(&fallback) {
                chain.push(fallback.clone());
            }
        }
    }
    fonts
}

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    let radius = CornerRadius::same(RADIUS);
    let hairline = |c| Stroke::new(1.0_f32, c);

    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = hairline(BORDER_STRONG);
    v.window_corner_radius = CornerRadius::same(CARD_RADIUS);
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(140),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.menu_corner_radius = radius;
    v.extreme_bg_color = INSET;
    v.faint_bg_color = CARD;
    v.code_bg_color = INSET;
    v.selection.bg_fill = ACCENT_SOFT;
    v.selection.stroke = hairline(ACCENT);
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = WARNING;
    v.error_fg_color = ERROR;
    v.text_cursor.stroke = Stroke::new(2.0_f32, ACCENT);
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    v.striped = false;

    let w = &mut v.widgets;
    // Labels and separators. Body copy is a step down from white; titles and
    // values ask for TEXT explicitly.
    w.noninteractive.bg_fill = CARD;
    w.noninteractive.weak_bg_fill = CARD;
    w.noninteractive.bg_stroke = hairline(BORDER);
    w.noninteractive.fg_stroke = hairline(TEXT_DIM);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = RAISED;
    w.inactive.weak_bg_fill = RAISED;
    w.inactive.bg_stroke = hairline(BORDER_STRONG);
    w.inactive.fg_stroke = hairline(TEXT);
    w.inactive.corner_radius = radius;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = HOVER;
    w.hovered.weak_bg_fill = HOVER;
    w.hovered.bg_stroke = hairline(rgb(84, 93, 110));
    w.hovered.fg_stroke = hairline(TEXT);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    // `strong()` text takes the active colour, so this is white, not accent.
    w.active.bg_fill = ACCENT_SOFT;
    w.active.weak_bg_fill = ACCENT_SOFT;
    w.active.bg_stroke = hairline(ACCENT);
    w.active.fg_stroke = hairline(TEXT);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open.bg_fill = HOVER;
    w.open.weak_bg_fill = HOVER;
    w.open.bg_stroke = hairline(ACCENT);
    w.open.fg_stroke = hairline(TEXT);
    w.open.corner_radius = radius;
    v
}
