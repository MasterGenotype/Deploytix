//! Host system dependency checking and installation

use crate::config::{Bootloader, Filesystem, PartitionLayout};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use crate::utils::prompt::prompt_confirm;
use std::collections::HashMap;
use std::process::Command;
use tracing::info;

/// Binary to package mapping for Artix/Arch
fn binary_to_package() -> HashMap<&'static str, &'static str> {
    let mut map = HashMap::new();
    // Core partitioning
    map.insert("sfdisk", "util-linux");
    map.insert("mkswap", "util-linux");
    map.insert("blkid", "util-linux");

    // Filesystems
    map.insert("mkfs.vfat", "dosfstools");
    map.insert("mkfs.ext4", "e2fsprogs");
    map.insert("mkfs.btrfs", "btrfs-progs");
    map.insert("mkfs.xfs", "xfsprogs");
    map.insert("mkfs.f2fs", "f2fs-tools");

    // Encryption
    map.insert("cryptsetup", "cryptsetup");

    // LVM
    map.insert("pvcreate", "lvm2");
    map.insert("vgcreate", "lvm2");
    map.insert("lvcreate", "lvm2");

    // Bootloaders
    map.insert("grub-install", "grub");
    map.insert("grub-mkconfig", "grub");
    map.insert("bootctl", "systemd");

    // Artix tools
    map.insert("basestrap", "artools");

    map
}

/// Check if a binary exists in PATH
fn binary_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Determine required binaries based on configuration
pub fn required_binaries(
    layout: &PartitionLayout,
    filesystem: &Filesystem,
    encryption: bool,
    bootloader: &Bootloader,
) -> Vec<&'static str> {
    let mut bins = vec![
        "sfdisk",
        "mkswap",
        "blkid",
        "mkfs.vfat", // EFI partition always FAT32
        "basestrap",
    ];

    // Filesystem-specific
    match filesystem {
        Filesystem::Ext4 => bins.push("mkfs.ext4"),
        Filesystem::Btrfs => bins.push("mkfs.btrfs"),
        Filesystem::Xfs => bins.push("mkfs.xfs"),
        Filesystem::F2fs => bins.push("mkfs.f2fs"),
    }

    // Boot partition uses ext4
    if !bins.contains(&"mkfs.ext4") {
        bins.push("mkfs.ext4");
    }

    // Encryption
    if encryption {
        bins.push("cryptsetup");
    }

    // LVM for LvmThin layout
    if *layout == PartitionLayout::LvmThin {
        bins.push("pvcreate");
        bins.push("vgcreate");
        bins.push("lvcreate");
    }

    // Bootloader
    match bootloader {
        Bootloader::Grub => {
            bins.push("grub-install");
            bins.push("grub-mkconfig");
        }
        Bootloader::SystemdBoot => {
            bins.push("bootctl");
        }
    }

    bins
}

/// Check for missing dependencies and return list of missing packages
pub fn check_dependencies(
    layout: &PartitionLayout,
    filesystem: &Filesystem,
    encryption: bool,
    bootloader: &Bootloader,
) -> Vec<String> {
    let required = required_binaries(layout, filesystem, encryption, bootloader);
    let bin_to_pkg = binary_to_package();

    let mut missing_packages: Vec<String> = Vec::new();

    for bin in required {
        if !binary_exists(bin) {
            if let Some(&pkg) = bin_to_pkg.get(bin) {
                if !missing_packages.contains(&pkg.to_string()) {
                    missing_packages.push(pkg.to_string());
                }
            } else {
                // Unknown package, just report the binary
                missing_packages.push(format!("(provides {})", bin));
            }
        }
    }

    missing_packages
}

/// Check dependencies and optionally install missing packages
/// Returns Ok(()) if all dependencies are satisfied (or were installed)
/// Returns Err if dependencies are missing and user declined to install
pub fn ensure_dependencies(
    cmd: &CommandRunner,
    layout: &PartitionLayout,
    filesystem: &Filesystem,
    encryption: bool,
    bootloader: &Bootloader,
) -> Result<()> {
    let missing = check_dependencies(layout, filesystem, encryption, bootloader);

    if missing.is_empty() {
        info!("All required host dependencies are installed");
        return Ok(());
    }

    println!("\n⚠ Missing host system packages:");
    for pkg in &missing {
        println!("  - {}", pkg);
    }
    println!();

    if cmd.is_dry_run() {
        println!(
            "[dry-run] Would install: pacman -S --noconfirm {}",
            missing.join(" ")
        );
        return Ok(());
    }

    let install = prompt_confirm(
        &format!(
            "Install missing packages? (pacman -S {})",
            missing.join(" ")
        ),
        true,
    )?;

    if !install {
        return Err(crate::utils::error::DeploytixError::ConfigError(
            "Required dependencies are missing. Please install them manually.".to_string(),
        ));
    }

    // Install packages
    println!("Installing packages...");
    let status = Command::new("pacman")
        .args(["-S", "--noconfirm"])
        .args(&missing)
        .status()?;

    if !status.success() {
        return Err(crate::utils::error::DeploytixError::CommandFailed {
            command: format!("pacman -S {}", missing.join(" ")),
            stderr: format!("Exit code: {:?}", status.code()),
        });
    }

    // Verify installation
    let still_missing = check_dependencies(layout, filesystem, encryption, bootloader);
    if !still_missing.is_empty() {
        return Err(crate::utils::error::DeploytixError::ConfigError(format!(
            "Failed to install some dependencies: {}",
            still_missing.join(", ")
        )));
    }

    info!("Successfully installed missing dependencies");
    Ok(())
}
