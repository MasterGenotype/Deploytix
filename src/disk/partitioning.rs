//! Partition creation and management

use crate::disk::detection::{get_device_info, partition_path};
use crate::disk::layouts::{extents_overlap, ComputedLayout, PartitionDef};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::fs;
use std::io::Write;
use tracing::info;
use uuid::Uuid;

/// Read the logical block size of a device from sysfs.
///
/// Returns the value from `/sys/block/<name>/queue/logical_block_size`,
/// which sfdisk uses to interpret the sector counts in a partition script.
/// Falls back to 512 when the attribute is unavailable (virtual devices,
/// older kernels).
///
/// Note: `/sys/block/<dev>/size` always reports capacity in 512-byte units
/// regardless of the logical block size, so `size_bytes = size_sectors * 512`
/// remains correct. Only the sector counts and alignment in the sfdisk
/// script need to use the actual logical block size.
fn logical_sector_size(device: &str) -> u64 {
    let name = std::path::Path::new(device)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let path = format!("/sys/block/{}/queue/logical_block_size", name);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(512)
}

/// Generate sfdisk script for a partition layout
pub fn generate_sfdisk_script(device: &str, layout: &ComputedLayout) -> Result<String> {
    let device_info = get_device_info(device).map_err(|e| {
        DeploytixError::PartitionError(format!("Cannot read device info for {}: {}", device, e))
    })?;
    // Read the actual logical sector size from sysfs so that 4096-byte-sector
    // NVMe drives get correct sector counts and alignment in the script.
    // /sys/block/<dev>/size always reports capacity in 512-byte units, so
    // size_bytes is still computed as size_sectors * 512.
    let sector_size = logical_sector_size(device);
    let total_sectors = device_info.size_bytes / sector_size;

    build_sfdisk_script(device, layout, sector_size, total_sectors)
}

/// Build the sfdisk script for a layout on a device of a known geometry.
///
/// Split from [`generate_sfdisk_script`] so it can be tested without a real
/// block device: the caller supplies the sector size and capacity that would
/// otherwise come from sysfs.
pub fn build_sfdisk_script(
    device: &str,
    layout: &ComputedLayout,
    sector_size: u64,
    total_sectors: u64,
) -> Result<String> {
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
    // (name, start, size, is_pinned) for every partition as actually placed.
    let mut placements: Vec<(&str, u64, u64, bool)> = Vec::new();

    for (i, part) in layout.partitions.iter().enumerate() {
        let part_path = partition_path(device, part.number);

        // A pinned partition is preserved from the table already on the
        // disk: it keeps its original extent and partition UUID so that
        // rewriting the table leaves its contents alone. Everything else is
        // placed sequentially from the running cursor.
        let (start_sector, size_sectors, part_uuid) = match &part.pinned {
            Some(pin) => (
                pin.start_sector,
                pin.size_sectors,
                pin.part_uuid
                    .clone()
                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
            ),
            None => {
                let size = if part.size_mib == 0 {
                    // Remainder — use all remaining space
                    last_lba - current_sector + 1
                } else {
                    (part.size_mib * 1024 * 1024) / sector_size
                };
                (current_sector, size, Uuid::new_v4().to_string())
            }
        };

        // Build partition line
        let mut line = format!(
            "{} : start={}, size={}, type={}, uuid={}, name=\"{}\"",
            part_path, start_sector, size_sectors, part.type_guid, part_uuid, part.name
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

        placements.push((
            part.name.as_str(),
            start_sector,
            size_sectors,
            part.pinned.is_some(),
        ));

        // Advance the cursor past whatever this partition occupies, pinned
        // or not, so a following unpinned partition is never placed on top
        // of a preserved one.
        if i < layout.partitions.len() - 1 {
            let next_sector = start_sector + size_sectors;
            current_sector = next_sector.div_ceil(align_sectors) * align_sectors;
        }
    }

    validate_placements(&placements, first_lba, last_lba)?;

    Ok(script)
}

/// Reject a layout in which any two partitions would occupy the same sectors,
/// or in which a partition falls outside the disk's usable range.
///
/// This is the safety net for a recovery install: the sequential allocator
/// places unpinned partitions from a running cursor, so growing ROOT in the
/// config can walk it straight over a preserved HOME sitting later on the
/// disk. Writing that table and running the first `mkfs` would destroy the
/// data the recovery install exists to keep, so it is refused here — before
/// anything is written.
fn validate_placements(
    placements: &[(&str, u64, u64, bool)],
    first_lba: u64,
    last_lba: u64,
) -> Result<()> {
    for (name, start, size, pinned) in placements {
        if *size == 0 {
            return Err(DeploytixError::PartitionError(format!(
                "partition {} was placed with zero length",
                name
            )));
        }
        if *start < first_lba || start + size - 1 > last_lba {
            return Err(DeploytixError::PartitionError(format!(
                "partition {} ({}..{}) falls outside the disk's usable range {}..{}{}",
                name,
                start,
                start + size - 1,
                first_lba,
                last_lba,
                if *pinned {
                    " — the preserved extent does not fit this disk"
                } else {
                    ""
                },
            )));
        }
    }

    for (i, (a_name, a_start, a_size, a_pinned)) in placements.iter().enumerate() {
        for (b_name, b_start, b_size, b_pinned) in placements.iter().skip(i + 1) {
            if !extents_overlap(*a_start, *a_size, *b_start, *b_size) {
                continue;
            }
            let preserved = if *a_pinned || *b_pinned {
                " — this would overwrite a partition the install was told to preserve"
            } else {
                ""
            };
            return Err(DeploytixError::PartitionError(format!(
                "partitions {} ({}+{}) and {} ({}+{}) overlap{}",
                a_name, a_start, a_size, b_name, b_start, b_size, preserved,
            )));
        }
    }

    Ok(())
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

    // Clear filesystem signatures.
    //
    // With nothing pinned this is the whole device, as it always was. With a
    // pinned partition, wiping the device would take the GPT that carries
    // the very extent being preserved with it, so instead each non-pinned
    // partition is wiped individually and the preserved one is left alone.
    // sfdisk then rewrites the table with the pinned entry at its original
    // start, size and UUID, which is a no-op for that partition's contents.
    let pinned: Vec<&PartitionDef> = layout
        .partitions
        .iter()
        .filter(|p| p.pinned.is_some())
        .collect();

    if pinned.is_empty() {
        info!("Wiping existing partition table on {}...", device);
        let _ = cmd.run("wipefs", &["-a", device]);
    } else {
        let preserved: Vec<&str> = pinned.iter().map(|p| p.name.as_str()).collect();
        info!(
            "Preserving {} on {}; wiping the other partitions individually",
            preserved.join(", "),
            device
        );
        for part in &layout.partitions {
            if part.pinned.is_some() {
                continue;
            }
            let part_path = partition_path(device, part.number);
            if !std::path::Path::new(&part_path).exists() {
                // Nothing there yet — a partition this run is adding.
                continue;
            }
            info!("Wiping signatures on {} ({})", part_path, part.name);
            let _ = cmd.run("wipefs", &["-a", &part_path]);
        }
    }

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
        // Zero first MB
        let _ = cmd.run(
            "dd",
            &[
                "if=/dev/zero",
                &format!("of={}", device),
                "bs=1M",
                "count=1",
                "conv=notrunc",
            ],
        );

        // Zero last MB (removes stale backup GPT headers)
        let device_info = get_device_info(device)?;
        if device_info.size_bytes > 1024 * 1024 {
            let last_mb_offset = (device_info.size_bytes / (1024 * 1024)) - 1;
            let _ = cmd.run(
                "dd",
                &[
                    "if=/dev/zero",
                    &format!("of={}", device),
                    "bs=1M",
                    "count=1",
                    &format!("seek={}", last_mb_offset),
                    "conv=notrunc",
                ],
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::layouts::{partition_types, PinnedExtent};

    fn part(number: u32, name: &str, size_mib: u64) -> PartitionDef {
        PartitionDef {
            number,
            name: name.to_string(),
            size_mib,
            type_guid: partition_types::LINUX_FILESYSTEM.to_string(),
            mount_point: None,
            is_swap: false,
            is_efi: false,
            is_luks: false,
            is_bios_boot: false,
            is_boot_fs: false,
            attributes: None,
            subvolume_name: None,
            pinned: None,
        }
    }

    fn layout(partitions: Vec<PartitionDef>) -> ComputedLayout {
        ComputedLayout {
            partitions,
            total_mib: 8192,
            subvolumes: None,
            planned_thin_volumes: None,
        }
    }

    /// 8 GiB at 512-byte sectors.
    const TOTAL_SECTORS: u64 = 8 * 1024 * 1024 * 1024 / 512;

    fn build(l: &ComputedLayout) -> Result<String> {
        build_sfdisk_script("/dev/sdz", l, 512, TOTAL_SECTORS)
    }

    #[test]
    fn unpinned_partitions_are_placed_sequentially() {
        let l = layout(vec![part(1, "EFI", 512), part(2, "ROOT", 1024)]);
        let script = build(&l).unwrap();
        assert!(script.contains("/dev/sdz1 : start=2048, size=1048576"));
        // 2048 + 1048576 = 1050624, already 1 MiB aligned.
        assert!(script.contains("/dev/sdz2 : start=1050624, size=2097152"));
    }

    /// The whole point of a pin: the preserved partition comes out at exactly
    /// the extent it already occupies, with its original UUID.
    #[test]
    fn a_pinned_partition_keeps_its_extent_and_uuid() {
        let mut home = part(2, "HOME", 0);
        home.pinned = Some(PinnedExtent {
            start_sector: 4_000_000,
            size_sectors: 8_000_000,
            part_uuid: Some("DEADBEEF-0000-0000-0000-00000000CAFE".to_string()),
        });
        let l = layout(vec![part(1, "ROOT", 1024), home]);
        let script = build(&l).unwrap();
        assert!(
            script.contains(
                "/dev/sdz2 : start=4000000, size=8000000, \
                 type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, \
                 uuid=DEADBEEF-0000-0000-0000-00000000CAFE"
            ),
            "pinned entry not emitted verbatim:\n{}",
            script
        );
    }

    /// A pinned partition with no recorded UUID still gets a valid one rather
    /// than an empty field.
    #[test]
    fn a_pinned_partition_without_a_uuid_gets_a_fresh_one() {
        let mut home = part(2, "HOME", 0);
        home.pinned = Some(PinnedExtent {
            start_sector: 4_000_000,
            size_sectors: 8_000_000,
            part_uuid: None,
        });
        let l = layout(vec![part(1, "ROOT", 1024), home]);
        let script = build(&l).unwrap();
        assert!(script.contains("start=4000000, size=8000000"));
        assert!(!script.contains("uuid=,"));
    }

    /// The regression this whole safety net exists for: ROOT is grown until
    /// the sequential allocator would place it across the preserved HOME.
    /// Writing that table and running mkfs would destroy the data.
    #[test]
    fn refuses_a_layout_that_would_overwrite_a_preserved_partition() {
        let mut home = part(2, "HOME", 0);
        home.pinned = Some(PinnedExtent {
            start_sector: 1_000_000,
            size_sectors: 8_000_000,
            part_uuid: None,
        });
        // ROOT starts at 2048 and runs 4 GiB = 8388608 sectors, well past
        // HOME's start at 1_000_000.
        let l = layout(vec![part(1, "ROOT", 4096), home]);
        let err = build(&l).unwrap_err().to_string();
        assert!(
            err.contains("overlap") && err.contains("preserve"),
            "unexpected error: {}",
            err
        );
    }

    /// A partition placed after a pinned one must clear it, not restart from
    /// wherever the cursor was before the pin.
    #[test]
    fn the_cursor_advances_past_a_pinned_partition() {
        let mut home = part(2, "HOME", 0);
        home.pinned = Some(PinnedExtent {
            start_sector: 1_000_000,
            size_sectors: 1_000_000,
            part_uuid: None,
        });
        let l = layout(vec![part(1, "ROOT", 100), home, part(3, "DATA", 100)]);
        let script = build(&l).unwrap();
        // 1_000_000 + 1_000_000 = 2_000_000, aligned up to a 2048 boundary.
        assert!(
            script.contains("/dev/sdz3 : start=2000896"),
            "DATA not placed after the pin:\n{}",
            script
        );
    }

    /// A preserved extent from a larger disk cannot be honoured here.
    #[test]
    fn refuses_a_pinned_extent_that_does_not_fit_the_disk() {
        let mut home = part(2, "HOME", 0);
        home.pinned = Some(PinnedExtent {
            start_sector: TOTAL_SECTORS - 1000,
            size_sectors: 500_000,
            part_uuid: None,
        });
        let l = layout(vec![part(1, "ROOT", 1024), home]);
        let err = build(&l).unwrap_err().to_string();
        assert!(
            err.contains("outside the disk's usable range"),
            "unexpected error: {}",
            err
        );
    }

    /// Nothing pinned means nothing changes for an ordinary install.
    #[test]
    fn an_ordinary_layout_still_validates() {
        let l = layout(vec![
            part(1, "EFI", 512),
            part(2, "BOOT", 2048),
            part(3, "ROOT", 0),
        ]);
        assert!(build(&l).is_ok());
    }
}
