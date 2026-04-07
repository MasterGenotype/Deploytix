//! Handheld gaming device configuration panel

use crate::config::DesktopEnvironment;
use crate::gui::{state::PackagesState, theme, widgets};
use egui::Ui;

/// Render handheld gaming sections.
pub(crate) fn show_sections(ui: &mut Ui, packages: &mut PackagesState) {
    widgets::validation_warning(
        ui,
        "This section is for handheld gaming devices (e.g. Steam Deck, GPD, AYANEO). \
         These features may not work correctly on non-gaming hardware.",
    );
    ui.add_space(theme::SPACING_MD);

    widgets::section(ui, "Gaming", |ui| {
        ui.checkbox(
            &mut packages.install_gaming,
            "Gaming packages (Steam, gamescope)",
        );

        if packages.install_gaming && packages.desktop_env != DesktopEnvironment::None {
            ui.add_space(theme::SPACING_XS);
            ui.indent("session_switching_indent", |ui| {
                ui.checkbox(
                    &mut packages.install_session_switching,
                    "Game Mode \u{2194} Desktop switching",
                );
                if packages.install_session_switching {
                    widgets::info_text(
                        ui,
                        "Boots into gamescope Game Mode; switch to desktop and back on demand.",
                    );
                }
            });
        } else {
            packages.install_session_switching = false;
        }
    });

    ui.add_space(theme::SPACING_MD);

    widgets::section(ui, "Handheld Services", |ui| {
        // ── sysctl gaming tweaks (no prerequisites) ──────────────────────
        ui.checkbox(
            &mut packages.sysctl_gaming_tweaks,
            "Gaming sysctl tweaks (vm.max_map_count, swappiness\u{2026})",
        );
        widgets::info_text(
            ui,
            "Writes /etc/sysctl.d/99-gaming.conf. Required for many Proton/WINE titles.",
        );

        ui.add_space(theme::SPACING_XS);

        // ── Handheld Daemon (requires yay) ────────────────────────────────
        let hhd_available = packages.install_yay;
        ui.add_enabled_ui(hhd_available, |ui| {
            ui.checkbox(
                &mut packages.install_hhd,
                "Handheld Daemon (HHD) \u{2014} gamepad remapping, TDP, per-game profiles",
            );
        });
        if !hhd_available {
            packages.install_hhd = false;
            widgets::info_text(
                ui,
                "Requires: yay AUR helper (enable in Optional Packages above).",
            );
        } else if packages.install_hhd {
            widgets::info_text(
                ui,
                "Installs hhd, adjustor, hhd-ui from the AUR. \
                 Writes an init-specific service file (runit/s6/dinit/openrc).",
            );
        }

        ui.add_space(theme::SPACING_XS);

        // ── Decky Loader (requires gaming packages / Steam) ───────────────
        let decky_available = packages.install_gaming;
        ui.add_enabled_ui(decky_available, |ui| {
            ui.checkbox(
                &mut packages.install_decky_loader,
                "Decky Loader \u{2014} Steam plugin framework",
            );
        });
        if !decky_available {
            packages.install_decky_loader = false;
            widgets::info_text(
                ui,
                "Requires: Gaming packages (Steam) \u{2014} enable the checkbox above.",
            );
        } else if packages.install_decky_loader {
            widgets::info_text(
                ui,
                "Downloads the PluginLoader binary from GitHub at install time. \
                 Requires internet access. Writes an init-specific service file.",
            );
        }
    });
}
