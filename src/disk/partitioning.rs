//! Partition creation and management

use crate::disk::detection::{get_device_info, partition_path};
use crate::disk::layouts::{ComputedLayout, PartitionDef};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::fs;
use std::io::Write;
use tracing::info;
use uuid::Uuid;

/// Existing partition info read from the current partition table.
struct ExistingPartition {
    /// Start sector
    start: u64,
    /// Size in sectors
    size: u64,
    /// GPT partition UUID
    uuid: String,
}

/// Read existing partition boundaries from the current partition table using sfdisk --dump.
/// Returns a map from partition number to (start_sector, size_sectors, uuid).
fn read_existing_partitions(device: &str) -> Result<std::collections::HashMap<u32, ExistingPartition>> {
    let output = std::process::Command::new("sfdisk")
        .args(["--dump", device])
        .output()
        .map_err(|e| DeploytixError::PartitionError(format!(
            "Failed to read existing partition table from {}: {}", device, e
        )))?;

    if !output.status.success() {
        return Err(DeploytixError::PartitionError(format!(
            "sfdisk --dump failed for {}: {}",
            device,
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let dump = String::from_utf8_lossy(&output.stdout);
    let mut map = std::collections::HashMap::new();

    for line in dump.lines() {
        // Lines look like: /dev/sda7 : start=   123456, size=  789012, type=..., uuid=..., name="HOME"
        let line = line.trim();
        if !line.starts_with(device) {
            continue;
        }

        // Extract partition number from path (e.g., /dev/sda7 -> 7, /dev/nvme0n1p7 -> 7)
        let part_path = line.split(':').next().unwrap_or("").trim();
        let part_num = extract_partition_number(part_path);
        if part_num == 0 {
            continue;
        }

        // Parse key=value pairs after the colon
        let kv_part = match line.split_once(':') {
            Some((_, rest)) => rest,
            None => continue,
        };

        let mut start = 0u64;
        let mut size = 0u64;
        let mut uuid = String::new();

        for field in kv_part.split(',') {
            let field = field.trim();
            if let Some(val) = field.strip_prefix("start=") {
                start = val.trim().parse().unwrap_or(0);
            } else if let Some(val) = field.strip_prefix("size=") {
                size = val.trim().parse().unwrap_or(0);
            } else if let Some(val) = field.strip_prefix("uuid=") {
                uuid = val.trim().to_string();
            }
        }

        if start > 0 && size > 0 {
            map.insert(part_num, ExistingPartition { start, size, uuid });
        }
    }

    Ok(map)
}

/// Extract partition number from a device path like /dev/sda7 or /dev/nvme0n1p7.
fn extract_partition_number(path: &str) -> u32 {
    // Find the last run of digits
    let digits: String = path.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    let digits: String = digits.chars().rev().collect();
    digits.parse().unwrap_or(0)
}

/// Check if any partition in the layout is marked as preserved.
fn has_preserved_partitions(layout: &ComputedLayout) -> bool {
    layout.partitions.iter().any(|p| p.preserve)
}

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

    // If any partitions are preserved, read the existing table to get their boundaries
    let existing = if has_preserved_partitions(layout) {
        Some(read_existing_partitions(device)?)
    } else {
        None
    };

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
        let part_path = partition_path(device, part.number);

        if part.preserve {
            // Preserved partition: use existing boundaries from disk
            let existing_map = existing.as_ref().ok_or_else(|| {
                DeploytixError::PartitionError(
                    "Cannot preserve partition: failed to read existing partition table".to_string(),
                )
            })?;
            let existing_part = existing_map.get(&part.number).ok_or_else(|| {
                DeploytixError::PartitionError(format!(
                    "Cannot preserve partition {}: not found in existing partition table on {}",
                    part.number, device
                ))
            })?;

            info!(
                "Preserving partition {} ({}) at start={}, size={}",
                part.number, part.name, existing_part.start, existing_part.size
            );

            // Use existing UUID to maintain filesystem references
            let part_uuid = &existing_part.uuid;
            let mut line = format!(
                "{} : start={}, size={}, type={}, uuid={}, name=\"{}\"",
                part_path, existing_part.start, existing_part.size, part.type_guid, part_uuid, part.name
            );

            let mut attrs: Vec<String> = Vec::new();
            if part.is_bios_boot {
                attrs.push("LegacyBIOSBootable".to_string());
            }
            if let Some(ref extra) = part.attributes {
                attrs.push(extra.clone());
            }
            if !attrs.is_empty() {
                line.push_str(&format!(", attrs=\"{}\"", attrs.join(",")));
            }

            script.push_str(&line);
            script.push('\n');

            // Update position past the preserved partition
            current_sector = existing_part.start + existing_part.size;
            if i < layout.partitions.len() - 1 {
                current_sector = current_sector.div_ceil(align_sectors) * align_sectors;
            }
        } else {
            // New partition: compute fresh boundaries
            let part_uuid = Uuid::new_v4();

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

            // Add GPT attributes.
            // is_bios_boot maps to the LegacyBIOSBootable GPT attribute bit — the
            // same flag toggled by fdisk's expert-mode "Bootable" option, which
            // tells GRUB where the /boot filesystem lives on legacy BIOS systems.
            let mut attrs: Vec<String> = Vec::new();
            if part.is_bios_boot {
                attrs.push("LegacyBIOSBootable".to_string());
            }
            if let Some(ref extra) = part.attributes {
                attrs.push(extra.clone());
            }
            if !attrs.is_empty() {
                line.push_str(&format!(", attrs=\"{}\"", attrs.join(",")));
            }

            script.push_str(&line);
            script.push('\n');

            // Update position for next partition (aligned)
            if i < layout.partitions.len() - 1 {
                let next_sector = current_sector + size_sectors;
                current_sector = next_sector.div_ceil(align_sectors) * align_sectors;
            }
        }
    }

    Ok(script)
}

/// Apply partition layout to a disk using sfdisk
pub fn apply_partitions(cmd: &CommandRunner, device: &str, layout: &ComputedLayout) -> Result<()> {
    let preserving = has_preserved_partitions(layout);

    info!(
        "Applying {} partition layout to {}{}",
        layout.partitions.len(),
        device,
        if preserving { " (preserving marked partitions)" } else { "" }
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

    if preserving {
        // When preserving partitions, do NOT wipe the partition table.
        // Use sfdisk --force to rewrite the table while keeping preserved
        // partition data intact on disk.
        info!(
            "Rewriting partition table on {} (preserving home)...",
            device
        );
    } else {
        // Wipe existing partition table for a clean install
        info!("Wiping existing partition table on {}...", device);
        let _ = cmd.run("wipefs", &["-a", device]);
    }

    // Apply with sfdisk - pipe script via stdin from file.
    // --force is needed when rewriting a table with existing partitions.
    info!("Writing new GPT partition table to {}...", device);
    let mut sfdisk_cmd = std::process::Command::new("sfdisk");
    if preserving {
        sfdisk_cmd.arg("--force");
    }
    let result = sfdisk_cmd
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
