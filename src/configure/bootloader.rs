//! Bootloader installation and configuration

use crate::config::{Bootloader, DeploymentConfig};
use crate::disk::formatting::get_partition_uuid;
use crate::disk::detection::partition_path;
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
        Bootloader::SystemdBoot => install_systemd_boot(cmd, config, install_root),
    }
}

/// Install GRUB bootloader
fn install_grub(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    install_root: &str,
) -> Result<()> {
    info!("Installing GRUB bootloader");

    // Get root partition UUID for kernel cmdline
    let root_part = partition_path(device, 4); // Root is partition 4 in standard layout
    let root_uuid = if cmd.is_dry_run() {
        "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string()
    } else {
        get_partition_uuid(&root_part)?
    };

    // Configure GRUB defaults
    configure_grub_defaults(cmd, config, &root_uuid, install_root)?;

    if cmd.is_dry_run() {
        println!("  [dry-run] grub-install --target=x86_64-efi --boot-directory=/boot --efi-directory=/boot/efi --removable {}", device);
        println!("  [dry-run] grub-mkconfig -o /boot/grub/grub.cfg");
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

    info!("GRUB installation complete");
    Ok(())
}

/// Configure GRUB defaults
fn configure_grub_defaults(
    cmd: &CommandRunner,
    _config: &DeploymentConfig,
    root_uuid: &str,
    install_root: &str,
) -> Result<()> {
    let grub_default_path = format!("{}/etc/default/grub", install_root);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure /etc/default/grub");
        return Ok(());
    }

    // Build kernel cmdline
    let cmdline_parts = [
        format!("root=UUID={}", root_uuid),
        "loglevel=3".to_string(),
    ];

    // TODO: Add encryption parameters if enabled
    // if config.disk.encryption {
    //     cmdline_parts.push("cryptdevice=...".to_string());
    // }

    // TODO: Add resume for hibernation
    // if config.system.hibernation {
    //     cmdline_parts.push(format!("resume=UUID={}", swap_uuid));
    // }

    let cmdline = cmdline_parts.join(" ");

    // Read current config or create new
    let mut content = fs::read_to_string(&grub_default_path).unwrap_or_else(|_| {
        r#"# GRUB boot loader configuration
GRUB_DEFAULT=0
GRUB_TIMEOUT=5
GRUB_DISTRIBUTOR="Artix"
"#
        .to_string()
    });

    // Update GRUB_CMDLINE_LINUX_DEFAULT
    let new_line = format!("GRUB_CMDLINE_LINUX_DEFAULT=\"{}\"", cmdline);
    if content.contains("GRUB_CMDLINE_LINUX_DEFAULT=") {
        content = content
            .lines()
            .map(|line| {
                if line.starts_with("GRUB_CMDLINE_LINUX_DEFAULT=")
                    || line.starts_with("#GRUB_CMDLINE_LINUX_DEFAULT=")
                {
                    &new_line
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    } else {
        content.push_str(&format!("\n{}\n", new_line));
    }

    fs::write(&grub_default_path, content)?;

    Ok(())
}

/// Install systemd-boot
fn install_systemd_boot(
    cmd: &CommandRunner,
    _config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    info!("Installing systemd-boot");

    if cmd.is_dry_run() {
        println!("  [dry-run] bootctl install");
        return Ok(());
    }

    // Note: systemd-boot requires systemd, which is not the default on Artix
    // This is included for completeness but may not work on pure Artix

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

    // Create entry
    let entries_dir = format!("{}/boot/loader/entries", install_root);
    fs::create_dir_all(&entries_dir)?;

    let entry_content = r#"title   Artix Linux
linux   /vmlinuz-linux-zen
initrd  /initramfs-linux-zen.img
options root=UUID=<ROOT_UUID> rw
"#;
    fs::write(format!("{}/artix.conf", entries_dir), entry_content)?;

    info!("systemd-boot installation complete");
    Ok(())
}
