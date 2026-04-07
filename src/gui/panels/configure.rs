//! Unified configuration panel
//!
//! Displays all pre-install options in a 3-column grid layout that fits
//! on screen without scrolling (at 75% zoom).

use crate::gui::{
    state::{DiskState, PackagesState, SystemState, UserState},
    theme,
};
use egui::{RichText, Ui};

use super::{
    disk_config, disk_selection, handheld_gaming, network_desktop, system_config, user_config,
};

/// Column heading rendered at the top of each grid column.
fn column_heading(ui: &mut Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .strong()
            .size(16.0)
            .color(theme::ACCENT),
    );
    ui.separator();
    ui.add_space(theme::SPACING_XS);
}

/// Sub-heading used to separate logical groups within a column.
fn sub_heading(ui: &mut Ui, title: &str) {
    ui.add_space(theme::SPACING_SM);
    ui.label(
        RichText::new(title)
            .strong()
            .size(14.0)
            .color(theme::ACCENT),
    );
    ui.separator();
    ui.add_space(theme::SPACING_XS);
}

/// Render the unified configuration panel as a 3-column grid.
///
/// Returns `true` when all sections pass validation and the user may
/// proceed to the review step.
pub fn show(
    ui: &mut Ui,
    disk: &mut DiskState,
    system: &mut SystemState,
    user: &mut UserState,
    packages: &mut PackagesState,
) -> bool {
    let mut disk_selected = false;
    let mut disk_valid = false;
    let mut system_valid = false;
    let mut user_valid = false;

    ui.columns(3, |cols| {
        // ═══ Column 1: Disk ═══════════════════════════════════════════
        column_heading(&mut cols[0], "Disk");
        disk_selected = disk_selection::show_sections(&mut cols[0], disk);
        cols[0].add_space(theme::SPACING_SM);
        disk_valid = disk_config::show_sections(&mut cols[0], disk);

        // ═══ Column 2: System & User ══════════════════════════════════
        column_heading(&mut cols[1], "System");
        system_valid = system_config::show_sections(&mut cols[1], system);
        sub_heading(&mut cols[1], "User Account");
        user_valid = user_config::show_sections(&mut cols[1], user);

        // ═══ Column 3: Packages & Gaming ══════════════════════════════
        column_heading(&mut cols[2], "Packages");
        network_desktop::show_sections(&mut cols[2], packages, &disk.filesystem);
        sub_heading(&mut cols[2], "Gaming");
        handheld_gaming::show_sections(&mut cols[2], packages);
    });

    disk_selected && disk_valid && system_valid && user_valid
}
