//! System configuration panel

use crate::config::{InitSystem, SecureBootMethod, SwapType};
use crate::gui::{state::SystemState, theme, widgets};
use egui::{RichText, Ui};

/// Render system configuration sections. Returns `true` when valid.
///
/// `swap_type` is read-only, and comes from the disk panel: hibernation needs
/// somewhere to put the image, so the choice made there decides whether this
/// panel can offer it at all.
pub(crate) fn show_sections(ui: &mut Ui, system: &mut SystemState, swap_type: &SwapType) -> bool {
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

    widgets::section(ui, "Power", |ui| {
        // ZRAM is RAM-backed: nothing survives the power-off, so there is no
        // image to resume from. Forced off rather than merely hidden, so a
        // config loaded with both set cannot reach validation and fail there.
        if *swap_type == SwapType::ZramOnly {
            system.hibernation = false;
            ui.add_enabled_ui(false, |ui| {
                ui.checkbox(&mut system.hibernation, "Hibernation (suspend to disk)");
            });
            widgets::info_text(
                ui,
                "Unavailable with ZRAM-only swap: ZRAM lives in RAM, so there is \
                 no hibernation image left after power-off. Choose a swap \
                 partition or a swap file.",
            );
        } else {
            ui.checkbox(&mut system.hibernation, "Hibernation (suspend to disk)");
            if system.hibernation {
                widgets::info_text(
                    ui,
                    "Adds the resume hook and resume=/resume_offset= to the kernel \
                     cmdline. Works on an immutable root: swap stays outside LUKS \
                     and LVM, and a swap file is placed on /var so it survives \
                     snapshot sets.",
                );
            }
        }
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
