//! System configuration panel

use crate::config::{InitSystem, SecureBootMethod};
use crate::gui::{state::SystemState, theme, widgets};
use egui::{RichText, Ui};

/// Render system configuration sections. Returns `true` when valid.
pub(crate) fn show_sections(ui: &mut Ui, system: &mut SystemState) -> bool {
    widgets::section(ui, "Init & Bootloader", |ui| {
        ui.horizontal(|ui| {
            ui.label("Init System:");
            egui::ComboBox::from_id_salt("init")
                .selected_text(format!("{}", system.init_system))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut system.init_system, InitSystem::Runit, "runit");
                    ui.selectable_value(&mut system.init_system, InitSystem::OpenRC, "openrc");
                    ui.selectable_value(&mut system.init_system, InitSystem::S6, "s6");
                    ui.selectable_value(&mut system.init_system, InitSystem::Dinit, "dinit");
                });
        });
        ui.add_space(theme::SPACING_XS);

        ui.horizontal(|ui| {
            ui.label("Bootloader:");
            ui.label(RichText::new(format!("{}", system.bootloader)).color(theme::TEXT_SECONDARY));
        });
    });

    widgets::section(ui, "SecureBoot", |ui| {
        ui.checkbox(&mut system.secureboot, "Enable SecureBoot signing");
        if system.secureboot {
            ui.add_space(theme::SPACING_XS);
            ui.horizontal(|ui| {
                ui.label("Method:");
                egui::ComboBox::from_id_salt("secureboot_method")
                    .selected_text(format!("{}", system.secureboot_method))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut system.secureboot_method,
                            SecureBootMethod::Sbctl,
                            "sbctl (automatic key management)",
                        );
                        ui.selectable_value(
                            &mut system.secureboot_method,
                            SecureBootMethod::Shim,
                            "Shim (MOK enrollment)",
                        );
                        ui.selectable_value(
                            &mut system.secureboot_method,
                            SecureBootMethod::ManualKeys,
                            "Manual Keys (provide your own)",
                        );
                    });
            });
        }
    });

    widgets::section(ui, "Locale & Identity", |ui| {
        ui.horizontal(|ui| {
            ui.label("Timezone:");
            ui.text_edit_singleline(&mut system.timezone);
        });
        ui.add_space(theme::SPACING_XS);

        ui.horizontal(|ui| {
            ui.label("Locale:");
            ui.text_edit_singleline(&mut system.locale);
        });
        ui.add_space(theme::SPACING_XS);

        ui.horizontal(|ui| {
            ui.label("Keymap:");
            ui.text_edit_singleline(&mut system.keymap);
        });
        ui.add_space(theme::SPACING_XS);

        ui.horizontal(|ui| {
            ui.label("Hostname:");
            ui.text_edit_singleline(&mut system.hostname);
        });
    });

    // Validation
    if system.hostname.is_empty() {
        widgets::validation_error(ui, "Hostname cannot be empty");
        return false;
    }

    true
}
