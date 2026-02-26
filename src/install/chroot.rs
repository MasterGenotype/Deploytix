//! Chroot and mount operations

use crate::disk::detection::partition_path;
use crate::disk::formatting::{create_btrfs_subvolumes, mount_btrfs_subvolumes};
use crate::disk::layouts::ComputedLayout;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use tracing::{info, warn};

/// Mount all partitions according to the layout
/// Handles both regular partitions and btrfs subvolume layouts
pub fn mount_partitions(
    cmd: &CommandRunner,
    device: &str,
    layout: &ComputedLayout,
    install_root: &str,
) -> Result<()> {
    // Check if this layout uses btrfs subvolumes
    if layout.uses_subvolumes() {
        return mount_partitions_with_subvolumes(cmd, device, layout, install_root);
    }

    info!(
        "Mounting {} partitions from {} to {}",
        layout.partitions.len(),
        device,
        install_root
    );

    // Sort partitions by mount point depth (root first, then deeper paths)
    let mut mount_order: Vec<_> = layout
        .partitions
        .iter()
        .filter(|p| p.mount_point.is_some() && !p.is_swap)
        .collect();

    mount_order.sort_by_key(|p| {
        let mp = p.mount_point.as_ref().unwrap();
        mp.matches('/').count()
    });

    // Mount each partition
    for part in mount_order {
        let part_path = partition_path(device, part.number);
        let mount_point = part.mount_point.as_ref().unwrap();

        // Construct full mount path
        let full_mount = if mount_point == "/" {
            install_root.to_string()
        } else {
            format!("{}{}", install_root, mount_point)
        };

        // Create mount point directory
        if !cmd.is_dry_run() {
            std::fs::create_dir_all(&full_mount)?;
        }

        info!("Mounting {} to {}", part_path, full_mount);
        cmd.run("mount", &[&part_path, &full_mount])?;
    }

    // Enable swap partitions
    for part in &layout.partitions {
        if part.is_swap {
            let part_path = partition_path(device, part.number);
            info!("Enabling swap on {}", part_path);
            cmd.run("swapon", &[&part_path])?;
        }
    }

    Ok(())
}

/// Mount partitions for layouts using btrfs subvolumes
/// Creates subvolumes on the ROOT partition and mounts them
fn mount_partitions_with_subvolumes(
    cmd: &CommandRunner,
    device: &str,
    layout: &ComputedLayout,
    install_root: &str,
) -> Result<()> {
    let subvolumes = layout.subvolumes.as_ref().ok_or_else(|| {
        crate::utils::error::DeploytixError::ConfigError(
            "Layout reports subvolumes in use but subvolumes field is None".to_string(),
        )
    })?;
    info!("Setting up btrfs subvolumes on {} (root partition)", device);

    // Find the ROOT partition
    let root_part = layout
        .partitions
        .iter()
        .find(|p| p.name == "ROOT")
        .ok_or_else(|| {
            crate::utils::error::DeploytixError::ConfigError(
                "No ROOT partition found for subvolume layout".to_string(),
            )
        })?;

    let root_path = partition_path(device, root_part.number);

    // Include the process ID in the temp path so concurrent installer instances
    // (e.g., integration tests) don't clobber each other's mounts.
    let temp_mount = format!("/tmp/deploytix_btrfs_setup_{}", std::process::id());
    create_btrfs_subvolumes(cmd, &root_path, subvolumes, &temp_mount)?;

    // Now mount the subvolumes to their final locations
    mount_btrfs_subvolumes(cmd, &root_path, subvolumes, install_root)?;

    // Mount other non-subvolume partitions (EFI, etc.)
    for part in &layout.partitions {
        if part.name == "ROOT" {
            continue; // Already handled via subvolumes
        }

        if part.is_swap {
            let part_path = partition_path(device, part.number);
            info!("Enabling swap on {}", part_path);
            cmd.run("swapon", &[&part_path])?;
        } else if let Some(ref mount_point) = part.mount_point {
            let part_path = partition_path(device, part.number);
            let full_mount = format!("{}{}", install_root, mount_point);

            if !cmd.is_dry_run() {
                std::fs::create_dir_all(&full_mount)?;
            }

            info!("Mounting {} to {}", part_path, full_mount);
            cmd.run("mount", &[&part_path, &full_mount])?;
        }
    }

    Ok(())
}

/// Unmount all partitions
pub fn unmount_all(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Unmounting all partitions from {}", install_root);

    // Disable swap
    let _ = cmd.run("swapoff", &["-a"]);

    // Get list of mounted filesystems under install_root
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
    let mut mount_points: Vec<&str> = mounts
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[1].starts_with(install_root) {
                Some(parts[1])
            } else {
                None
            }
        })
        .collect();

    // Sort by depth (deepest first for proper unmounting)
    mount_points.sort_by_key(|b| std::cmp::Reverse(b.matches('/').count()));

    // Unmount each
    for mp in mount_points {
        info!("Unmounting {}", mp);
        if let Err(e) = cmd.run("umount", &[mp]) {
            warn!("Failed to unmount {}: {} (trying lazy unmount)", mp, e);
            if let Err(e2) = cmd.run("umount", &["-l", mp]) {
                warn!("Lazy unmount also failed for {}: {}", mp, e2);
            }
        }
    }

    Ok(())
}

/// Bind mount necessary directories for chroot
#[allow(dead_code)]
pub fn setup_chroot_mounts(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    let binds = [
        ("/dev", "dev"),
        ("/dev/pts", "dev/pts"),
        ("/proc", "proc"),
        ("/sys", "sys"),
        ("/run", "run"),
    ];

    for (src, dst) in binds {
        let target = format!("{}/{}", install_root, dst);
        if !cmd.is_dry_run() {
            std::fs::create_dir_all(&target)?;
        }

        // Check if already mounted
        let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
        if mounts.contains(&target) {
            continue;
        }

        cmd.run("mount", &["--bind", src, &target])?;
    }

    Ok(())
}

/// Clean up chroot mounts
#[allow(dead_code)]
pub fn cleanup_chroot_mounts(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    let targets = ["run", "sys", "proc", "dev/pts", "dev"];

    for dst in targets {
        let target = format!("{}/{}", install_root, dst);
        let _ = cmd.run("umount", &["-l", &target]);
    }

    Ok(())
}
