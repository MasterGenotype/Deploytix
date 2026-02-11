//! Bootloader installation and configuration

use crate::config::{Bootloader, DeploymentConfig, PartitionLayout};
use crate::configure::encryption::get_luks_uuid;
use crate::disk::formatting::get_partition_uuid;
use crate::disk::detection::partition_path;
use crate::disk::layouts::ComputedLayout;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// Install and configure the bootloader
pub fn install_bootloader(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    install_root: &str,
) -> Result<()> {
    match config.system.bootloader {
        Bootloader::Grub => install_grub(cmd, config, device, install_root),
        Bootloader::SystemdBoot => install_systemd_boot(cmd, config, device, install_root),
    }
}

/// Install and configure the bootloader with layout info (for encrypted systems)
pub fn install_bootloader_with_layout(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    layout: &ComputedLayout,
    install_root: &str,
) -> Result<()> {
    match config.system.bootloader {
        Bootloader::Grub => install_grub_with_layout(cmd, config, device, layout, install_root),
        Bootloader::SystemdBoot => install_systemd_boot(cmd, config, device, install_root),
    }
}

/// Install GRUB bootloader (non-encrypted, uses layout detection)
fn install_grub(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    install_root: &str,
) -> Result<()> {
    info!("Installing GRUB bootloader to {} (x86_64-efi)", device);

    // Get root partition - find it dynamically based on layout
    let root_partition_num = match config.disk.layout {
        PartitionLayout::Standard => 4,  // Root is partition 4 in standard layout
        PartitionLayout::Minimal => 3,   // Root is partition 3 in minimal layout
        PartitionLayout::CryptoSubvolume => {
            // For encrypted systems, should use install_grub_with_layout
            return Err(crate::utils::error::DeploytixError::ConfigError(
                "CryptoSubvolume layout requires install_bootloader_with_layout".to_string()
            ));
        }
        PartitionLayout::Custom => 4,    // Assume standard for custom
    };

    let root_part = partition_path(device, root_partition_num);
    let root_uuid = if cmd.is_dry_run() {
        "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string()
    } else {
        get_partition_uuid(&root_part)?
    };

    // Configure GRUB defaults
    configure_grub_defaults(cmd, config, &root_uuid, None, install_root)?;

    run_grub_install(cmd, device, install_root)?;

    info!("GRUB installation complete");
    Ok(())
}

/// Install GRUB bootloader with layout info (for encrypted systems)
fn install_grub_with_layout(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    layout: &ComputedLayout,
    install_root: &str,
) -> Result<()> {
    info!("Installing GRUB bootloader to {} (x86_64-efi, encrypted)", device);

    // Find LUKS partition from layout
    let luks_part = layout.partitions.iter().find(|p| p.is_luks);

    if let Some(luks) = luks_part {
        // Encrypted system
        let luks_device = partition_path(device, luks.number);
        let luks_uuid = if cmd.is_dry_run() {
            "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string()
        } else {
            get_luks_uuid(&luks_device)?
        };

        // Configure GRUB defaults for encrypted system
        configure_grub_defaults(cmd, config, &luks_uuid, Some(&config.disk.luks_mapper_name), install_root)?;
    } else {
        // Fall back to non-encrypted
        return install_grub(cmd, config, device, install_root);
    }

    run_grub_install(cmd, device, install_root)?;

    info!("GRUB installation complete");
    Ok(())
}

/// Run grub-install, grub-mkconfig, and create EFI boot entry
fn run_grub_install(
    cmd: &CommandRunner,
    device: &str,
    install_root: &str,
) -> Result<()> {
    if cmd.is_dry_run() {
        println!("  [dry-run] grub-install --target=x86_64-efi --boot-directory=/boot --efi-directory=/boot/efi --removable {}", device);
        println!("  [dry-run] grub-mkconfig -o /boot/grub/grub.cfg");
        println!("  [dry-run] efibootmgr --create --disk {} --part 1 --loader /EFI/BOOT/BOOTX64.EFI --label 'Artix Linux'", device);
        return Ok(());
    }

    // Install GRUB
    let grub_install_cmd = format!(
        "grub-install --target=x86_64-efi --boot-directory=/boot --efi-directory=/boot/efi --removable {}",
        device
    );
    cmd.run_in_chroot(install_root, &grub_install_cmd)?;

    // Generate GRUB config
    cmd.run_in_chroot(install_root, "grub-mkconfig -o /boot/grub/grub.cfg")?;

    // Create EFI boot entry using efibootmgr (required for bootable system)
    create_efi_boot_entry(cmd, device, 1, "Artix Linux")?;

    Ok(())
}

/// Create EFI boot entry using efibootmgr
///
/// This is REQUIRED after grub-install to register the boot entry in UEFI firmware.
/// Without this, the system will not present a bootable option after reboot.
pub fn create_efi_boot_entry(
    cmd: &CommandRunner,
    device: &str,
    efi_partition: u32,
    label: &str,
) -> Result<()> {
    info!("Creating EFI boot entry for {} on {} partition {}", label, device, efi_partition);

    if cmd.is_dry_run() {
        println!("  [dry-run] efibootmgr --create --disk {} --part {} --loader /EFI/BOOT/BOOTX64.EFI --label '{}'",
            device, efi_partition, label);
        return Ok(());
    }

    // Create boot entry pointing to GRUB's EFI binary
    // --removable flag in grub-install places it at /EFI/BOOT/BOOTX64.EFI
    cmd.run("efibootmgr", &[
        "--create",
        "--disk", device,
        "--part", &efi_partition.to_string(),
        "--loader", "/EFI/BOOT/BOOTX64.EFI",
        "--label", label,
    ])?;

    info!("EFI boot entry '{}' created successfully", label);
    Ok(())
}

/// Configure GRUB defaults
/// For encrypted systems, pass luks_uuid and mapper_name
fn configure_grub_defaults(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    root_or_luks_uuid: &str,
    mapper_name: Option<&str>,
    install_root: &str,
) -> Result<()> {
    let grub_default_path = format!("{}/etc/default/grub", install_root);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure /etc/default/grub");
        if mapper_name.is_some() {
            println!("    GRUB_ENABLE_CRYPTODISK=y");
            println!("    cryptdevice=UUID=<LUKS_UUID>:<mapper> root=/dev/mapper/<mapper>");
        }
        return Ok(());
    }

    // Build kernel cmdline
    let mut cmdline_parts = vec!["quiet".to_string()];

    if let Some(mapper) = mapper_name {
        // Encrypted system
        cmdline_parts.push(format!("cryptdevice=UUID={}:{}", root_or_luks_uuid, mapper));
        cmdline_parts.push(format!("root=/dev/mapper/{}", mapper));
        cmdline_parts.push("rootflags=subvol=@".to_string());
        cmdline_parts.push("rw".to_string());
    } else {
        // Non-encrypted system
        cmdline_parts.push(format!("root=UUID={}", root_or_luks_uuid));
        cmdline_parts.push("rw".to_string());
    }

    // Add resume for hibernation
    if config.system.hibernation {
        // TODO: Get swap UUID and add resume parameter
        // cmdline_parts.push(format!("resume=UUID={}", swap_uuid));
    }

    let cmdline = cmdline_parts.join(" ");

    // Build GRUB config content
    let mut content = format!(
        r#"# GRUB boot loader configuration
# Generated by Deploytix

GRUB_DEFAULT=0
GRUB_TIMEOUT=5
GRUB_DISTRIBUTOR="Artix"
GRUB_CMDLINE_LINUX_DEFAULT="{}"
"#,
        cmdline
    );

    // Add cryptodisk support for encrypted systems
    if mapper_name.is_some() {
        content.push_str("GRUB_ENABLE_CRYPTODISK=y\n");
    }

    fs::create_dir_all(format!("{}/etc/default", install_root))?;
    fs::write(&grub_default_path, content)?;

    info!("GRUB defaults written to /etc/default/grub");
    Ok(())
}

/// Install systemd-boot
fn install_systemd_boot(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    install_root: &str,
) -> Result<()> {
    info!("Installing systemd-boot");

    // Note: systemd-boot requires systemd, which is not the default on Artix
    // This is included for completeness but may not work on pure Artix

    // Get root partition dynamically based on layout
    let root_partition_num = match config.disk.layout {
        PartitionLayout::Standard => 4,
        PartitionLayout::Minimal => 3,
        PartitionLayout::CryptoSubvolume => {
            return Err(crate::utils::error::DeploytixError::ConfigError(
                "systemd-boot is not supported with CryptoSubvolume layout (use GRUB)".to_string()
            ));
        }
        PartitionLayout::Custom => 4,
    };

    let root_part = partition_path(device, root_partition_num);
    let root_uuid = if cmd.is_dry_run() {
        "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string()
    } else {
        get_partition_uuid(&root_part)?
    };

    if cmd.is_dry_run() {
        println!("  [dry-run] bootctl install");
        println!("  [dry-run] Would create loader.conf and artix.conf with UUID={}", root_uuid);
        return Ok(());
    }

    cmd.run_in_chroot(install_root, "bootctl install")?;

    // Create loader.conf
    let loader_conf = format!("{}/boot/loader/loader.conf", install_root);
    let loader_content = r#"default artix.conf
timeout 3
console-mode max
editor no
"#;
    fs::create_dir_all(format!("{}/boot/loader", install_root))?;
    fs::write(&loader_conf, loader_content)?;

    // Create entry with actual UUID (fixes P0 placeholder bug)
    let entries_dir = format!("{}/boot/loader/entries", install_root);
    fs::create_dir_all(&entries_dir)?;

    let entry_content = format!(
        r#"title   Artix Linux
linux   /vmlinuz-linux-zen
initrd  /initramfs-linux-zen.img
options root=UUID={} rw
"#,
        root_uuid
    );
    fs::write(format!("{}/artix.conf", entries_dir), entry_content)?;

    info!("systemd-boot installation complete");
    Ok(())
}
