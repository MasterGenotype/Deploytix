//! Filesystem formatting

use crate::config::Filesystem;
use crate::disk::detection::partition_path;
use crate::disk::layouts::ComputedLayout;
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
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
