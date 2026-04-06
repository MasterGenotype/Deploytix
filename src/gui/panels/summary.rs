//! Summary and review panel

use crate::gui::{
    state::{DiskState, InstallState, PackagesState, SystemState, UserState},
    theme, widgets,
};
use egui::{RichText, Ui};

/// Render the summary panel. Returns `true` when the user may proceed.
pub fn show(
    ui: &mut Ui,
    disk: &DiskState,
    system: &SystemState,
    user: &UserState,
    packages: &PackagesState,
    install: &mut InstallState,
) -> bool {
    widgets::page_heading(ui, "Review Configuration");

    egui::ScrollArea::vertical().show(ui, |ui| {
        // ── Summary table ──────────────────────────────────────────
        widgets::section(ui, "Configuration Summary", |ui| {
            let parts_str: String = disk
                .partitions
                .iter()
                .map(|p| {
                    if p.size_mib == 0 {
                        format!("{} (remainder)", p.mount_point)
                    } else {
                        format!("{} ({} GiB)", p.mount_point, p.size_mib / 1024)
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");

            let mut gpu_list = Vec::new();
            if packages.gpu_nvidia {
                gpu_list.push("NVIDIA");
            }
            if packages.gpu_amd {
                gpu_list.push("AMD");
            }
            if packages.gpu_intel {
                gpu_list.push("Intel");
            }
            let gpu_str = if gpu_list.is_empty() {
                "None".to_string()
            } else {
                gpu_list.join(", ")
            };

            egui::Grid::new("summary_grid")
                .num_columns(2)
                .spacing([20.0, 4.0])
                .show(ui, |ui| {
                    row(ui, "Target Disk", disk.selected_device_path());
                    row(ui, "Partitions", &parts_str);
                    row(ui, "Filesystem", &format!("{}", disk.filesystem));
                    row(
                        ui,
                        "Encryption",
                        if disk.encryption {
                            "Enabled"
                        } else {
                            "Disabled"
                        },
                    );
                    if disk.encryption {
                        row(
                            ui,
                            "Boot Encryption",
                            if disk.boot_encryption {
                                "Enabled (LUKS1)"
                            } else {
                                "Disabled"
                            },
                        );
                    }
                    row(
                        ui,
                        "Integrity",
                        if disk.integrity {
                            "Enabled (HMAC-SHA256)"
                        } else {
                            "Disabled"
                        },
                    );
                    row(ui, "Swap", &format!("{}", disk.swap_type));
                    row(ui, "Init System", &format!("{}", system.init_system));
                    row(ui, "Bootloader", &format!("{}", system.bootloader));
                    row(
                        ui,
                        "SecureBoot",
                        if system.secureboot {
                            "Enabled"
                        } else {
                            "Disabled"
                        },
                    );
                    row(ui, "Hostname", &system.hostname);
                    row(ui, "Username", &user.username);
                    row(ui, "Network", &format!("{}", packages.network_backend));
                    row(ui, "Desktop", &format!("{}", packages.desktop_env));
                    row(ui, "GPU Drivers", &gpu_str);
                    row(
                        ui,
                        "Wine",
                        if packages.install_wine {
                            "Enabled"
                        } else {
                            "Disabled"
                        },
                    );
                    row(
                        ui,
                        "Gaming",
                        if packages.install_gaming {
                            "Enabled"
                        } else {
                            "Disabled"
                        },
                    );
                    row(
                        ui,
                        "Session Switching",
                        if packages.install_session_switching {
                            "Enabled"
                        } else {
                            "Disabled"
                        },
                    );
                    row(
                        ui,
                        "yay AUR Helper",
                        if packages.install_yay {
                            "Enabled"
                        } else {
                            "Disabled"
                        },
                    );
                    row(
                        ui,
                        "Btrfs Tools",
                        if packages.install_btrfs_tools {
                            "Enabled (snapper, btrfs-assistant)"
                        } else {
                            "Disabled"
                        },
                    );
                    row(
                        ui,
                        "Modular Mod Manager",
                        if packages.install_modular {
                            "Enabled"
                        } else {
                            "Disabled"
                        },
                    );
                });
        });

        // ── Save configuration ─────────────────────────────────────
        widgets::section(ui, "Save Configuration", |ui| {
            ui.horizontal(|ui| {
                ui.label("Path:");
                ui.add(
                    egui::TextEdit::singleline(&mut install.save_config_path).desired_width(250.0),
                );
                if ui.button("\u{1f4be} Save Config").clicked() {
                    install.save_requested = true;
                }
            });
            if let Some((msg, is_error)) = &install.save_config_status {
                let color = if *is_error {
                    theme::ERROR
                } else {
                    theme::SUCCESS
                };
                ui.label(RichText::new(msg).color(color));
            }
        });

        // ── Install options ────────────────────────────────────────
        widgets::section(ui, "Install Options", |ui| {
            ui.checkbox(
                &mut install.dry_run,
                "Dry run mode (preview only, no changes)",
            );
            ui.add_space(theme::SPACING_SM);

            ui.label(
                RichText::new("\u{26a0} WARNING: This will ERASE ALL DATA on the selected disk!")
                    .color(theme::ERROR)
                    .strong(),
            );
            ui.add_space(theme::SPACING_XS);
            ui.checkbox(&mut install.confirmed, "I understand and want to proceed");
        });
    });

    install.confirmed || install.dry_run
}

fn row(ui: &mut Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).color(theme::TEXT_SECONDARY));
    ui.label(RichText::new(value).strong());
    ui.end_row();
}
