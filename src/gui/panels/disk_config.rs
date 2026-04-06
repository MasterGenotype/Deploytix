//! Disk configuration panel

use crate::config::{CustomPartitionEntry, Filesystem, SwapType};
use crate::disk::layouts::{BOOT_MIB, EFI_MIB};
use crate::gui::{state::DiskState, theme, widgets};
use egui::{RichText, Ui};

/// Estimated swap size for GUI space calculations (8 GiB).
const SWAP_ESTIMATE_MIB: u64 = 8192;

/// Minimum partition size in GiB shown on sliders.
const MIN_PART_GIB: u64 = 1;

/// Render the disk configuration panel. Returns `true` when configuration is valid.
pub fn show(ui: &mut Ui, disk: &mut DiskState) -> bool {
    widgets::page_heading(ui, "Disk Configuration");

    let disk_size_mib = disk.selected_disk_size_mib();

    let output = egui::ScrollArea::vertical().show(ui, |ui| {
        // ── Filesystem & Swap ──────────────────────────────────────
        widgets::section(ui, "Filesystem & Swap", |ui| {
            filesystem_section(
                ui,
                &mut disk.filesystem,
                &mut disk.swap_type,
                &mut disk.zram_percent,
            );
        });

        // ── Encryption ─────────────────────────────────────────────
        widgets::section(ui, "Encryption", |ui| {
            encryption_section(
                ui,
                &mut disk.encryption,
                &mut disk.encryption_password,
                &mut disk.boot_encryption,
                &mut disk.integrity,
            );
        });

        // ── LVM Thin Provisioning ──────────────────────────────────
        widgets::section(ui, "LVM Thin Provisioning", |ui| {
            lvm_section(
                ui,
                &mut disk.use_lvm_thin,
                &mut disk.lvm_vg_name,
                &mut disk.lvm_thin_pool_name,
                &mut disk.lvm_thin_pool_percent,
            );
        });

        // Auto-enable subvolumes for btrfs
        disk.use_subvolumes = disk.filesystem == Filesystem::Btrfs;

        // ── Options ────────────────────────────────────────────────
        widgets::section(ui, "Options", |ui| {
            ui.checkbox(
                &mut disk.preserve_home,
                "Preserve existing /home (reinstall without overwriting user data)",
            );
            if disk.preserve_home {
                widgets::info_text(
                    ui,
                    "System partitions will be erased but /home will be kept intact.",
                );
                if disk.filesystem == Filesystem::Zfs {
                    widgets::validation_warning(ui, "preserve_home is not supported with ZFS");
                }
                if disk.use_lvm_thin {
                    widgets::validation_warning(
                        ui,
                        "preserve_home is not supported with LVM thin provisioning",
                    );
                }
                let has_home = disk.partitions.iter().any(|p| p.mount_point == "/home");
                if !has_home && !disk.use_subvolumes {
                    widgets::validation_warning(
                        ui,
                        "preserve_home requires a /home partition or subvolumes",
                    );
                }
            }
        });

        // ── Partitions ─────────────────────────────────────────────
        widgets::section(ui, "Partitions", |ui| {
            partition_section(
                ui,
                disk_size_mib,
                &disk.swap_type,
                &mut disk.partitions,
                &mut disk.new_partition_mount,
                &mut disk.new_partition_size,
                &mut disk.new_partition_label,
            );
        });

        // ── Validation ─────────────────────────────────────────────
        validate(ui, disk)
    });

    output.inner
}

fn filesystem_section(
    ui: &mut Ui,
    filesystem: &mut Filesystem,
    swap_type: &mut SwapType,
    zram_percent: &mut u8,
) {
    ui.horizontal(|ui| {
        ui.label("Filesystem:");
        egui::ComboBox::from_id_salt("filesystem")
            .selected_text(format!("{}", filesystem))
            .show_ui(ui, |ui| {
                ui.selectable_value(filesystem, Filesystem::Btrfs, "btrfs");
                ui.selectable_value(filesystem, Filesystem::Ext4, "ext4");
                ui.selectable_value(filesystem, Filesystem::Xfs, "xfs");
                ui.selectable_value(filesystem, Filesystem::Zfs, "zfs");
                ui.selectable_value(filesystem, Filesystem::F2fs, "f2fs");
            });
    });
    ui.add_space(theme::SPACING_XS);

    let supports_swap_file = *filesystem == Filesystem::Btrfs || *filesystem == Filesystem::Ext4;
    if !supports_swap_file && *swap_type == SwapType::FileZram {
        *swap_type = SwapType::Partition;
    }

    ui.horizontal(|ui| {
        ui.label("Swap Type:");
        egui::ComboBox::from_id_salt("swap_type")
            .selected_text(format!("{}", swap_type))
            .show_ui(ui, |ui| {
                ui.selectable_value(swap_type, SwapType::Partition, "Swap Partition");
                if supports_swap_file {
                    ui.selectable_value(swap_type, SwapType::FileZram, "Swap File + ZRAM");
                }
                ui.selectable_value(swap_type, SwapType::ZramOnly, "ZRAM Only");
            });
    });

    if *swap_type == SwapType::FileZram || *swap_type == SwapType::ZramOnly {
        ui.add_space(theme::SPACING_XS);
        ui.horizontal(|ui| {
            ui.label("ZRAM Size (% of RAM):");
            ui.add(egui::Slider::new(zram_percent, 10..=100).suffix("%"));
        });
    }
}

fn encryption_section(
    ui: &mut Ui,
    encryption: &mut bool,
    password: &mut String,
    boot_encryption: &mut bool,
    integrity: &mut bool,
) {
    ui.checkbox(encryption, "Enable LUKS encryption on data partitions");

    if *encryption {
        ui.add_space(theme::SPACING_SM);
        ui.horizontal(|ui| {
            ui.label("Password:");
            ui.add(egui::TextEdit::singleline(password).password(true));
        });
        ui.add_space(theme::SPACING_XS);

        ui.checkbox(integrity, "Enable dm-integrity (per-sector HMAC-SHA256)");
        if *integrity {
            widgets::info_text(
                ui,
                "Detects silent data corruption. Disables TRIM/discard support.",
            );
        }
        ui.add_space(theme::SPACING_XS);

        ui.checkbox(boot_encryption, "Encrypt /boot partition (LUKS1)");
        if *integrity && *boot_encryption {
            widgets::info_text(
                ui,
                "Note: /boot uses LUKS1 without integrity (LUKS1 does not support dm-integrity)",
            );
        }
    } else {
        *boot_encryption = false;
        *integrity = false;
    }
}

fn lvm_section(
    ui: &mut Ui,
    use_lvm_thin: &mut bool,
    vg_name: &mut String,
    pool_name: &mut String,
    pool_percent: &mut u8,
) {
    ui.checkbox(use_lvm_thin, "Enable LVM thin provisioning");

    if *use_lvm_thin {
        widgets::info_text(
            ui,
            "Data partitions are collapsed into a single LVM PV with thin volumes.",
        );
        ui.add_space(theme::SPACING_SM);

        ui.horizontal(|ui| {
            ui.label("Volume Group Name:");
            ui.text_edit_singleline(vg_name);
        });
        ui.add_space(theme::SPACING_XS);

        ui.horizontal(|ui| {
            ui.label("Thin Pool Name:");
            ui.text_edit_singleline(pool_name);
        });
        ui.add_space(theme::SPACING_XS);

        ui.horizontal(|ui| {
            ui.label("Thin Pool Size (% of VG):");
            ui.add(egui::Slider::new(pool_percent, 50..=100).suffix("%"));
        });
        ui.add_space(theme::SPACING_XS);

        widgets::info_text(
            ui,
            "Thin volumes: root (50G), usr (50G), var (30G), home (200G)",
        );
    }
}

fn partition_section(
    ui: &mut Ui,
    disk_size_mib: u64,
    swap_type: &SwapType,
    partitions: &mut Vec<CustomPartitionEntry>,
    new_mount: &mut String,
    new_size: &mut String,
    new_label: &mut String,
) {
    let swap_overhead = if *swap_type == SwapType::Partition {
        SWAP_ESTIMATE_MIB
    } else {
        0
    };
    let reserved_mib = EFI_MIB + BOOT_MIB + swap_overhead;
    let data_budget_mib = disk_size_mib.saturating_sub(reserved_mib);
    let data_budget_gib = data_budget_mib / 1024;

    widgets::info_text(
        ui,
        &format!(
            "System reserved: EFI 0.5 GiB + Boot 2 GiB{} \u{2014} {:.1} GiB available for data",
            if swap_overhead > 0 {
                format!(" + Swap ~{} GiB", swap_overhead / 1024)
            } else {
                String::new()
            },
            data_budget_mib as f64 / 1024.0,
        ),
    );
    ui.add_space(theme::SPACING_SM);

    // Per-partition sliders
    let mut remove_idx: Option<usize> = None;
    let fixed_total_mib: u64 = partitions.iter().map(|p| p.size_mib).sum();
    let remainder_gib = data_budget_mib.saturating_sub(fixed_total_mib) / 1024;

    egui::ScrollArea::vertical()
        .max_height(160.0)
        .id_salt("partitions_scroll")
        .show(ui, |ui| {
            let part_count = partitions.len();
            for i in 0..part_count {
                let is_remainder = partitions[i].size_mib == 0;
                let mount = partitions[i].mount_point.clone();
                let label = partitions[i].effective_label();

                ui.horizontal(|ui| {
                    ui.label(format!("{} ({})", mount, label));

                    if is_remainder {
                        ui.label(
                            RichText::new(format!("{} GiB (remainder)", remainder_gib))
                                .color(theme::TEXT_MUTED),
                        );
                    } else {
                        let current_gib = partitions[i].size_mib / 1024;
                        let other_fixed_mib: u64 = partitions
                            .iter()
                            .enumerate()
                            .filter(|(j, p)| *j != i && p.size_mib > 0)
                            .map(|(_, p)| p.size_mib)
                            .sum();
                        let max_gib = data_budget_mib
                            .saturating_sub(other_fixed_mib)
                            .saturating_sub(1024)
                            / 1024;
                        let max_gib = max_gib.max(MIN_PART_GIB);

                        let mut gib = current_gib;
                        ui.add(
                            egui::Slider::new(&mut gib, MIN_PART_GIB..=max_gib)
                                .suffix(" GiB")
                                .clamping(egui::SliderClamping::Always),
                        );
                        partitions[i].size_mib = gib * 1024;
                    }

                    if mount != "/" && ui.small_button("\u{2715}").clicked() {
                        remove_idx = Some(i);
                    }
                });
            }
        });

    if let Some(idx) = remove_idx {
        partitions.remove(idx);
    }

    // Allocation bar
    let allocated_gib = fixed_total_mib / 1024;
    let fraction = if data_budget_gib > 0 {
        (allocated_gib as f32) / (data_budget_gib as f32)
    } else {
        0.0
    };
    ui.add_space(theme::SPACING_XS);
    ui.add(egui::ProgressBar::new(fraction.min(1.0)).text(format!(
        "Allocated: {} / {} GiB",
        allocated_gib, data_budget_gib
    )));
    if allocated_gib > data_budget_gib {
        widgets::validation_error(ui, "Partitions exceed available disk space");
    }
    ui.add_space(theme::SPACING_SM);

    // Add new partition form
    ui.label(RichText::new("Add Partition").strong());
    ui.add_space(theme::SPACING_XS);
    ui.horizontal(|ui| {
        ui.label("Mount:");
        ui.add(egui::TextEdit::singleline(new_mount).desired_width(100.0));
        ui.label("Size (GiB, 0=rest):");
        ui.add(egui::TextEdit::singleline(new_size).desired_width(60.0));
        ui.label("Label:");
        ui.add(egui::TextEdit::singleline(new_label).desired_width(80.0));

        if ui.button("\u{2795} Add").clicked() {
            try_add_partition(partitions, new_mount, new_size, new_label);
        }
    });
}

fn try_add_partition(
    partitions: &mut Vec<CustomPartitionEntry>,
    mount: &mut String,
    size: &mut String,
    label: &mut String,
) {
    let m = mount.trim();
    let size_gib: u64 = size.parse().unwrap_or(0);

    let valid = m.starts_with('/')
        && m != "/boot"
        && m != "/boot/efi"
        && !m.is_empty()
        && !partitions.iter().any(|p| p.mount_point == m)
        && !(size_gib == 0 && partitions.iter().any(|p| p.size_mib == 0));

    if valid {
        let lbl = if label.trim().is_empty() {
            None
        } else {
            Some(label.trim().to_string())
        };
        partitions.push(CustomPartitionEntry {
            mount_point: m.to_string(),
            label: lbl,
            size_mib: size_gib * 1024,
            encryption: None,
        });
        mount.clear();
        size.clear();
        label.clear();
    }
}

fn validate(ui: &mut Ui, disk: &DiskState) -> bool {
    if disk.encryption && disk.encryption_password.is_empty() {
        widgets::validation_error(ui, "Please enter an encryption password");
        return false;
    }
    if disk.use_lvm_thin && disk.lvm_vg_name.is_empty() {
        widgets::validation_error(ui, "Volume group name cannot be empty");
        return false;
    }
    if disk.use_lvm_thin && disk.lvm_thin_pool_name.is_empty() {
        widgets::validation_error(ui, "Thin pool name cannot be empty");
        return false;
    }
    if disk.partitions.is_empty() {
        widgets::validation_error(ui, "At least one partition is required");
        return false;
    }
    if !disk.partitions.iter().any(|p| p.mount_point == "/") {
        widgets::validation_error(ui, "A root (/) partition is required");
        return false;
    }
    true
}
