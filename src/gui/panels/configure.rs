//! Unified configuration panel
//!
//! Consolidates all pre-install options into a single scrollable view
//! with category-based sections.

use crate::gui::{
    state::{DiskState, PackagesState, SystemState, UserState},
    theme,
};
use egui::{RichText, Ui};

use super::{
    disk_config, disk_selection, handheld_gaming, network_desktop, system_config, user_config,
};

/// Render a category heading — larger than section titles, with a separator.
fn category_heading(ui: &mut Ui, title: &str) {
    ui.add_space(theme::SPACING_MD);
    ui.separator();
    ui.add_space(theme::SPACING_SM);
    ui.label(
        RichText::new(title)
            .strong()
            .size(18.0)
            .color(theme::ACCENT),
    );
    ui.add_space(theme::SPACING_SM);
}

/// Render the unified configuration panel.
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
    let output = egui::ScrollArea::vertical().show(ui, |ui| {
        // ═══════════════════════════════════════════════════════════════
        //  TARGET DISK
        // ═══════════════════════════════════════════════════════════════
        category_heading(ui, "Target Disk");
        let disk_selected = disk_selection::show_sections(ui, disk);

        // ═══════════════════════════════════════════════════════════════
        //  DISK CONFIGURATION
        // ═══════════════════════════════════════════════════════════════
        category_heading(ui, "Disk Configuration");
        let disk_valid = disk_config::show_sections(ui, disk);

        // ═══════════════════════════════════════════════════════════════
        //  SYSTEM
        // ═══════════════════════════════════════════════════════════════
        category_heading(ui, "System");
        let system_valid = system_config::show_sections(ui, system);

        // ═══════════════════════════════════════════════════════════════
        //  USER ACCOUNT
        // ═══════════════════════════════════════════════════════════════
        category_heading(ui, "User Account");
        let user_valid = user_config::show_sections(ui, user);

        // ═══════════════════════════════════════════════════════════════
        //  NETWORK & DESKTOP
        // ═══════════════════════════════════════════════════════════════
        category_heading(ui, "Network & Desktop");
        network_desktop::show_sections(ui, packages, &disk.filesystem);

        // ═══════════════════════════════════════════════════════════════
        //  HANDHELD GAMING
        // ═══════════════════════════════════════════════════════════════
        category_heading(ui, "Handheld Gaming");
        handheld_gaming::show_sections(ui, packages);

        // Bottom padding so the last section isn't flush with the edge
        ui.add_space(theme::SPACING_MD);

        disk_selected && disk_valid && system_valid && user_valid
    });

    output.inner
}
