//! Network and desktop configuration panel

use crate::config::{DesktopEnvironment, Filesystem, IwdFrontend, NetworkBackend};
use crate::gui::{state::PackagesState, theme, widgets};
use egui::Ui;

/// Render network & desktop sections.
pub(crate) fn show_sections(ui: &mut Ui, packages: &mut PackagesState, filesystem: &Filesystem) {
    widgets::section(ui, "Network", |ui| {
        // Steam's gamepad UI configures Wi-Fi through NetworkManager; the
        // standalone iwd backend would leave Game Mode network setup broken
        // (and fail validation), so coerce it while session switching is on.
        if packages.install_session_switching && packages.network_backend == NetworkBackend::Iwd {
            packages.network_backend = NetworkBackend::NetworkManager;
        }
        ui.horizontal(|ui| {
            ui.label("Backend:");
            egui::ComboBox::from_id_salt("network")
                .selected_text(format!("{}", packages.network_backend))
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut packages.network_backend,
                        NetworkBackend::Iwd,
                        "iwd + GUI frontend (AUR)",
                    );
                    ui.selectable_value(
                        &mut packages.network_backend,
                        NetworkBackend::NetworkManager,
                        "NetworkManager + iwd",
                    );
                    ui.selectable_value(
                        &mut packages.network_backend,
                        NetworkBackend::NetworkManagerWpa,
                        "NetworkManager + wpa_supplicant",
                    );
                });
        });
        if packages.install_session_switching {
            widgets::info_text(
                ui,
                "Game Mode session switching requires a NetworkManager backend \
                 (Steam's gamepad UI configures Wi-Fi through NetworkManager).",
            );
        }

        // Sub-choice: iwd GUI frontend (AUR) only when standalone iwd is picked.
        if packages.network_backend == NetworkBackend::Iwd {
            ui.add_space(theme::SPACING_XS);
            ui.horizontal(|ui| {
                ui.label("Frontend:");
                egui::ComboBox::from_id_salt("iwd_frontend")
                    .selected_text(format!("{}", packages.iwd_frontend))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut packages.iwd_frontend,
                            IwdFrontend::Iwgtk,
                            "iwgtk (GTK)",
                        );
                        ui.selectable_value(
                            &mut packages.iwd_frontend,
                            IwdFrontend::Iwdgui,
                            "iwdgui (GTK)",
                        );
                        ui.selectable_value(
                            &mut packages.iwd_frontend,
                            IwdFrontend::Iwqt,
                            "iwqt (Qt)",
                        );
                    });
            });
            if !packages.install_yay {
                widgets::info_text(
                    ui,
                    "Requires: yay AUR helper (enable in Optional Packages below). \
                     Validation will fail without it.",
                );
            }
        }

        ui.add_space(theme::SPACING_XS);

        // Optional Wi-Fi pre-seeding — gives the installed system connectivity
        // from the very first boot (Steam's first-run bootstrap in Game Mode
        // needs network before its own OOBE network page exists).
        ui.label("Pre-seed Wi-Fi network (optional):");
        ui.horizontal(|ui| {
            ui.label("SSID:");
            ui.text_edit_singleline(&mut packages.wifi_ssid);
        });
        if !packages.wifi_ssid.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Passphrase:");
                ui.add(egui::TextEdit::singleline(&mut packages.wifi_password).password(true));
            });
            widgets::info_text(
                ui,
                "Credentials are written to the installed system so it auto-connects \
                 on first boot. Leave the passphrase empty for an open network.",
            );
        }

        ui.add_space(theme::SPACING_XS);

        ui.checkbox(
            &mut packages.sysctl_network_performance,
            "Network performance sysctl tweaks (BBR + fq, larger buffers, ECN\u{2026})",
        );
        if packages.sysctl_network_performance {
            widgets::info_text(
                ui,
                "Writes /etc/sysctl.d/99-network-performance.conf. Switches TCP \
                 congestion control to BBR and raises socket buffer ceilings for \
                 Wi-Fi 6 / 1\u{00a0}GbE+ links.",
            );
        }
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
    });
}
