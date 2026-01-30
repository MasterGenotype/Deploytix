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

/// Format all partitions according to the layout
pub fn format_all_partitions(
    cmd: &CommandRunner,
    device: &str,
    layout: &ComputedLayout,
    filesystem: &Filesystem,
) -> Result<()> {
    info!("Formatting all partitions on {}", device);

    for part in &layout.partitions {
        let part_path = partition_path(device, part.number);

        if part.is_efi {
            format_efi(cmd, &part_path)?;
        } else if part.is_swap {
            format_swap(cmd, &part_path, Some(&part.name))?;
        } else if part.is_luks {
            // LUKS partitions are handled separately by encryption module
            info!("Skipping LUKS partition {} (handled by encryption module)", part_path);
        } else if part.is_bios_boot {
            // BIOS Boot partition is never formatted
            info!("Skipping BIOS Boot partition {} (no filesystem)", part_path);
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

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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

/// Create btrfs filesystem on a device (typically a LUKS-mapped device)
pub fn create_btrfs_filesystem(
    cmd: &CommandRunner,
    device: &str,
    label: &str,
) -> Result<()> {
    info!("Creating btrfs filesystem on {} with label {}", device, label);

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
pub fn create_btrfs_subvolumes(
    cmd: &CommandRunner,
    device: &str,
    subvolumes: &[SubvolumeDef],
    temp_mount: &str,
) -> Result<()> {
    info!("Creating btrfs subvolumes on {}", device);

    if cmd.is_dry_run() {
        println!("  [dry-run] mount {} {}", device, temp_mount);
        for sv in subvolumes {
            println!("  [dry-run] btrfs subvolume create {}/{}", temp_mount, sv.name);
        }
        println!("  [dry-run] umount {}", temp_mount);
        return Ok(());
    }

    // Create temp mount point
    fs::create_dir_all(temp_mount)?;

    // Mount the btrfs root
    cmd.run("mount", &[device, temp_mount])?;

    // Create each subvolume
    for sv in subvolumes {
        let subvol_path = format!("{}/{}", temp_mount, sv.name);
        cmd.run("btrfs", &["subvolume", "create", &subvol_path])
            .map_err(|e| {
                DeploytixError::FilesystemError(format!("Failed to create subvolume {}: {}", sv.name, e))
            })?;
        info!("Created subvolume: {}", sv.name);
    }

    // Unmount
    cmd.run("umount", &[temp_mount])?;

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
        a.mount_point.matches('/').count()
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
