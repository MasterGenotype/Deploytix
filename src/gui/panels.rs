//! Panel components for the GUI wizard

use crate::config::{
    Bootloader, DesktopEnvironment, Filesystem, InitSystem, NetworkBackend, PartitionLayout,
    SecureBootMethod, SwapType,
};
use crate::disk::detection::BlockDevice;
use egui::{RichText, Ui};

/// Disk selection panel
pub fn disk_selection_panel(
    ui: &mut Ui,
    devices: &[BlockDevice],
    selected_index: &mut Option<usize>,
    refreshing: &mut bool,
) -> bool {
    let mut can_proceed = false;

    ui.heading("Select Target Disk");
    ui.add_space(8.0);
    ui.label("Choose the disk where Artix Linux will be installed.");
    ui.label(
        RichText::new("⚠ All data on the selected disk will be erased!")
            .color(egui::Color32::YELLOW),
    );
    ui.add_space(16.0);

    if ui.button("🔄 Refresh Disks").clicked() {
        *refreshing = true;
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(8.0);

    if devices.is_empty() {
        ui.label(
            "No suitable disks found. Make sure you have connected a disk and it's not mounted.",
        );
    } else {
        egui::ScrollArea::vertical()
            .max_height(300.0)
            .show(ui, |ui| {
                for (i, dev) in devices.iter().enumerate() {
                    let is_selected = *selected_index == Some(i);
                    let text = format!(
                        "{} - {} {} ({})",
                        dev.path,
                        dev.size_human(),
                        dev.model.as_deref().unwrap_or("Unknown"),
                        dev.device_type
                    );

                    if ui.selectable_label(is_selected, &text).clicked() {
                        *selected_index = Some(i);
                    }
                }
            });

        if selected_index.is_some() {
            can_proceed = true;
        }
    }

    can_proceed
}

/// Disk configuration panel
#[allow(clippy::too_many_arguments)]
pub fn disk_config_panel(
    ui: &mut Ui,
    layout: &mut PartitionLayout,
    filesystem: &mut Filesystem,
    encryption: &mut bool,
    encryption_password: &mut String,
    boot_encryption: &mut bool,
    integrity: &mut bool,
    swap_type: &mut SwapType,
    zram_percent: &mut u8,
    lvm_vg_name: &mut String,
    lvm_thin_pool_name: &mut String,
    lvm_thin_pool_percent: &mut u8,
) -> bool {
    ui.heading("Disk Configuration");
    ui.add_space(8.0);

    // Partition Layout
    let prev_layout = layout.clone();
    ui.label("Partition Layout:");
    egui::ComboBox::from_id_salt("layout")
        .selected_text(format!("{}", layout))
        .show_ui(ui, |ui| {
            ui.selectable_value(
                layout,
                PartitionLayout::Standard,
                "Standard (EFI, Boot, Swap, Root, Usr, Var, Home)",
            );
            ui.selectable_value(
                layout,
                PartitionLayout::Minimal,
                "Minimal (EFI, Boot, Swap, Root with subvolumes)",
            );
            ui.selectable_value(
                layout,
                PartitionLayout::LvmThin,
                "LVM Thin (EFI, Boot, LUKS + LVM thin provisioning)",
            );
        });
    ui.add_space(8.0);

    // LvmThin requires encryption and btrfs — enforce automatically
    if *layout == PartitionLayout::LvmThin {
        *encryption = true;
        *filesystem = Filesystem::Btrfs;
    }

    // When switching away from LvmThin, clear the encryption state that was
    // force-enabled by LvmThin so it doesn't bleed into Standard/Minimal.
    if prev_layout == PartitionLayout::LvmThin && *layout != PartitionLayout::LvmThin {
        *encryption = false;
    }

    // Filesystem
    ui.label("Filesystem:");
    if *layout == PartitionLayout::LvmThin {
        // LvmThin requires btrfs, show as read-only
        ui.label(
            RichText::new("btrfs (required by LVM Thin layout)")
                .color(egui::Color32::LIGHT_GRAY),
        );
    } else {
        egui::ComboBox::from_id_salt("filesystem")
            .selected_text(format!("{}", filesystem))
            .show_ui(ui, |ui| {
                ui.selectable_value(filesystem, Filesystem::Btrfs, "btrfs");
                ui.selectable_value(filesystem, Filesystem::Ext4, "ext4");
                ui.selectable_value(filesystem, Filesystem::Xfs, "xfs");
                ui.selectable_value(filesystem, Filesystem::F2fs, "f2fs");
            });
    }
    ui.add_space(8.0);

    // Swap Configuration
    ui.label("Swap Type:");
    egui::ComboBox::from_id_salt("swap_type")
        .selected_text(format!("{}", swap_type))
        .show_ui(ui, |ui| {
            ui.selectable_value(swap_type, SwapType::Partition, "Swap Partition");
            ui.selectable_value(swap_type, SwapType::FileZram, "Swap File + ZRAM");
            ui.selectable_value(swap_type, SwapType::ZramOnly, "ZRAM Only");
        });
    ui.add_space(8.0);

    // ZRAM percentage (shown for ZRAM options)
    if *swap_type == SwapType::FileZram || *swap_type == SwapType::ZramOnly {
        ui.horizontal(|ui| {
            ui.label("ZRAM Size (% of RAM):");
            ui.add(egui::Slider::new(zram_percent, 10..=100).suffix("%"));
        });
        ui.add_space(8.0);
    }

    // Encryption
    if *layout == PartitionLayout::LvmThin {
        // LvmThin always requires encryption — show as informational
        ui.label(
            RichText::new("LUKS encryption: enabled (required by LVM Thin layout)")
                .color(egui::Color32::LIGHT_GRAY),
        );
        ui.add_space(8.0);
    } else if *layout == PartitionLayout::Standard {
        ui.checkbox(encryption, "Enable LUKS encryption on data partitions");
        ui.add_space(8.0);
    } else {
        *encryption = false;
    }

    // Encryption password and integrity options
    if *encryption {
        ui.label("Encryption Password:");
        ui.add(egui::TextEdit::singleline(encryption_password).password(true));
        ui.add_space(8.0);

        ui.checkbox(integrity, "Enable dm-integrity (per-sector HMAC-SHA256 integrity)");
        if *integrity {
            ui.label(
                RichText::new("Detects silent data corruption. Disables TRIM/discard support.")
                    .weak(),
            );
        }
        ui.add_space(8.0);

        // Boot encryption uses LUKS1 (integrity is automatically disabled for boot)
        // Available for Standard and LvmThin layouts
        if *layout == PartitionLayout::Standard || *layout == PartitionLayout::LvmThin {
            ui.checkbox(boot_encryption, "Encrypt /boot partition (LUKS1)");
            if *integrity && *boot_encryption {
                ui.label(
                    RichText::new("Note: /boot uses LUKS1 without integrity (LUKS1 does not support dm-integrity)")
                        .weak(),
                );
            }
            ui.add_space(8.0);
        } else {
            *boot_encryption = false;
        }
    } else {
        *boot_encryption = false;
        *integrity = false;
    }

    // LVM Thin Provisioning settings
    if *layout == PartitionLayout::LvmThin {
        ui.separator();
        ui.add_space(8.0);
        ui.label(RichText::new("LVM Thin Provisioning Settings").strong());
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("Volume Group Name:");
            ui.text_edit_singleline(lvm_vg_name);
        });
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("Thin Pool Name:");
            ui.text_edit_singleline(lvm_thin_pool_name);
        });
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label("Thin Pool Size (% of VG):");
            ui.add(egui::Slider::new(lvm_thin_pool_percent, 50..=100).suffix("%"));
        });
        ui.add_space(4.0);

        ui.label(
            RichText::new("Thin volumes: root (50G), usr (50G), var (30G), home (200G)")
                .weak(),
        );
        ui.add_space(8.0);
    }

    // Validation
    if *encryption && encryption_password.is_empty() {
        ui.label(RichText::new("⚠ Please enter an encryption password").color(egui::Color32::RED));
        return false;
    }

    if *layout == PartitionLayout::LvmThin && lvm_vg_name.is_empty() {
        ui.label(
            RichText::new("⚠ Volume group name cannot be empty").color(egui::Color32::RED),
        );
        return false;
    }

    if *layout == PartitionLayout::LvmThin && lvm_thin_pool_name.is_empty() {
        ui.label(
            RichText::new("⚠ Thin pool name cannot be empty").color(egui::Color32::RED),
        );
        return false;
    }

    true
}

/// System configuration panel
#[allow(clippy::too_many_arguments)]
pub fn system_config_panel(
    ui: &mut Ui,
    init: &mut InitSystem,
    bootloader: &mut Bootloader,
    timezone: &mut String,
    locale: &mut String,
    keymap: &mut String,
    hostname: &mut String,
    secureboot: &mut bool,
    secureboot_method: &mut SecureBootMethod,
) -> bool {
    ui.heading("System Configuration");
    ui.add_space(8.0);

    // Init System
    ui.label("Init System:");
    egui::ComboBox::from_id_salt("init")
        .selected_text(format!("{}", init))
        .show_ui(ui, |ui| {
            ui.selectable_value(init, InitSystem::Runit, "runit");
            ui.selectable_value(init, InitSystem::OpenRC, "openrc");
            ui.selectable_value(init, InitSystem::S6, "s6");
            ui.selectable_value(init, InitSystem::Dinit, "dinit");
        });
    ui.add_space(8.0);

    // Bootloader
    ui.label("Bootloader:");
    egui::ComboBox::from_id_salt("bootloader")
        .selected_text(format!("{}", bootloader))
        .show_ui(ui, |ui| {
            ui.selectable_value(bootloader, Bootloader::Grub, "GRUB");
            ui.selectable_value(bootloader, Bootloader::SystemdBoot, "systemd-boot");
        });
    ui.add_space(8.0);

    // SecureBoot
    ui.checkbox(secureboot, "Enable SecureBoot signing");
    if *secureboot {
        ui.add_space(4.0);
        ui.label("SecureBoot Method:");
        egui::ComboBox::from_id_salt("secureboot_method")
            .selected_text(format!("{}", secureboot_method))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    secureboot_method,
                    SecureBootMethod::Sbctl,
                    "sbctl (automatic key management)",
                );
                ui.selectable_value(
                    secureboot_method,
                    SecureBootMethod::Shim,
                    "Shim (MOK enrollment)",
                );
                ui.selectable_value(
                    secureboot_method,
                    SecureBootMethod::ManualKeys,
                    "Manual Keys (provide your own)",
                );
            });
    }
    ui.add_space(8.0);

    ui.separator();
    ui.add_space(8.0);

    // Locale settings
    ui.horizontal(|ui| {
        ui.label("Timezone:");
        ui.text_edit_singleline(timezone);
    });
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.label("Locale:");
        ui.text_edit_singleline(locale);
    });
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.label("Keymap:");
        ui.text_edit_singleline(keymap);
    });
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.label("Hostname:");
        ui.text_edit_singleline(hostname);
    });
    ui.add_space(8.0);

    // Validation
    if hostname.is_empty() {
        ui.label(RichText::new("⚠ Hostname cannot be empty").color(egui::Color32::RED));
        return false;
    }

    true
}

/// User configuration panel
pub fn user_config_panel(
    ui: &mut Ui,
    username: &mut String,
    password: &mut String,
    password_confirm: &mut String,
    sudoer: &mut bool,
) -> bool {
    ui.heading("User Configuration");
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        ui.label("Username:");
        ui.text_edit_singleline(username);
    });
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.label("Password:");
        ui.add(egui::TextEdit::singleline(password).password(true));
    });
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.label("Confirm Password:");
        ui.add(egui::TextEdit::singleline(password_confirm).password(true));
    });
    ui.add_space(8.0);

    ui.checkbox(sudoer, "Add user to wheel group (sudo access)");
    ui.add_space(8.0);

    // Validation
    if username.is_empty() {
        ui.label(RichText::new("⚠ Username cannot be empty").color(egui::Color32::RED));
        return false;
    }
    if username.contains(' ') {
        ui.label(RichText::new("⚠ Username cannot contain spaces").color(egui::Color32::RED));
        return false;
    }
    if password.is_empty() {
        ui.label(RichText::new("⚠ Password cannot be empty").color(egui::Color32::RED));
        return false;
    }
    if password != password_confirm {
        ui.label(RichText::new("⚠ Passwords do not match").color(egui::Color32::RED));
        return false;
    }

    true
}

/// Network and desktop configuration panel
pub fn network_desktop_panel(
    ui: &mut Ui,
    network_backend: &mut NetworkBackend,
    desktop_env: &mut DesktopEnvironment,
) -> bool {
    ui.heading("Network & Desktop");
    ui.add_space(8.0);

    // Network Backend
    ui.label("Network Backend:");
    egui::ComboBox::from_id_salt("network")
        .selected_text(format!("{}", network_backend))
        .show_ui(ui, |ui| {
            ui.selectable_value(network_backend, NetworkBackend::Iwd, "iwd (standalone)");
            ui.selectable_value(
                network_backend,
                NetworkBackend::NetworkManager,
                "NetworkManager + iwd",
            );
        });
    ui.add_space(16.0);

    // Desktop Environment
    ui.label("Desktop Environment:");
    egui::ComboBox::from_id_salt("desktop")
        .selected_text(format!("{}", desktop_env))
        .show_ui(ui, |ui| {
            ui.selectable_value(
                desktop_env,
                DesktopEnvironment::None,
                "None (headless/server)",
            );
            ui.selectable_value(desktop_env, DesktopEnvironment::Kde, "KDE Plasma");
            ui.selectable_value(desktop_env, DesktopEnvironment::Gnome, "GNOME");
            ui.selectable_value(desktop_env, DesktopEnvironment::Xfce, "XFCE");
        });
    ui.add_space(8.0);

    true
}

/// Summary and install panel
#[allow(clippy::too_many_arguments)]
pub fn summary_panel(
    ui: &mut Ui,
    device_path: &str,
    layout: &PartitionLayout,
    filesystem: &Filesystem,
    encryption: bool,
    boot_encryption: bool,
    integrity: bool,
    swap_type: &SwapType,
    init: &InitSystem,
    bootloader: &Bootloader,
    secureboot: bool,
    hostname: &str,
    username: &str,
    network_backend: &NetworkBackend,
    desktop_env: &DesktopEnvironment,
    dry_run: &mut bool,
    confirmed: &mut bool,
) -> bool {
    ui.heading("Review Configuration");
    ui.add_space(8.0);

    egui::Grid::new("summary_grid")
        .num_columns(2)
        .spacing([20.0, 4.0])
        .show(ui, |ui| {
            ui.label("Target Disk:");
            ui.label(RichText::new(device_path).strong());
            ui.end_row();

            ui.label("Partition Layout:");
            ui.label(format!("{}", layout));
            ui.end_row();

            ui.label("Filesystem:");
            ui.label(format!("{}", filesystem));
            ui.end_row();

            ui.label("Encryption:");
            ui.label(if encryption { "Enabled" } else { "Disabled" });
            ui.end_row();

            if encryption {
                ui.label("Boot Encryption:");
                ui.label(if boot_encryption {
                    "Enabled (LUKS1)"
                } else {
                    "Disabled"
                });
                ui.end_row();
            }

            ui.label("Integrity:");
            ui.label(if integrity {
                "Enabled (HMAC-SHA256)"
            } else {
                "Disabled"
            });
            ui.end_row();

            ui.label("Swap:");
            ui.label(format!("{}", swap_type));
            ui.end_row();

            ui.label("Init System:");
            ui.label(format!("{}", init));
            ui.end_row();

            ui.label("Bootloader:");
            ui.label(format!("{}", bootloader));
            ui.end_row();

            ui.label("SecureBoot:");
            ui.label(if secureboot { "Enabled" } else { "Disabled" });
            ui.end_row();

            ui.label("Hostname:");
            ui.label(hostname);
            ui.end_row();

            ui.label("Username:");
            ui.label(username);
            ui.end_row();

            ui.label("Network:");
            ui.label(format!("{}", network_backend));
            ui.end_row();

            ui.label("Desktop:");
            ui.label(format!("{}", desktop_env));
            ui.end_row();
        });

    ui.add_space(16.0);
    ui.separator();
    ui.add_space(8.0);

    ui.checkbox(dry_run, "Dry run mode (preview only, no changes)");
    ui.add_space(8.0);

    ui.label(
        RichText::new("⚠ WARNING: This will ERASE ALL DATA on the selected disk!")
            .color(egui::Color32::RED)
            .strong(),
    );
    ui.add_space(4.0);
    ui.checkbox(confirmed, "I understand and want to proceed");
    ui.add_space(8.0);

    *confirmed || *dry_run
}

/// Installation progress panel
pub fn progress_panel(
    ui: &mut Ui,
    status: &str,
    progress: f32,
    log_messages: &[String],
    finished: bool,
    error: Option<&str>,
) {
    ui.heading(if finished {
        "Installation Complete"
    } else {
        "Installing..."
    });
    ui.add_space(8.0);

    if let Some(err) = error {
        ui.label(RichText::new(format!("❌ Error: {}", err)).color(egui::Color32::RED));
        ui.add_space(8.0);
    } else if finished {
        ui.label(
            RichText::new("✓ Installation completed successfully!")
                .color(egui::Color32::GREEN)
                .strong(),
        );
        ui.add_space(4.0);
        ui.label("You can now reboot into your new Artix Linux system.");
        ui.add_space(8.0);
    } else {
        ui.label(status);
        ui.add_space(8.0);
        ui.add(egui::ProgressBar::new(progress).show_percentage());
        ui.add_space(8.0);
    }

    ui.separator();
    ui.label("Log:");
    ui.add_space(4.0);

    // Use auto_shrink(false) and stick_to_bottom(true) for auto-scrolling logs
    let scroll_area = egui::ScrollArea::vertical()
        .max_height(300.0)
        .auto_shrink([false, false])
        .stick_to_bottom(true);

    scroll_area.show(ui, |ui| {
        for msg in log_messages {
            ui.label(RichText::new(msg).monospace().size(11.0));
        }
        // Add invisible widget at end to ensure scroll area updates
        if !log_messages.is_empty() {
            ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
        }
    });
}
