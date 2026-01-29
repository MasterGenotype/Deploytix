//! Chroot and mount operations

use crate::disk::detection::partition_path;
use crate::disk::layouts::ComputedLayout;
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::path::Path;
use tracing::info;

/// Mount all partitions according to the layout
pub fn mount_partitions(
    cmd: &CommandRunner,
    device: &str,
    layout: &ComputedLayout,
    install_root: &str,
) -> Result<()> {
    info!("Mounting partitions to {}", install_root);

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
    mount_points.sort_by(|a, b| b.matches('/').count().cmp(&a.matches('/').count()));

    // Unmount each
    for mp in mount_points {
        info!("Unmounting {}", mp);
        let _ = cmd.run("umount", &[mp]);
    }

    Ok(())
}

/// Bind mount necessary directories for chroot
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
pub fn cleanup_chroot_mounts(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    let targets = ["run", "sys", "proc", "dev/pts", "dev"];

    for dst in targets {
        let target = format!("{}/{}", install_root, dst);
        let _ = cmd.run("umount", &["-l", &target]);
    }

    Ok(())
}
