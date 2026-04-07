//! Disk selection panel

use crate::gui::{state::DiskState, theme, widgets};
use egui::{RichText, Ui};

/// Render disk selection sections. Returns `true` when a disk is selected.
pub(crate) fn show_sections(ui: &mut Ui, disk: &mut DiskState) -> bool {
    ui.label("Choose the disk where Artix Linux will be installed.");
    widgets::validation_warning(ui, "All data on the selected disk will be erased!");
    ui.add_space(theme::SPACING_MD);

    if ui.button("\u{1f504} Refresh Disks").clicked() {
        disk.refreshing = true;
    }
    ui.add_space(theme::SPACING_SM);

    widgets::section(ui, "Available Disks", |ui| {
        if disk.devices.is_empty() {
            ui.label(
                RichText::new(
                    "No suitable disks found. Ensure a disk is connected and not mounted.",
                )
                .color(theme::TEXT_MUTED),
            );
        } else {
            egui::ScrollArea::vertical()
                .max_height(280.0)
                .id_salt("disk_list_scroll")
                .show(ui, |ui| {
                    for (i, dev) in disk.devices.iter().enumerate() {
                        let is_selected = disk.selected_device_index == Some(i);
                        let text = format!(
                            "{} \u{2014} {} {} ({})",
                            dev.path,
                            dev.size_human(),
                            dev.model.as_deref().unwrap_or("Unknown"),
                            dev.device_type
                        );
                        if ui.selectable_label(is_selected, &text).clicked() {
                            disk.selected_device_index = Some(i);
                        }
                    }
                });
        }
    });

    disk.selected_device_index.is_some()
}
