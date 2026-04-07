//! Network and desktop configuration panel

use crate::config::{DesktopEnvironment, Filesystem, NetworkBackend};
use crate::gui::{state::PackagesState, theme, widgets};
use egui::Ui;

/// Render network & desktop sections.
pub(crate) fn show_sections(ui: &mut Ui, packages: &mut PackagesState, filesystem: &Filesystem) {
    widgets::section(ui, "Network", |ui| {
        ui.horizontal(|ui| {
            ui.label("Backend:");
            egui::ComboBox::from_id_salt("network")
                .selected_text(format!("{}", packages.network_backend))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut packages.network_backend,
                        NetworkBackend::Iwd,
                        "iwd (standalone)",
                    );
                    ui.selectable_value(
                        &mut packages.network_backend,
                        NetworkBackend::NetworkManager,
                        "NetworkManager + iwd",
                    );
                });
        });
    });

    widgets::section(ui, "Desktop Environment", |ui| {
        egui::ComboBox::from_id_salt("desktop")
            .selected_text(format!("{}", packages.desktop_env))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut packages.desktop_env,
                    DesktopEnvironment::None,
                    "None (headless/server)",
                );
                ui.selectable_value(
                    &mut packages.desktop_env,
                    DesktopEnvironment::Kde,
                    "KDE Plasma",
                );
                ui.selectable_value(
                    &mut packages.desktop_env,
                    DesktopEnvironment::Gnome,
                    "GNOME",
                );
                ui.selectable_value(&mut packages.desktop_env, DesktopEnvironment::Xfce, "XFCE");
            });
    });

    widgets::section(ui, "GPU Drivers", |ui| {
        ui.checkbox(
            &mut packages.gpu_nvidia,
            "NVIDIA (nvidia, nvidia-utils, linux-firmware-nvidia)",
        );
        ui.checkbox(
            &mut packages.gpu_amd,
            "AMD (mesa, vulkan-radeon, xf86-video-amdgpu)",
        );
        ui.checkbox(
            &mut packages.gpu_intel,
            "Intel (mesa, vulkan-intel, xf86-video-intel)",
        );
    });

    widgets::section(ui, "Optional Packages", |ui| {
        ui.checkbox(
            &mut packages.install_wine,
            "Wine compatibility (wine, vkd3d, winetricks, wine-mono, wine-gecko)",
        );
        ui.add_space(theme::SPACING_XS);

        ui.checkbox(
            &mut packages.install_yay,
            "yay AUR helper (built from source)",
        );
        if packages.install_yay {
            widgets::info_text(
                ui,
                "Go will be installed as a build dependency. yay is built as your user via makepkg.",
            );
        }
        ui.add_space(theme::SPACING_XS);

        if packages.install_yay && *filesystem == Filesystem::Btrfs {
            ui.checkbox(
                &mut packages.install_btrfs_tools,
                "Btrfs snapshot tools (snapper, btrfs-assistant) via yay",
            );
            ui.add_space(theme::SPACING_XS);
        } else {
            packages.install_btrfs_tools = false;
        }

        ui.checkbox(
            &mut packages.install_modular,
            "Modular mod manager (CLI + GUI for NexusMods, GameBanana)",
        );
        if packages.install_modular {
            widgets::info_text(
                ui,
                "Extensible mod manager with plugin support for downloading and organizing game mods.",
            );
        }
    });
}
