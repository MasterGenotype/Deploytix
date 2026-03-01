//! Filesystem formatting

use crate::config::Filesystem;
use crate::disk::detection::partition_path;
use crate::disk::layouts::{ComputedLayout, SubvolumeDef};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::fs;
use tracing::info;

/// Format a partition with the specified filesystem
pub fn format_partition(
    cmd: &CommandRunner,
    partition: &str,
    filesystem: &Filesystem,
    label: Option<&str>,
) -> Result<()> {
    info!("Formatting {} as {}", partition, filesystem);

    let label_args: Vec<&str> = match (filesystem, label) {
        (Filesystem::Ext4, Some(l)) => vec!["-L", l],
        (Filesystem::Btrfs, Some(l)) => vec!["-L", l],
        (Filesystem::Xfs, Some(l)) => vec!["-L", l],
        (Filesystem::F2fs, Some(l)) => vec!["-l", l],
        _ => vec![],
    };

    let result = match filesystem {
        Filesystem::Ext4 => {
            let mut args = vec!["-F"];
            args.extend(&label_args);
            args.push(partition);
            cmd.run("mkfs.ext4", &args)
        }
        Filesystem::Btrfs => {
            let mut args = vec!["-f"];
            args.extend(&label_args);
            args.push(partition);
            cmd.run("mkfs.btrfs", &args)
        }
        Filesystem::Xfs => {
            let mut args = vec!["-f"];
            args.extend(&label_args);
            args.push(partition);
            cmd.run("mkfs.xfs", &args)
        }
        Filesystem::F2fs => {
            let mut args = vec!["-f"];
            args.extend(&label_args);
            args.push(partition);
            cmd.run("mkfs.f2fs", &args)
        }
        Filesystem::Zfs => {
            // ZFS uses zpool create rather than a traditional mkfs tool.
            // The pool name is taken from the label when provided, otherwise
            // derived from the partition path's last component.
            let pool_name =
                label.unwrap_or_else(|| partition.rsplit('/').next().unwrap_or("zpool"));
            cmd.run(
                "zpool",
                &[
                    "create",
                    "-f",
                    "-m",
                    "none",
                    "-o",
                    "ashift=12",
                    "-O",
                    "atime=off",
                    pool_name,
                    partition,
                ],
            )
        }
    };

    result.map(|_| ()).map_err(|e| {
        DeploytixError::FilesystemError(format!("Failed to format {}: {}", partition, e))
    })
}

/// Format the EFI partition as FAT32
pub fn format_efi(cmd: &CommandRunner, partition: &str) -> Result<()> {
    info!("Formatting {} as FAT32 (EFI)", partition);

    cmd.run("mkfs.vfat", &["-F32", "-n", "EFI", partition])
        .map(|_| ())
        .map_err(|e| {
            DeploytixError::FilesystemError(format!("Failed to format EFI partition: {}", e))
        })
}
/// Format the /boot filesystem partition with the configured boot filesystem.
///
/// The /boot partition stores the kernel, initramfs, and GRUB configuration.
/// It must NOT be confused with the raw BIOS Boot partition (core.img area),
/// which carries no filesystem and must never be formatted.
/// In standalone GRUB mode the .efi binary is placed on the EFI (FAT32)
/// partition; the /boot partition is still needed for kernel/initramfs files.
///
/// When `boot_filesystem` is ZFS, a dedicated boot pool (`bpool`) is created
/// with a restricted feature set that GRUB can read.
pub fn format_boot_partition(
    cmd: &CommandRunner,
    partition: &str,
    boot_filesystem: &Filesystem,
) -> Result<()> {
    info!("Formatting {} as {} (BOOT)", partition, boot_filesystem);
    if *boot_filesystem == Filesystem::Zfs {
        return create_zfs_boot_pool(cmd, partition);
    }
    format_partition(cmd, partition, boot_filesystem, Some("BOOT")).map_err(|e| {
        DeploytixError::FilesystemError(format!("Failed to format BOOT partition: {}", e))
    })
}

/// Format a swap partition
pub fn format_swap(cmd: &CommandRunner, partition: &str, label: Option<&str>) -> Result<()> {
    info!("Formatting {} as swap", partition);

    let mut args = vec![];
    if let Some(l) = label {
        args.extend(["-L", l]);
    }
    args.push(partition);

    cmd.run("mkswap", &args).map(|_| ()).map_err(|e| {
        DeploytixError::FilesystemError(format!("Failed to format swap partition: {}", e))
    })
}

/// Format all partitions according to the layout.
///
/// `filesystem` applies to data partitions; `boot_filesystem` applies to the
/// /boot partition (`is_boot_fs`).  The EFI partition is always FAT32.
///
/// `is_bios_boot` is purely a GPT attribute (LegacyBIOSBootable, the
/// "Bootable" flag in fdisk/sfdisk expert mode) and has NO effect on
/// formatting.  The /boot partition carries *both* `is_bios_boot` and
/// `is_boot_fs`; its filesystem is determined by `boot_filesystem`.
/// A standalone `is_bios_boot` partition (without `is_boot_fs`) is raw
/// storage for GRUB core.img and is never formatted.
pub fn format_all_partitions(
    cmd: &CommandRunner,
    device: &str,
    layout: &ComputedLayout,
    filesystem: &Filesystem,
    boot_filesystem: &Filesystem,
    preserve_home: bool,
) -> Result<()> {
    info!(
        "Formatting {} partitions on {} (data fs: {}, boot fs: {}, preserve_home: {})",
        layout.partitions.len(),
        device,
        filesystem,
        boot_filesystem,
        preserve_home,
    );

    for part in &layout.partitions {
        let part_path = partition_path(device, part.number);

        // Skip /home partition when preserve_home is enabled
        if preserve_home && part.mount_point.as_deref() == Some("/home") {
            info!(
                "Skipping {} (preserve_home: /home partition preserved)",
                part_path
            );
            continue;
        }

        // When preserve_home + subvolumes, skip the ROOT partition entirely.
        // The ROOT btrfs filesystem contains the @home subvolume; reformatting
        // it would destroy user data.  create_btrfs_subvolumes() handles the
        // selective refresh (delete all subvols except @home, then recreate).
        if preserve_home && layout.uses_subvolumes() && part.name == "ROOT" {
            info!(
                "Skipping {} (preserve_home + subvolumes: ROOT btrfs contains @home)",
                part_path
            );
            continue;
        }

        if part.is_efi {
            format_efi(cmd, &part_path)?;
        } else if part.is_bios_boot && !part.is_boot_fs {
            // Standalone BIOS Boot partition: raw area for GRUB core.img.
            // Only the GPT LegacyBIOSBootable attribute is set (by sfdisk);
            // no filesystem is ever applied.
            info!(
                "Skipping {} (BIOS Boot partition, raw — no filesystem)",
                part_path
            );
        } else if part.is_swap {
            format_swap(cmd, &part_path, Some(&part.name))?;
        } else if part.is_luks {
            // LUKS partitions are handled separately by the encryption module
            info!(
                "Skipping {} (LUKS partition, formatted by encryption module)",
                part_path
            );
        } else if part.is_boot_fs {
            // /boot filesystem: kernel, initramfs, and GRUB config live here.
            // Formatted with the chosen boot filesystem (not the data filesystem).
            format_boot_partition(cmd, &part_path, boot_filesystem)?;
        } else {
            format_partition(cmd, &part_path, filesystem, Some(&part.name))?;
        }
    }

    info!("All partitions formatted successfully");
    Ok(())
}

/// Get the UUID of a formatted partition
pub fn get_partition_uuid(partition: &str) -> Result<String> {
    let output = std::process::Command::new("blkid")
        .args(["-s", "UUID", "-o", "value", partition])
        .output()
        .map_err(|e| DeploytixError::FilesystemError(e.to_string()))?;

    if !output.status.success() {
        return Err(DeploytixError::FilesystemError(format!(
            "Failed to get UUID for {}",
            partition
        )));
    }

    let uuid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if uuid.is_empty() {
        return Err(DeploytixError::FilesystemError(format!(
            "blkid returned empty UUID for {}",
            partition
        )));
    }
    Ok(uuid)
}

/// Get all partition UUIDs for a layout
#[allow(dead_code)]
pub fn get_all_uuids(
    device: &str,
    layout: &ComputedLayout,
) -> Result<Vec<(String, String, Option<String>)>> {
    // Returns (partition_path, uuid, mount_point)
    let mut results = Vec::new();

    for part in &layout.partitions {
        let part_path = partition_path(device, part.number);
        let uuid = get_partition_uuid(&part_path)?;
        results.push((part_path, uuid, part.mount_point.clone()));
    }

    Ok(results)
}

/// Default ZFS data pool name.
pub const ZFS_RPOOL_NAME: &str = "rpool";
/// Default ZFS boot pool name.
pub const ZFS_BPOOL_NAME: &str = "bpool";
/// Default root dataset under the data pool.
pub const ZFS_ROOT_DATASET: &str = "rpool/ROOT";
/// Default boot dataset under the boot pool.
pub const ZFS_BOOT_DATASET: &str = "bpool/BOOT";

/// Create a ZFS data pool (`rpool`) on a device with production-ready defaults.
///
/// The pool is created with `mountpoint=none` so that all mounts are handled
/// via legacy mountpoints in fstab. Datasets are created separately by
/// [`create_zfs_datasets`].
pub fn create_zfs_pool(cmd: &CommandRunner, device: &str) -> Result<()> {
    info!("Creating ZFS data pool ({}) on {}", ZFS_RPOOL_NAME, device);

    if cmd.is_dry_run() {
        println!("  [dry-run] zpool create -f -o ashift=12 -O mountpoint=none -O atime=off -O compression=zstd -O xattr=sa -O acltype=posixacl {} {}",
            ZFS_RPOOL_NAME, device);
        return Ok(());
    }

    cmd.run(
        "zpool",
        &[
            "create",
            "-f",
            "-o",
            "ashift=12",
            "-O",
            "mountpoint=none",
            "-O",
            "atime=off",
            "-O",
            "compression=zstd",
            "-O",
            "xattr=sa",
            "-O",
            "acltype=posixacl",
            ZFS_RPOOL_NAME,
            device,
        ],
    )
    .map(|_| ())
    .map_err(|e| {
        DeploytixError::FilesystemError(format!(
            "Failed to create ZFS pool {}: {}",
            ZFS_RPOOL_NAME, e
        ))
    })
}

/// Create a ZFS boot pool (`bpool`) with a GRUB-compatible feature set.
///
/// GRUB's ZFS driver supports only a subset of pool features. The boot pool
/// restricts features to those GRUB can read, ensuring the kernel and
/// initramfs are always accessible at early boot.
fn create_zfs_boot_pool(cmd: &CommandRunner, device: &str) -> Result<()> {
    info!("Creating ZFS boot pool ({}) on {}", ZFS_BPOOL_NAME, device);

    if cmd.is_dry_run() {
        println!("  [dry-run] zpool create -f -d -o ashift=12 -o feature@... -O mountpoint=none -O compression=lz4 {} {}",
            ZFS_BPOOL_NAME, device);
        return Ok(());
    }

    // GRUB-compatible feature flags — enable only the features GRUB's ZFS
    // driver can handle. Everything else stays disabled so the boot pool
    // remains readable by the bootloader.
    cmd.run(
        "zpool",
        &[
            "create",
            "-f",
            "-d",
            "-o",
            "ashift=12",
            "-o",
            "feature@async_destroy=enabled",
            "-o",
            "feature@bookmarks=enabled",
            "-o",
            "feature@embedded_data=enabled",
            "-o",
            "feature@empty_bpobj=enabled",
            "-o",
            "feature@enabled_txg=enabled",
            "-o",
            "feature@extensible_dataset=enabled",
            "-o",
            "feature@filesystem_limits=enabled",
            "-o",
            "feature@hole_birth=enabled",
            "-o",
            "feature@large_blocks=enabled",
            "-o",
            "feature@lz4_compress=enabled",
            "-o",
            "feature@spacemap_histogram=enabled",
            "-O",
            "mountpoint=none",
            "-O",
            "compression=lz4",
            "-O",
            "atime=off",
            ZFS_BPOOL_NAME,
            device,
        ],
    )
    .map(|_| ())
    .map_err(|e| {
        DeploytixError::FilesystemError(format!("Failed to create ZFS boot pool: {}", e))
    })?;

    // Create the boot dataset with legacy mountpoint
    cmd.run(
        "zfs",
        &["create", "-o", "mountpoint=legacy", ZFS_BOOT_DATASET],
    )
    .map(|_| ())
    .map_err(|e| {
        DeploytixError::FilesystemError(format!("Failed to create ZFS boot dataset: {}", e))
    })
}

/// Standard ZFS dataset layout for a system installation.
///
/// All datasets use `mountpoint=legacy` so they can be managed via fstab.
pub const ZFS_DATASETS: &[(&str, &str)] = &[
    ("rpool/ROOT", "/"),
    ("rpool/home", "/home"),
    ("rpool/var", "/var"),
    ("rpool/var/log", "/var/log"),
];

/// Create the standard ZFS dataset hierarchy under an existing data pool.
pub fn create_zfs_datasets(cmd: &CommandRunner) -> Result<()> {
    info!("Creating ZFS datasets under {}", ZFS_RPOOL_NAME);

    if cmd.is_dry_run() {
        for (ds, mp) in ZFS_DATASETS {
            println!(
                "  [dry-run] zfs create -o mountpoint=legacy {} (→ {})",
                ds, mp
            );
        }
        return Ok(());
    }

    for (ds, mp) in ZFS_DATASETS {
        cmd.run("zfs", &["create", "-o", "mountpoint=legacy", ds])
            .map(|_| ())
            .map_err(|e| {
                DeploytixError::FilesystemError(format!(
                    "Failed to create ZFS dataset {} ({}): {}",
                    ds, mp, e
                ))
            })?;
        info!("Created dataset {} → {}", ds, mp);
    }

    Ok(())
}

/// Mount all ZFS datasets under the given install root (shallowest first).
pub fn mount_zfs_datasets(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Mounting ZFS datasets to {}", install_root);

    // ZFS_DATASETS is already ordered shallowest-first
    for (ds, mp) in ZFS_DATASETS {
        let target = if *mp == "/" {
            install_root.to_string()
        } else {
            format!("{}{}", install_root, mp)
        };

        if !cmd.is_dry_run() {
            fs::create_dir_all(&target)?;
        }

        if cmd.is_dry_run() {
            println!("  [dry-run] mount -t zfs {} {}", ds, target);
        } else {
            cmd.run("mount", &["-t", "zfs", ds, &target]).map_err(|e| {
                DeploytixError::FilesystemError(format!(
                    "Failed to mount ZFS dataset {}: {}",
                    ds, e
                ))
            })?;
            info!("Mounted {} to {}", ds, target);
        }
    }

    Ok(())
}

/// Mount the ZFS boot dataset at `<install_root>/boot`.
pub fn mount_zfs_boot(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    let target = format!("{}/boot", install_root);
    info!(
        "Mounting ZFS boot dataset {} to {}",
        ZFS_BOOT_DATASET, target
    );

    if !cmd.is_dry_run() {
        fs::create_dir_all(&target)?;
    }

    if cmd.is_dry_run() {
        println!("  [dry-run] mount -t zfs {} {}", ZFS_BOOT_DATASET, target);
        return Ok(());
    }

    cmd.run("mount", &["-t", "zfs", ZFS_BOOT_DATASET, &target])
        .map(|_| ())
        .map_err(|e| {
            DeploytixError::FilesystemError(format!("Failed to mount ZFS boot dataset: {}", e))
        })
}

/// Export (unmount) ZFS pools during cleanup.
pub fn export_zfs_pools(cmd: &CommandRunner) -> Result<()> {
    info!("Exporting ZFS pools");
    // Export boot pool first, then data pool
    let _ = cmd.run("zpool", &["export", ZFS_BPOOL_NAME]);
    let _ = cmd.run("zpool", &["export", ZFS_RPOOL_NAME]);
    Ok(())
}

/// Create btrfs filesystem on a device (typically a LUKS-mapped device)
pub fn create_btrfs_filesystem(cmd: &CommandRunner, device: &str, label: &str) -> Result<()> {
    info!(
        "Creating btrfs filesystem on {} with label {}",
        device, label
    );

    if cmd.is_dry_run() {
        println!("  [dry-run] mkfs.btrfs -f -L {} {}", label, device);
        return Ok(());
    }

    cmd.run("mkfs.btrfs", &["-f", "-L", label, device])
        .map(|_| ())
        .map_err(|e| {
            DeploytixError::FilesystemError(format!("Failed to create btrfs filesystem: {}", e))
        })
}

/// Create btrfs subvolumes on a device
///
/// Follows the canonical BTRFS subvolume setup order:
/// 1. Mount the raw BTRFS filesystem to the filesystem mountpoint
/// 2. Create each subvolume inside the mountpoint (prefixed with @)
/// 3. Unmount from the filesystem mountpoint
pub fn create_btrfs_subvolumes(
    cmd: &CommandRunner,
    device: &str,
    subvolumes: &[SubvolumeDef],
    fs_mount: &str,
    preserve_home: bool,
) -> Result<()> {
    info!(
        "Creating btrfs subvolumes on {} (mounted at {}, preserve_home: {})",
        device, fs_mount, preserve_home,
    );

    if cmd.is_dry_run() {
        println!("  [dry-run] mount {} {}", device, fs_mount);
        for sv in subvolumes {
            if preserve_home && sv.mount_point == "/home" {
                println!("  [dry-run] SKIP subvolume {} (preserve_home)", sv.name);
                continue;
            }
            println!(
                "  [dry-run] btrfs subvolume create {}/{}",
                fs_mount, sv.name
            );
        }
        println!("  [dry-run] umount {}", fs_mount);
        return Ok(());
    }

    // Create filesystem mountpoint
    fs::create_dir_all(fs_mount)?;

    // Mount the raw btrfs filesystem to its mountpoint
    cmd.run("mount", &[device, fs_mount])?;

    // When preserve_home is enabled, delete all subvolumes EXCEPT @home
    // so we get a fresh system while keeping user data.
    if preserve_home {
        info!("preserve_home: deleting existing subvolumes except @home");
        for sv in subvolumes {
            if sv.mount_point == "/home" {
                info!("preserve_home: keeping subvolume {}", sv.name);
                continue;
            }
            let subvol_path = format!("{}/{}", fs_mount, sv.name);
            // Subvolume may not exist (first install), ignore errors
            if std::path::Path::new(&subvol_path).exists() {
                let _ = cmd.run("btrfs", &["subvolume", "delete", &subvol_path]);
                info!("Deleted existing subvolume: {}", sv.name);
            }
        }
    }

    // Create each subvolume inside the filesystem mountpoint
    for sv in subvolumes {
        // Skip @home subvolume when preserve_home is enabled
        if preserve_home && sv.mount_point == "/home" {
            let subvol_path = format!("{}/{}", fs_mount, sv.name);
            if std::path::Path::new(&subvol_path).exists() {
                info!(
                    "preserve_home: subvolume {} already exists, skipping",
                    sv.name
                );
                continue;
            }
            // If @home doesn't exist, fall through to create it
            info!(
                "preserve_home: subvolume {} does not exist, creating it",
                sv.name
            );
        }

        let subvol_path = format!("{}/{}", fs_mount, sv.name);
        cmd.run("btrfs", &["subvolume", "create", &subvol_path])
            .map_err(|e| {
                DeploytixError::FilesystemError(format!(
                    "Failed to create subvolume {}: {}",
                    sv.name, e
                ))
            })?;
        info!("Created subvolume: {}", sv.name);
    }

    // Unmount from filesystem mountpoint
    cmd.run("umount", &[fs_mount])?;

    info!("All subvolumes created successfully");
    Ok(())
}

/// Mount btrfs subvolumes for installation
pub fn mount_btrfs_subvolumes(
    cmd: &CommandRunner,
    device: &str,
    subvolumes: &[SubvolumeDef],
    install_root: &str,
) -> Result<()> {
    info!("Mounting btrfs subvolumes to {}", install_root);

    if cmd.is_dry_run() {
        for sv in subvolumes {
            println!(
                "  [dry-run] mount -o subvol={},{} {} {}{}",
                sv.name, sv.mount_options, device, install_root, sv.mount_point
            );
        }
        return Ok(());
    }

    // Sort subvolumes by mount point depth (root first)
    let mut sorted_subvolumes = subvolumes.to_vec();
    sorted_subvolumes.sort_by(|a, b| {
        a.mount_point
            .matches('/')
            .count()
            .cmp(&b.mount_point.matches('/').count())
    });

    for sv in &sorted_subvolumes {
        let target = format!("{}{}", install_root, sv.mount_point);
        fs::create_dir_all(&target)?;

        let options = format!("subvol={},{}", sv.name, sv.mount_options);
        cmd.run("mount", &["-o", &options, device, &target])
            .map_err(|e| {
                DeploytixError::FilesystemError(format!("Failed to mount {}: {}", sv.name, e))
            })?;
        info!("Mounted {} to {}", sv.name, target);
    }

    info!("All subvolumes mounted successfully");
    Ok(())
}
