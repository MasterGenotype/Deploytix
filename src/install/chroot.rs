//! Chroot and mount operations

use crate::config::Filesystem;
use crate::disk::detection::partition_path;
use crate::disk::formatting::{
    create_btrfs_subvolumes, create_zfs_datasets, create_zfs_pool, mount_btrfs_subvolumes,
    mount_zfs_boot, mount_zfs_datasets,
};
use crate::disk::layouts::ComputedLayout;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use tracing::{info, warn};

/// Mount all partitions according to the layout
/// Handles regular partitions, btrfs subvolume layouts, and ZFS dataset layouts
pub fn mount_partitions(
    cmd: &CommandRunner,
    device: &str,
    layout: &ComputedLayout,
    install_root: &str,
) -> Result<()> {
    mount_partitions_inner(cmd, device, layout, install_root, false)
}

/// Mount all partitions with preserve_home support
pub fn mount_partitions_preserve(
    cmd: &CommandRunner,
    device: &str,
    layout: &ComputedLayout,
    install_root: &str,
    preserve_home: bool,
) -> Result<()> {
    mount_partitions_inner(cmd, device, layout, install_root, preserve_home)
}

fn mount_partitions_inner(
    cmd: &CommandRunner,
    device: &str,
    layout: &ComputedLayout,
    install_root: &str,
    preserve_home: bool,
) -> Result<()> {
    // Check if this layout uses btrfs subvolumes
    if layout.uses_subvolumes() {
        return mount_partitions_with_subvolumes(cmd, device, layout, install_root, preserve_home);
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

/// Format and mount a ZFS layout.
///
/// Creates the data pool and datasets, mounts them via legacy mountpoints,
/// then mounts EFI, boot, and swap normally.
pub fn mount_partitions_zfs(
    cmd: &CommandRunner,
    device: &str,
    layout: &ComputedLayout,
    _filesystem: &Filesystem,
    boot_filesystem: &Filesystem,
    install_root: &str,
) -> Result<()> {
    info!("Setting up ZFS pools and datasets on {}", device);

    // Find the ROOT partition to create the data pool on
    let root_part = layout
        .partitions
        .iter()
        .find(|p| p.mount_point.as_deref() == Some("/") || p.name == "ROOT")
        .ok_or_else(|| {
            crate::utils::error::DeploytixError::ConfigError(
                "No ROOT partition found for ZFS layout".to_string(),
            )
        })?;
    let root_path = partition_path(device, root_part.number);

    // Create the data pool and datasets
    create_zfs_pool(cmd, &root_path)?;
    create_zfs_datasets(cmd)?;

    // Mount datasets to install root
    if !cmd.is_dry_run() {
        std::fs::create_dir_all(install_root)?;
    }
    mount_zfs_datasets(cmd, install_root)?;

    // Mount boot partition
    if *boot_filesystem == Filesystem::Zfs {
        // Boot pool was already created by format_boot_partition()
        mount_zfs_boot(cmd, install_root)?;
    } else {
        // Non-ZFS boot: mount the partition normally
        let boot_part = layout.partitions.iter().find(|p| p.is_boot_fs);
        if let Some(boot) = boot_part {
            let boot_dev = partition_path(device, boot.number);
            let boot_mount = format!("{}/boot", install_root);
            if !cmd.is_dry_run() {
                std::fs::create_dir_all(&boot_mount)?;
            }
            cmd.run("mount", &[&boot_dev, &boot_mount])?;
        }
    }

    // Mount EFI partition
    let efi_part = layout.partitions.iter().find(|p| p.is_efi);
    if let Some(efi) = efi_part {
        let efi_dev = partition_path(device, efi.number);
        let efi_mount = format!("{}/boot/efi", install_root);
        if !cmd.is_dry_run() {
            std::fs::create_dir_all(&efi_mount)?;
        }
        cmd.run("mount", &[&efi_dev, &efi_mount])?;
    }

    // Enable swap
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
    preserve_home: bool,
) -> Result<()> {
    let subvolumes = layout.subvolumes.as_ref().unwrap();
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

    // Create subvolumes on the ROOT partition
    // This temporarily mounts the raw btrfs, creates subvolumes, then unmounts
    let temp_mount = "/tmp/deploytix_btrfs_setup";
    create_btrfs_subvolumes(cmd, &root_path, subvolumes, temp_mount, preserve_home)?;

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
