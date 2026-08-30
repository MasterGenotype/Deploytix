//! System configuration panel

use crate::config::{InitSystem, SecureBootMethod};
use crate::gui::{state::SystemState, theme, widgets};
use egui::{RichText, Ui};

/// Render system configuration sections. Returns `true` when valid.
pub(crate) fn show_sections(ui: &mut Ui, system: &mut SystemState) -> bool {
    widgets::section(ui, "Init & Bootloader", |ui| {
        widgets::combo_row(
            ui,
            "Init System:",
            "init",
            format!("{}", system.init_system),
            |ui| {
                ui.selectable_value(&mut system.init_system, InitSystem::Runit, "runit");
                ui.selectable_value(&mut system.init_system, InitSystem::OpenRC, "openrc");
                ui.selectable_value(&mut system.init_system, InitSystem::S6, "s6");
                ui.selectable_value(&mut system.init_system, InitSystem::Dinit, "dinit");
            },
        );
        ui.add_space(theme::SPACING_XS);

        ui.horizontal_wrapped(|ui| {
            ui.label("Bootloader:");
            ui.label(RichText::new(format!("{}", system.bootloader)).color(theme::TEXT_SECONDARY));
        });
    });

    widgets::section(ui, "SecureBoot", |ui| {
        ui.checkbox(&mut system.secureboot, "Enable SecureBoot signing");
        if system.secureboot {
            ui.add_space(theme::SPACING_XS);
            widgets::combo_row(
                ui,
                "Method:",
                "secureboot_method",
                format!("{}", system.secureboot_method),
                |ui| {
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
                },
            );
        }
    });

    widgets::section(ui, "Locale & Identity", |ui| {
        widgets::text_row(ui, "Timezone:", &mut system.timezone);
        ui.add_space(theme::SPACING_XS);

        widgets::text_row(ui, "Locale:", &mut system.locale);
        ui.add_space(theme::SPACING_XS);

        widgets::text_row(ui, "Keymap:", &mut system.keymap);
        ui.add_space(theme::SPACING_XS);

        widgets::text_row(ui, "Hostname:", &mut system.hostname);
    });

    // Validation
    if system.hostname.is_empty() {
        widgets::validation_error(ui, "Hostname cannot be empty");
        return false;
    }

    true
}
