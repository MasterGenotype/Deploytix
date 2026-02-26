//! Partition layout definitions and sizing calculations
//!
//! Ported from Disk-Populater.sh
//!
//! Layouts define the *partition table* only. Storage features (encryption,
//! LVM thin, subvolumes) are applied as layers by the installer pipeline.

use crate::config::{CustomPartitionEntry, DiskConfig, PartitionLayout, SwapType};
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

/// Planned thin volume definition (saved when LVM thin collapses partitions)
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PlannedThinVolume {
    /// Volume name (e.g., "root", "home")
    pub name: String,
    /// Virtual size string for lvcreate (e.g., "50G")
    pub virtual_size: String,
    /// Mount point (e.g., "/", "/home")
    pub mount_point: String,
}

/// Computed partition layout for a specific disk
#[derive(Debug, Clone)]
pub struct ComputedLayout {
    pub partitions: Vec<PartitionDef>,
    pub total_mib: u64,
    /// Btrfs subvolumes (None for layouts without subvolumes)
    pub subvolumes: Option<Vec<SubvolumeDef>>,
    /// When LVM thin is applied, the data partitions are collapsed into a
    /// single LVM PV partition and the original volumes are stored here
    /// as planned thin volumes.
    #[allow(dead_code)]
    pub planned_thin_volumes: Option<Vec<PlannedThinVolume>>,
}

impl ComputedLayout {
    /// Check if this layout uses btrfs subvolumes
    pub fn uses_subvolumes(&self) -> bool {
        self.subvolumes.is_some() && !self.subvolumes.as_ref().unwrap().is_empty()
    }

    /// Check if this layout has LVM thin provisioning
    #[allow(dead_code)]
    pub fn uses_lvm_thin(&self) -> bool {
        self.planned_thin_volumes.is_some()
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
/// Partitions are created without encryption flags; encryption is applied
/// as a layer by the installer pipeline.
fn compute_standard_layout(disk_mib: u64) -> Result<ComputedLayout> {
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
    if home_mib == 0 {
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
            // is_bios_boot = true sets the GPT LegacyBIOSBootable attribute bit
            // (the "bootable" flag in fdisk/sfdisk expert mode), enabling GRUB
            // to locate this partition on legacy BIOS systems.
            is_bios_boot: true,
            is_boot_fs: true,
            attributes: None,
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
            is_luks: false,
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
            is_luks: false,
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
            is_luks: false,
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
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        },
    ];

    Ok(ComputedLayout {
        partitions,
        total_mib: disk_mib,
        subvolumes: None,
        planned_thin_volumes: None,
    })
}

/// Compute the minimal 4-partition layout
///
/// Layout: EFI, Boot, Swap, Root
/// This layout supports both UEFI and Legacy BIOS boot.
/// Subvolumes are applied separately by the installer if use_subvolumes is enabled.
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
            is_bios_boot: true,
            is_boot_fs: true,
            attributes: None,
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
            mount_point: Some("/".to_string()),
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
        subvolumes: None,
        planned_thin_volumes: None,
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
            is_bios_boot: true,
            is_boot_fs: true,
            attributes: None,
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
        subvolumes: None,
        planned_thin_volumes: None,
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
            is_bios_boot: true,
            is_boot_fs: true,
            attributes: None,
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
        subvolumes: None,
        planned_thin_volumes: None,
    })
}

/// Compute partition layout for a disk from a `DiskConfig`.
///
/// The layout defines the partition table only. Encryption flags (`is_luks`)
/// are applied as a post-processing step so that the same layout function
/// works regardless of whether encryption is enabled.
///
/// When `use_lvm_thin` is true, `apply_lvm_thin_to_layout` is called
/// automatically to collapse data partitions into a single LVM PV.
pub fn compute_layout_from_config(
    disk_config: &DiskConfig,
    disk_mib: u64,
) -> Result<ComputedLayout> {
    let use_swap_partition = disk_config.swap_type == SwapType::Partition;

    let mut layout = match disk_config.layout {
        PartitionLayout::Standard => compute_standard_layout(disk_mib)?,
        PartitionLayout::Minimal => compute_minimal_layout(disk_mib)?,
        PartitionLayout::LvmThin => {
            // Legacy compat: should have been normalized to Standard + use_lvm_thin
            // by DeploymentConfig::normalize_legacy_lvmthin(). Handle here as safety net.
            compute_standard_layout(disk_mib)?
        }
        PartitionLayout::Custom => {
            let entries = disk_config.custom_partitions.as_deref().ok_or_else(|| {
                DeploytixError::ConfigError("Custom layout requires custom_partitions".into())
            })?;
            compute_custom_layout(
                disk_mib,
                disk_config.encryption,
                use_swap_partition,
                entries,
            )?
        }
    };

    // Apply encryption flags to data partitions.
    // When LVM thin is active, encryption is applied to the single LVM PV
    // partition by apply_lvm_thin_to_layout, not to individual data partitions.
    if disk_config.encryption && !disk_config.use_lvm_thin {
        apply_encryption_flags(&mut layout);
    }

    // Apply subvolumes if requested.
    // When subvolumes are active, the raw ROOT partition is not mounted directly;
    // clear its mount_point so that subvolumes are mounted instead.
    if disk_config.use_subvolumes {
        layout.subvolumes = Some(standard_subvolumes());
        for part in &mut layout.partitions {
            if part.mount_point.as_deref() == Some("/") && !part.is_efi && !part.is_boot_fs {
                part.mount_point = None;
            }
        }
    }

    // Apply LVM thin: collapse data partitions into a single LVM PV
    if disk_config.use_lvm_thin {
        layout = apply_lvm_thin_to_layout(layout, disk_config.encryption)?;
    }

    Ok(layout)
}

/// Legacy wrapper — kept for backward compatibility with existing call sites.
/// Prefer `compute_layout_from_config`.
#[allow(dead_code)]
pub fn compute_layout(
    layout: &PartitionLayout,
    disk_mib: u64,
    encryption: bool,
    use_swap_partition: bool,
    custom_partitions: Option<&[CustomPartitionEntry]>,
) -> Result<ComputedLayout> {
    let mut computed = match layout {
        PartitionLayout::Standard | PartitionLayout::LvmThin => {
            // LvmThin normalised to Standard; compute as standard.
            compute_standard_layout(disk_mib)?
        }
        PartitionLayout::Minimal => compute_minimal_layout(disk_mib)?,
        PartitionLayout::Custom => {
            let entries = custom_partitions.ok_or_else(|| {
                DeploytixError::ConfigError("Custom layout requires custom_partitions".into())
            })?;
            compute_custom_layout(disk_mib, encryption, use_swap_partition, entries)?
        }
    };

    if encryption {
        apply_encryption_flags(&mut computed);
    }

    Ok(computed)
}

/// Mark data partitions (non-EFI, non-boot, non-swap) as LUKS containers.
fn apply_encryption_flags(layout: &mut ComputedLayout) {
    for part in &mut layout.partitions {
        if !part.is_efi && !part.is_boot_fs && !part.is_swap && !part.is_bios_boot {
            part.is_luks = true;
        }
    }
}

/// Transform a layout by collapsing data partitions into a single LVM PV partition.
///
/// System partitions (EFI, Boot, Swap) are preserved. Data partitions are
/// removed from the partition table and recorded as `planned_thin_volumes`
/// to be created as thin LVs inside the LVM PV.
pub fn apply_lvm_thin_to_layout(
    layout: ComputedLayout,
    encryption: bool,
) -> Result<ComputedLayout> {
    // Separate system partitions from data partitions
    let mut system_parts: Vec<PartitionDef> = Vec::new();
    let mut planned_volumes: Vec<PlannedThinVolume> = Vec::new();

    for part in layout.partitions {
        if part.is_efi || part.is_boot_fs || part.is_swap || part.is_bios_boot {
            system_parts.push(part);
        } else {
            // Data partition: convert to a planned thin volume.
            // mount_point may be Some("/home") or None (cleared by subvolumes
            // for the ROOT partition).  Derive the mount point from the name
            // when it has been cleared.
            let mount_point = part
                .mount_point
                .clone()
                .unwrap_or_else(|| {
                    if part.name.eq_ignore_ascii_case("ROOT") {
                        "/".to_string()
                    } else {
                        format!("/{}", part.name.to_lowercase())
                    }
                });

            // Convert data partition sizes to virtual thin volume sizes
            let virtual_size = if part.size_mib == 0 {
                // Remainder partition gets a generous virtual size
                "200G".to_string()
            } else if part.size_mib >= 1024 {
                format!("{}G", part.size_mib / 1024)
            } else {
                format!("{}M", part.size_mib)
            };

            planned_volumes.push(PlannedThinVolume {
                name: part.name.to_lowercase(),
                virtual_size,
                mount_point,
            });
        }
    }

    // Determine next partition number after system partitions
    let next_part_num = system_parts.iter().map(|p| p.number).max().unwrap_or(0) + 1;

    // Add the LVM PV partition (takes remainder of disk)
    system_parts.push(PartitionDef {
        number: next_part_num,
        name: "LVM".to_string(),
        size_mib: 0, // Remainder
        type_guid: partition_types::LINUX_FILESYSTEM.to_string(),
        mount_point: None,
        is_swap: false,
        is_efi: false,
        is_luks: encryption, // Only LUKS-encrypted when encryption is enabled
        is_bios_boot: false,
        is_boot_fs: false,
        attributes: None,
    });

    Ok(ComputedLayout {
        partitions: system_parts,
        total_mib: layout.total_mib,
        subvolumes: layout.subvolumes,
        planned_thin_volumes: Some(planned_volumes),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PartitionLayout;

    // ── Pure math helpers ────────────────────────────────────────────────────

    #[test]
    fn floor_align_rounds_down_to_nearest_multiple() {
        assert_eq!(floor_align(0, 4), 0);
        assert_eq!(floor_align(3, 4), 0);
        assert_eq!(floor_align(4, 4), 4);
        assert_eq!(floor_align(7, 4), 4);
        assert_eq!(floor_align(1023, 1024), 0);
        assert_eq!(floor_align(1024, 1024), 1024);
        assert_eq!(floor_align(2047, 1024), 1024);
        assert_eq!(floor_align(2048, 1024), 2048);
    }

    #[test]
    fn clamp_returns_value_within_range() {
        assert_eq!(clamp(50, 0, 100), 50);
        assert_eq!(clamp(0, 10, 100), 10, "below min should clamp up");
        assert_eq!(clamp(200, 0, 100), 100, "above max should clamp down");
        assert_eq!(clamp(0, 0, 100), 0, "equal to min should be unchanged");
        assert_eq!(clamp(100, 0, 100), 100, "equal to max should be unchanged");
    }

    #[test]
    fn calculate_swap_mib_doubles_ram_within_bounds() {
        // 512 MiB RAM -> 2*512 = 1024, below SWAP_MIN_MIB (4096), so clamped up
        let small = calculate_swap_mib(512);
        assert_eq!(small, floor_align(SWAP_MIN_MIB, ALIGN_MIB));

        // 8 GiB RAM -> 2*8192 = 16384, within bounds
        let typical = calculate_swap_mib(8192);
        assert_eq!(typical, floor_align(16384, ALIGN_MIB));

        // 64 GiB RAM -> 2*65536 = 131072, above SWAP_MAX_MIB (20480), clamped down
        let large = calculate_swap_mib(65536);
        assert_eq!(large, floor_align(SWAP_MAX_MIB, ALIGN_MIB));
    }

    #[test]
    fn calculate_swap_mib_result_is_always_aligned() {
        for ram in [512, 1024, 2048, 4096, 8192, 16384, 32768, 65536] {
            let swap = calculate_swap_mib(ram);
            assert_eq!(
                swap % ALIGN_MIB,
                0,
                "swap for RAM={} must be 4 MiB aligned",
                ram
            );
        }
    }

    // ── Layout query helpers ─────────────────────────────────────────────────

    fn make_partition(number: u32, name: &str, is_luks: bool) -> PartitionDef {
        PartitionDef {
            number,
            name: name.to_string(),
            size_mib: 1024,
            type_guid: String::new(),
            mount_point: Some(format!("/{}", name.to_lowercase())),
            is_swap: false,
            is_efi: false,
            is_luks,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
        }
    }

    fn make_layout(partitions: Vec<PartitionDef>) -> ComputedLayout {
        ComputedLayout {
            total_mib: 100_000,
            partitions,
            subvolumes: None,
            planned_thin_volumes: None,
        }
    }

    #[test]
    fn get_luks_partitions_returns_only_encrypted_partitions() {
        let layout = make_layout(vec![
            make_partition(1, "EFI", false),
            make_partition(2, "ROOT", true),
            make_partition(3, "HOME", true),
        ]);
        let luks = get_luks_partitions(&layout);
        assert_eq!(luks.len(), 2);
        assert!(luks.iter().all(|p| p.is_luks));
        assert_eq!(luks[0].name, "ROOT");
        assert_eq!(luks[1].name, "HOME");
    }

    #[test]
    fn get_luks_partitions_empty_when_no_encryption() {
        let layout = make_layout(vec![
            make_partition(1, "EFI", false),
            make_partition(2, "ROOT", false),
        ]);
        assert!(get_luks_partitions(&layout).is_empty());
    }

    #[test]
    fn has_usr_partition_true_only_for_standard_layout() {
        assert!(has_usr_partition(&PartitionLayout::Standard));
        assert!(!has_usr_partition(&PartitionLayout::Minimal));
        assert!(!has_usr_partition(&PartitionLayout::LvmThin));
        assert!(!has_usr_partition(&PartitionLayout::Custom));
    }

    #[test]
    fn standard_subvolumes_includes_root_and_home() {
        let svols = standard_subvolumes();
        let mounts: Vec<&str> = svols.iter().map(|s| s.mount_point.as_str()).collect();
        assert!(mounts.contains(&"/"), "must include root subvolume");
        assert!(mounts.contains(&"/home"), "must include /home subvolume");
        assert!(!svols.is_empty());
    }

    #[test]
    fn standard_subvolumes_each_have_non_empty_fields() {
        for sv in standard_subvolumes() {
            assert!(!sv.name.is_empty(), "subvolume name must not be empty");
            assert!(
                sv.mount_point.starts_with('/'),
                "mount_point must start with /"
            );
            assert!(
                !sv.mount_options.is_empty(),
                "mount_options must not be empty"
            );
        }
    }
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
