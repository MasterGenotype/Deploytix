//! Partition layout definitions and sizing calculations
//!
//! Ported from Disk-Populater.sh

use crate::config::{CustomPartitionEntry, PartitionLayout};
use crate::disk::detection::get_ram_mib;
use crate::utils::error::{DeploytixError, Result};

/// GPT partition type GUIDs
#[allow(dead_code)]
pub mod partition_types {
    pub const EFI: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
    pub const BIOS_BOOT: &str = "21686148-6449-6E6F-744E-656564454649";
    pub const LINUX_SWAP: &str = "0657FD6D-A4AB-43C4-84E5-0933C84B4F4F";
    pub const LINUX_ROOT_X86_64: &str = "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709";
    pub const LINUX_USR_X86_64: &str = "8484680C-9521-48C6-9C11-B0720656F69E";
    pub const LINUX_VAR: &str = "4D21B016-B534-45C2-A9FB-5C16E091FD2D";
    pub const LINUX_HOME: &str = "933AC7E1-2EB4-4F13-B844-0E14E2AEF915";
    pub const LINUX_FILESYSTEM: &str = "0FC63DAF-8483-4772-8E79-3D69D8477DE4";
}

/// Btrfs subvolume definition
#[derive(Debug, Clone)]
pub struct SubvolumeDef {
    /// Subvolume name (e.g., "@", "@home")
    pub name: String,
    /// Mount point (e.g., "/", "/home")
    pub mount_point: String,
    /// Mount options
    pub mount_options: String,
}

/// Create standard btrfs subvolume definitions
/// Following the common convention: @=root, @home, @var, @log, @snapshots
pub fn standard_subvolumes() -> Vec<SubvolumeDef> {
    let default_opts = "defaults,noatime,compress=zstd".to_string();
    vec![
        SubvolumeDef {
            name: "@".to_string(),
            mount_point: "/".to_string(),
            mount_options: default_opts.clone(),
        },
        SubvolumeDef {
            name: "@home".to_string(),
            mount_point: "/home".to_string(),
            mount_options: default_opts.clone(),
        },
        SubvolumeDef {
            name: "@var".to_string(),
            mount_point: "/var".to_string(),
            mount_options: default_opts.clone(),
        },
        SubvolumeDef {
            name: "@log".to_string(),
            mount_point: "/var/log".to_string(),
            mount_options: default_opts.clone(),
        },
        SubvolumeDef {
            name: "@snapshots".to_string(),
            mount_point: "/.snapshots".to_string(),
            mount_options: default_opts,
        },
    ]
}

/// A single partition definition
#[derive(Debug, Clone)]
pub struct PartitionDef {
    /// Partition number (1-based)
    pub number: u32,
    /// Partition name/label
    pub name: String,
    /// Size in MiB (0 = remainder of disk)
    pub size_mib: u64,
    /// GPT type GUID
    pub type_guid: String,
    /// Mount point (e.g., "/", "/boot", etc.)
    pub mount_point: Option<String>,
    /// Whether this is a swap partition
    pub is_swap: bool,
    /// Whether this is the EFI partition
    pub is_efi: bool,
    /// Whether this is a LUKS container partition
    pub is_luks: bool,
    /// Whether this is a BIOS Boot partition
    pub is_bios_boot: bool,
    /// Whether this is a BIOS FileSystem Partition
    pub is_boot_fs: bool,
    /// Additional attributes (e.g., LegacyBIOSBootable)
    pub attributes: Option<String>,
}

/// Computed partition layout for a specific disk
#[derive(Debug, Clone)]
pub struct ComputedLayout {
    pub partitions: Vec<PartitionDef>,
    pub total_mib: u64,
    /// Btrfs subvolumes (None for layouts with separate partitions)
    pub subvolumes: Option<Vec<SubvolumeDef>>,
}

impl ComputedLayout {
    /// Check if this layout uses btrfs subvolumes
    pub fn uses_subvolumes(&self) -> bool {
        self.subvolumes.is_some() && !self.subvolumes.as_ref().unwrap().is_empty()
    }
}

/// Sizing constants from Disk-Populater.sh
const EFI_MIB: u64 = 512;
const BOOT_MIB: u64 = 2048;

/// Ratios for dynamic sizing
const ROOT_RATIO: f64 = 0.06441;
const USR_RATIO: f64 = 0.26838;
const VAR_RATIO: f64 = 0.05368;

/// Minimum sizes
const ROOT_MIN_MIB: u64 = 20480; // 20 GiB
const USR_MIN_MIB: u64 = 20480; // 20 GiB
const VAR_MIN_MIB: u64 = 8192; // 8 GiB

/// Swap limits
const SWAP_MIN_MIB: u64 = 4096; // 4 GiB
const SWAP_MAX_MIB: u64 = 20480; // 20 GiB

/// Alignment in MiB
const ALIGN_MIB: u64 = 4;

/// Align value down to alignment boundary
fn floor_align(value: u64, align: u64) -> u64 {
    (value / align) * align
}

/// Clamp value between min and max
fn clamp(value: u64, min: u64, max: u64) -> u64 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// Calculate swap size based on RAM
fn calculate_swap_mib(ram_mib: u64) -> u64 {
    let swap = 2 * ram_mib;
    floor_align(clamp(swap, SWAP_MIN_MIB, SWAP_MAX_MIB), ALIGN_MIB)
}

/// Compute the standard 7-partition layout
///
/// Layout: EFI, Boot, Swap, Root, Usr, Var, Home
/// When encryption is enabled, Root/Usr/Var/Home partitions are marked as LUKS containers.
fn compute_standard_layout(disk_mib: u64, encryption: bool) -> Result<ComputedLayout> {
    let ram_mib = get_ram_mib();
    let swap_mib = calculate_swap_mib(ram_mib);

    // Reserved space (fixed partitions)
    let reserved_mib = EFI_MIB + BOOT_MIB + swap_mib;
    let remain_mib = disk_mib.saturating_sub(reserved_mib);

    // Minimum total required
    let min_total_mib = reserved_mib + ROOT_MIN_MIB + USR_MIN_MIB + VAR_MIN_MIB + 1;
    if disk_mib < min_total_mib {
        return Err(DeploytixError::DiskTooSmall {
            size_mib: disk_mib,
            required_mib: min_total_mib,
        });
    }

    // Calculate sizes based on ratios
    let mut root_mib = ((remain_mib as f64) * ROOT_RATIO) as u64;
    let mut usr_mib = ((remain_mib as f64) * USR_RATIO) as u64;
    let mut var_mib = ((remain_mib as f64) * VAR_RATIO) as u64;

    // Apply minimums
    if root_mib < ROOT_MIN_MIB {
        root_mib = ROOT_MIN_MIB;
    }
    if usr_mib < USR_MIN_MIB {
        usr_mib = USR_MIN_MIB;
    }
    if var_mib < VAR_MIN_MIB {
        var_mib = VAR_MIN_MIB;
    }

    // Align down
    root_mib = floor_align(root_mib, ALIGN_MIB);
    usr_mib = floor_align(usr_mib, ALIGN_MIB);
    var_mib = floor_align(var_mib, ALIGN_MIB);

    // Calculate home (remainder)
    let home_mib = disk_mib
        .saturating_sub(EFI_MIB)
        .saturating_sub(BOOT_MIB)
        .saturating_sub(swap_mib)
        .saturating_sub(root_mib)
        .saturating_sub(usr_mib)
        .saturating_sub(var_mib);

    // If home is too small, reduce other partitions deterministically
    if home_mib == 0 || home_mib > disk_mib {
        let mut deficit =
            (EFI_MIB + BOOT_MIB + swap_mib + root_mib + usr_mib + var_mib).saturating_sub(disk_mib);

        // Reduce USR first
        let reducible = usr_mib.saturating_sub(USR_MIN_MIB);
        if reducible > 0 && deficit > 0 {
            let take = deficit.min(reducible);
            usr_mib -= take;
            usr_mib = floor_align(usr_mib, ALIGN_MIB);
            deficit -= take;
        }

        // Then ROOT
        let reducible = root_mib.saturating_sub(ROOT_MIN_MIB);
        if reducible > 0 && deficit > 0 {
            let take = deficit.min(reducible);
            root_mib -= take;
            root_mib = floor_align(root_mib, ALIGN_MIB);
            deficit -= take;
        }

        // Then VAR
        let reducible = var_mib.saturating_sub(VAR_MIN_MIB);
        if reducible > 0 && deficit > 0 {
            let take = deficit.min(reducible);
            var_mib -= take;
            var_mib = floor_align(var_mib, ALIGN_MIB);
        }

        // Note: home_mib will be 0 (remainder) in the partition definition
        // The recalculated value here is just for validation purposes
        let _recalculated_home = disk_mib
            .saturating_sub(EFI_MIB)
            .saturating_sub(BOOT_MIB)
            .saturating_sub(swap_mib)
            .saturating_sub(root_mib)
            .saturating_sub(usr_mib)
            .saturating_sub(var_mib);
    }

    let partitions = vec![
        PartitionDef {
            number: 1,
            name: "EFI".to_string(),
            size_mib: EFI_MIB,
            type_guid: partition_types::EFI.to_string(),
            mount_point: Some("/boot/efi".to_string()),
            is_swap: false,
            is_efi: true,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        },
        PartitionDef {
            number: 2,
            name: "BOOT".to_string(),
            size_mib: BOOT_MIB,
            type_guid: partition_types::LINUX_FILESYSTEM.to_string(),
            mount_point: Some("/boot".to_string()),
            is_swap: false,
            is_efi: false,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: true,
            attributes: Some("LegacyBIOSBootable".to_string()),
        },
        PartitionDef {
            number: 3,
            name: "SWAP".to_string(),
            size_mib: swap_mib,
            type_guid: partition_types::LINUX_SWAP.to_string(),
            mount_point: None,
            is_swap: true,
            is_efi: false,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        },
        PartitionDef {
            number: 4,
            name: "ROOT".to_string(),
            size_mib: root_mib,
            type_guid: partition_types::LINUX_ROOT_X86_64.to_string(),
            mount_point: Some("/".to_string()),
            is_swap: false,
            is_efi: false,
            is_luks: encryption,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        },
        PartitionDef {
            number: 5,
            name: "USR".to_string(),
            size_mib: usr_mib,
            type_guid: partition_types::LINUX_USR_X86_64.to_string(),
            mount_point: Some("/usr".to_string()),
            is_swap: false,
            is_efi: false,
            is_luks: encryption,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        },
        PartitionDef {
            number: 6,
            name: "VAR".to_string(),
            size_mib: var_mib,
            type_guid: partition_types::LINUX_VAR.to_string(),
            mount_point: Some("/var".to_string()),
            is_swap: false,
            is_efi: false,
            is_luks: encryption,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        },
        PartitionDef {
            number: 7,
            name: "HOME".to_string(),
            size_mib: 0, // Remainder
            type_guid: partition_types::LINUX_HOME.to_string(),
            mount_point: Some("/home".to_string()),
            is_swap: false,
            is_efi: false,
            is_luks: encryption,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        },
    ];

    Ok(ComputedLayout {
        partitions,
        total_mib: disk_mib,
        subvolumes: None,
    })
}

/// Compute the minimal 4-partition layout with btrfs subvolumes
///
/// Layout: EFI, Boot, Swap, Root (with @, @home, @var, @log, @snapshots subvolumes)
/// This layout supports both UEFI and Legacy BIOS boot.
fn compute_minimal_layout(disk_mib: u64) -> Result<ComputedLayout> {
    let ram_mib = get_ram_mib();
    let swap_mib = calculate_swap_mib(ram_mib);

    // Minimum total required
    let min_total_mib = EFI_MIB + BOOT_MIB + swap_mib + ROOT_MIN_MIB;
    if disk_mib < min_total_mib {
        return Err(DeploytixError::DiskTooSmall {
            size_mib: disk_mib,
            required_mib: min_total_mib,
        });
    }

    let partitions = vec![
        PartitionDef {
            number: 1,
            name: "EFI".to_string(),
            size_mib: EFI_MIB,
            type_guid: partition_types::EFI.to_string(),
            mount_point: Some("/boot/efi".to_string()),
            is_swap: false,
            is_efi: true,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        },
        PartitionDef {
            number: 2,
            name: "BOOT".to_string(),
            size_mib: BOOT_MIB,
            type_guid: partition_types::LINUX_FILESYSTEM.to_string(),
            mount_point: Some("/boot".to_string()),
            is_swap: false,
            is_efi: false,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: true,
            attributes: Some("LegacyBIOSBootable".to_string()),
        },
        PartitionDef {
            number: 3,
            name: "SWAP".to_string(),
            size_mib: swap_mib,
            type_guid: partition_types::LINUX_SWAP.to_string(),
            mount_point: None,
            is_swap: true,
            is_efi: false,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        },
        PartitionDef {
            number: 4,
            name: "ROOT".to_string(),
            size_mib: 0, // Remainder
            type_guid: partition_types::LINUX_ROOT_X86_64.to_string(),
            // Note: mount_point is None because we mount subvolumes, not the raw partition
            mount_point: None,
            is_swap: false,
            is_efi: false,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        },
    ];

    Ok(ComputedLayout {
        partitions,
        total_mib: disk_mib,
        // Use standard btrfs subvolumes for the Minimal layout
        subvolumes: Some(standard_subvolumes()),
    })
}

/// Get all LUKS partition definitions from layout
pub fn get_luks_partitions(layout: &ComputedLayout) -> Vec<&PartitionDef> {
    layout.partitions.iter().filter(|p| p.is_luks).collect()
}

/// Compute the LVM thin provisioning layout
///
/// Layout: EFI, Boot, optional Swap (if use_swap_partition), LVM PV (LUKS container)
/// The LVM PV contains a thin pool with thin volumes for root, usr, var, home.
fn compute_lvm_thin_layout(disk_mib: u64, use_swap_partition: bool) -> Result<ComputedLayout> {
    let ram_mib = get_ram_mib();
    let swap_mib = if use_swap_partition {
        calculate_swap_mib(ram_mib)
    } else {
        0
    };

    // Minimum: 512 MiB EFI + 2048 MiB Boot + optional Swap + at least 50 GiB for LVM
    let min_lvm_mib: u64 = 51200; // 50 GiB minimum for thin pool
    let min_total_mib = EFI_MIB + BOOT_MIB + swap_mib + min_lvm_mib;
    if disk_mib < min_total_mib {
        return Err(DeploytixError::DiskTooSmall {
            size_mib: disk_mib,
            required_mib: min_total_mib,
        });
    }

    let mut partitions = vec![
        // Partition 1: EFI System Partition
        PartitionDef {
            number: 1,
            name: "EFI".to_string(),
            size_mib: EFI_MIB,
            type_guid: partition_types::EFI.to_string(),
            mount_point: Some("/boot/efi".to_string()),
            is_swap: false,
            is_efi: true,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        },
        // Partition 2: Boot (can be LUKS1 encrypted)
        PartitionDef {
            number: 2,
            name: "BOOT".to_string(),
            size_mib: BOOT_MIB,
            type_guid: partition_types::LINUX_FILESYSTEM.to_string(),
            mount_point: Some("/boot".to_string()),
            is_swap: false,
            is_efi: false,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: true,
            attributes: Some("LegacyBIOSBootable".to_string()),
        },
    ];

    let mut next_part_num = 3;

    // Optional swap partition
    if use_swap_partition {
        partitions.push(PartitionDef {
            number: next_part_num,
            name: "SWAP".to_string(),
            size_mib: swap_mib,
            type_guid: partition_types::LINUX_SWAP.to_string(),
            mount_point: None,
            is_swap: true,
            is_efi: false,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        });
        next_part_num += 1;
    }

    // LVM PV partition (LUKS container, remainder of disk)
    partitions.push(PartitionDef {
        number: next_part_num,
        name: "LVM".to_string(),
        size_mib: 0, // Remainder
        type_guid: partition_types::LINUX_FILESYSTEM.to_string(),
        mount_point: None, // Mounted via LVM
        is_swap: false,
        is_efi: false,
        is_luks: true, // Will be LUKS encrypted
        is_bios_boot: false,
        is_boot_fs: false,
        attributes: None,
    });

    Ok(ComputedLayout {
        partitions,
        total_mib: disk_mib,
        subvolumes: None, // LVM thin uses LVs, not btrfs subvolumes
    })
}

/// Compute the custom user-defined layout
///
/// Layout: EFI, Boot, optional Swap, then user-defined partitions from `entries`.
/// Exactly one partition may have `size_mib = 0` (remainder).
fn compute_custom_layout(
    disk_mib: u64,
    encryption: bool,
    use_swap_partition: bool,
    entries: &[CustomPartitionEntry],
) -> Result<ComputedLayout> {
    let ram_mib = get_ram_mib();
    let swap_mib = if use_swap_partition {
        calculate_swap_mib(ram_mib)
    } else {
        0
    };

    // Validate: at most one remainder partition
    let remainder_count = entries.iter().filter(|e| e.size_mib == 0).count();
    if remainder_count > 1 {
        return Err(DeploytixError::ConfigError(
            "Only one custom partition may specify size_mib = 0 (remainder)".to_string(),
        ));
    }

    // Calculate reserved space for system partitions
    let reserved_mib = EFI_MIB + BOOT_MIB + swap_mib;

    // Calculate total fixed size from user entries
    let fixed_total: u64 = entries.iter().map(|e| e.size_mib).sum();

    // Validate disk has enough space
    let min_required = reserved_mib + fixed_total + if remainder_count == 0 { 0 } else { 1024 }; // 1GiB minimum for remainder
    if disk_mib < min_required {
        return Err(DeploytixError::DiskTooSmall {
            size_mib: disk_mib,
            required_mib: min_required,
        });
    }

    // Build system partitions
    let mut partitions = vec![
        PartitionDef {
            number: 1,
            name: "EFI".to_string(),
            size_mib: EFI_MIB,
            type_guid: partition_types::EFI.to_string(),
            mount_point: Some("/boot/efi".to_string()),
            is_swap: false,
            is_efi: true,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        },
        PartitionDef {
            number: 2,
            name: "BOOT".to_string(),
            size_mib: BOOT_MIB,
            type_guid: partition_types::LINUX_FILESYSTEM.to_string(),
            mount_point: Some("/boot".to_string()),
            is_swap: false,
            is_efi: false,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: true,
            attributes: Some("LegacyBIOSBootable".to_string()),
        },
    ];

    let mut next_part_num: u32 = 3;

    // Optional swap partition
    if use_swap_partition {
        partitions.push(PartitionDef {
            number: next_part_num,
            name: "SWAP".to_string(),
            size_mib: swap_mib,
            type_guid: partition_types::LINUX_SWAP.to_string(),
            mount_point: None,
            is_swap: true,
            is_efi: false,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        });
        next_part_num += 1;
    }

    // Add user-defined partitions
    for entry in entries {
        let label = entry.effective_label();
        let is_luks = entry.is_encrypted(encryption);

        // Determine GPT type GUID based on mount point
        let type_guid = match entry.mount_point.as_str() {
            "/" => partition_types::LINUX_ROOT_X86_64,
            "/usr" => partition_types::LINUX_USR_X86_64,
            "/var" => partition_types::LINUX_VAR,
            "/home" => partition_types::LINUX_HOME,
            _ => partition_types::LINUX_FILESYSTEM,
        }
        .to_string();

        // Align size (remainder stays 0)
        let size_mib = if entry.size_mib > 0 {
            floor_align(entry.size_mib, ALIGN_MIB)
        } else {
            0
        };

        partitions.push(PartitionDef {
            number: next_part_num,
            name: label,
            size_mib,
            type_guid,
            mount_point: Some(entry.mount_point.clone()),
            is_swap: false,
            is_efi: false,
            is_luks,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        });
        next_part_num += 1;
    }

    Ok(ComputedLayout {
        partitions,
        total_mib: disk_mib,
        subvolumes: None, // Custom layout uses separate partitions
    })
}

/// Compute partition layout for a disk
///
/// For Standard layout, pass `encryption` flag to enable LUKS on data partitions.
/// For Custom layout, pass `custom_partitions` with user-defined partition entries.
pub fn compute_layout(
    layout: &PartitionLayout,
    disk_mib: u64,
    encryption: bool,
    use_swap_partition: bool,
    custom_partitions: Option<&[CustomPartitionEntry]>,
) -> Result<ComputedLayout> {
    match layout {
        PartitionLayout::Standard => compute_standard_layout(disk_mib, encryption),
        PartitionLayout::Minimal => compute_minimal_layout(disk_mib),
        PartitionLayout::LvmThin => compute_lvm_thin_layout(disk_mib, use_swap_partition),
        PartitionLayout::Custom => {
            let entries = custom_partitions.ok_or_else(|| {
                DeploytixError::ConfigError("Custom layout requires custom_partitions".into())
            })?;
            compute_custom_layout(disk_mib, encryption, use_swap_partition, entries)
        }
    }
}

/// Compute LVM thin layout with swap type consideration
#[allow(dead_code)]
pub fn compute_lvm_thin_layout_with_swap(
    disk_mib: u64,
    use_swap_partition: bool,
) -> Result<ComputedLayout> {
    compute_lvm_thin_layout(disk_mib, use_swap_partition)
}

/// Check if layout has a separate /usr partition
#[allow(dead_code)]
pub fn has_usr_partition(layout: &PartitionLayout) -> bool {
    matches!(layout, PartitionLayout::Standard)
}

/// Print layout summary
pub fn print_layout_summary(layout: &ComputedLayout) {
    println!("\nPartition layout (total: {} MiB):", layout.total_mib);
    println!("{:<6} {:<10} {:>10} {:<20}", "NUM", "NAME", "SIZE", "MOUNT");
    println!("{}", "-".repeat(50));

    for part in &layout.partitions {
        let size_str = if part.size_mib == 0 {
            "remainder".to_string()
        } else {
            format!("{} MiB", part.size_mib)
        };

        println!(
            "{:<6} {:<10} {:>10} {:<20}",
            part.number,
            part.name,
            size_str,
            part.mount_point.as_deref().unwrap_or("-")
        );
    }
    println!();
}
