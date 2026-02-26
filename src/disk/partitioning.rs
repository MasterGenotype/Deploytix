//! Partition creation and management

use crate::disk::detection::{get_device_info, partition_path};
use crate::disk::layouts::{ComputedLayout, PartitionDef};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::fs;
use std::io::Write;
use tracing::info;
use uuid::Uuid;

/// Generate sfdisk script for a partition layout
pub fn generate_sfdisk_script(device: &str, layout: &ComputedLayout) -> Result<String> {
    let device_info = get_device_info(device).map_err(|e| {
        DeploytixError::PartitionError(format!(
            "Cannot read device info for {}: {}",
            device, e
        ))
    })?;
    let sector_size = 512u64; // Default, could be read from sysfs
    let total_sectors = device_info.size_bytes / sector_size;

    let first_lba = 2048u64;
    let last_lba = total_sectors.saturating_sub(34);

    let label_id = Uuid::new_v4();

    let mut script = String::new();
    script.push_str("label: gpt\n");
    script.push_str(&format!("label-id: {}\n", label_id));
    script.push_str(&format!("device: {}\n", device));
    script.push_str("unit: sectors\n");
    script.push_str(&format!("first-lba: {}\n", first_lba));
    script.push_str(&format!("last-lba: {}\n", last_lba));
    script.push_str(&format!("sector-size: {}\n", sector_size));
    script.push('\n');

    let align_sectors = (1024 * 1024) / sector_size; // 1 MiB alignment
    let mut current_sector = first_lba;

    for (i, part) in layout.partitions.iter().enumerate() {
        let part_uuid = Uuid::new_v4();
        let part_path = partition_path(device, part.number);

        // Calculate size in sectors
        let size_sectors = if part.size_mib == 0 {
            // Remainder - use all remaining space
            last_lba - current_sector + 1
        } else {
            (part.size_mib * 1024 * 1024) / sector_size
        };

        // Build partition line
        let mut line = format!(
            "{} : start={}, size={}, type={}, uuid={}, name=\"{}\"",
            part_path, current_sector, size_sectors, part.type_guid, part_uuid, part.name
        );

        // Add the Legacy BIOS Bootable attribute when flagged (fdisk/sfdisk
        // expert-mode "bootable" toggle — GPT attribute bit 2).  This is what
        // tells the firmware which partition to use for BIOS booting on GPT
        // disks and is separate from any filesystem placed on the partition.
        if part.is_bios_boot {
            line.push_str(", attrs=\"LegacyBIOSBootable\"");
        } else if let Some(ref attrs) = part.attributes {
            line.push_str(&format!(", attrs=\"{}\"", attrs));
        }

        script.push_str(&line);
        script.push('\n');

        // Update position for next partition (aligned)
        if i < layout.partitions.len() - 1 {
            let next_sector = current_sector + size_sectors;
            current_sector = next_sector.div_ceil(align_sectors) * align_sectors;
        }
    }

    Ok(script)
}

/// Apply partition layout to a disk using sfdisk
pub fn apply_partitions(cmd: &CommandRunner, device: &str, layout: &ComputedLayout) -> Result<()> {
    info!(
        "Applying {} partition layout to {}",
        layout.partitions.len(),
        device
    );

    // Generate sfdisk script
    let script = generate_sfdisk_script(device, layout)?;

    if cmd.is_dry_run() {
        println!("  [dry-run] Would apply sfdisk script:");
        for line in script.lines() {
            println!("    {}", line);
        }
        return Ok(());
    }

    // Write script to temp file
    let script_path = "/tmp/deploytix/partition_script";
    fs::create_dir_all("/tmp/deploytix")?;
    let mut file = fs::File::create(script_path)?;
    file.write_all(script.as_bytes())?;
    drop(file);

    // Wipe existing partition table
    info!("Wiping existing partition table on {}...", device);
    let _ = cmd.run("wipefs", &["-a", device]);

    // Apply with sfdisk - pipe script via stdin from file
    info!("Writing new GPT partition table to {}...", device);
    let result = std::process::Command::new("sfdisk")
        .arg(device)
        .stdin(fs::File::open(script_path)?)
        .output()
        .map_err(|e| DeploytixError::CommandFailed {
            command: "sfdisk".to_string(),
            stderr: e.to_string(),
        })?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(DeploytixError::PartitionError(format!(
            "sfdisk failed: {}",
            stderr
        )));
    }

    // Notify kernel of partition changes
    info!(
        "Notifying kernel of partition table changes on {}...",
        device
    );
    let _ = cmd.run("partprobe", &[device]);
    let _ = cmd.run("udevadm", &["settle"]);

    // Clean up
    let _ = fs::remove_file(script_path);

    info!(
        "Partitioning of {} complete ({} partitions created)",
        device,
        layout.partitions.len()
    );
    Ok(())
}

/// Get list of partition paths for a layout
#[allow(dead_code)]
pub fn get_partition_paths(device: &str, layout: &ComputedLayout) -> Vec<(PartitionDef, String)> {
    layout
        .partitions
        .iter()
        .map(|p| (p.clone(), partition_path(device, p.number)))
        .collect()
}

/// Wipe partition table from a device
#[allow(dead_code)]
pub fn wipe_partition_table(cmd: &CommandRunner, device: &str) -> Result<()> {
    info!("Wiping partition table on {}", device);

    cmd.run("wipefs", &["-a", device])?;

    // Also zero the first and last MB to ensure clean state
    if !cmd.is_dry_run() {
        let _ = std::process::Command::new("dd")
            .args([
                "if=/dev/zero",
                &format!("of={}", device),
                "bs=1M",
                "count=1",
                "conv=notrunc",
            ])
            .output();
    }

    Ok(())
}
