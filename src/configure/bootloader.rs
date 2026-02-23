//! Bootloader installation and configuration

use crate::config::{Bootloader, DeploymentConfig, PartitionLayout, SecureBootMethod, SwapType};
use crate::configure::encryption::get_luks_uuid;
use crate::disk::detection::partition_path;
use crate::disk::formatting::get_partition_uuid;
use crate::disk::layouts::ComputedLayout;
use crate::disk::lvm;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// GRUB modules to embed for standalone EFI binary
/// Includes crypto modules for LUKS encryption support
const GRUB_STANDALONE_MODULES: &str = "all_video boot btrfs cat chain configfile echo \
    efifwsetup efinet ext2 fat font gettext gfxmenu gfxterm gfxterm_background \
    gzio halt help hfsplus iso9660 jpeg keystatus loadenv loopback linux ls \
    lsefi lsefimmap lsefisystab lssal memdisk minicmd normal ntfs part_apple \
    part_msdos part_gpt password_pbkdf2 png probe reboot regexp search \
    search_fs_uuid search_fs_file search_label sleep smbios squash4 test true \
    video xfs zstd cryptodisk luks luks2 gcry_rijndael gcry_sha256 gcry_sha512";

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

    // Get root partition - find it dynamically based on layout.
    // Encrypted layouts must use install_bootloader_with_layout to get the LUKS UUID.
    if config.disk.encryption
        && (config.disk.layout == PartitionLayout::Standard
            || config.disk.layout == PartitionLayout::Custom)
    {
        return Err(crate::utils::error::DeploytixError::ConfigError(
            "Encrypted Standard/Custom layout requires install_bootloader_with_layout".to_string(),
        ));
    }

    let root_partition_num = match config.disk.layout {
        PartitionLayout::Standard => 4, // EFI(1), Boot(2), Swap(3), Root(4)
        PartitionLayout::Minimal => 4,  // EFI(1), Boot(2), Swap(3), Root(4)
        PartitionLayout::LvmThin => {
            // LvmThin should use install_bootloader_with_layout
            return Err(crate::utils::error::DeploytixError::ConfigError(
                "LvmThin layout requires install_bootloader_with_layout".to_string(),
            ));
        }
        PartitionLayout::Custom => {
            // Root partition number depends on whether a swap partition is present
            // and the order of user-defined partitions: EFI(1), Boot(2), [Swap(3)], user...
            let has_swap = config.disk.swap_type == SwapType::Partition;
            let first_user_num: u32 = if has_swap { 4 } else { 3 };
            match config
                .disk
                .custom_partitions
                .as_ref()
                .and_then(|parts| parts.iter().position(|p| p.mount_point == "/"))
            {
                Some(idx) => first_user_num + idx as u32,
                None => {
                    return Err(crate::utils::error::DeploytixError::ConfigError(
                        "Custom layout has no root (/) partition defined".to_string(),
                    ))
                }
            }
        }
    };

    let root_part = partition_path(device, root_partition_num);
    let root_uuid = if cmd.is_dry_run() {
        "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string()
    } else {
        get_partition_uuid(&root_part)?
    };

    // Configure GRUB defaults (no subvolumes for non-layout-aware path)
    // Note: This path is used for non-encrypted systems; Minimal layout with
    // subvolumes should use install_bootloader_with_layout instead
    let uses_subvolumes = config.disk.layout == PartitionLayout::Minimal;
    configure_grub_defaults(cmd, config, &root_uuid, None, uses_subvolumes, install_root)?;

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
    info!(
        "Installing GRUB bootloader to {} (x86_64-efi, encrypted)",
        device
    );

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

        if config.disk.layout == PartitionLayout::LvmThin {
            // LvmThin: encrypt hook needs cryptdevice= parameter,
            // root is on an LVM LV, not a mapper device
            configure_grub_defaults_lvm_thin(cmd, config, &luks_uuid, install_root)?;
        } else {
            // Standard layout: configure with mapper name for root
            configure_grub_defaults(
                cmd,
                config,
                &luks_uuid,
                Some(&config.disk.luks_mapper_name),
                layout.uses_subvolumes(),
                install_root,
            )?;
        }
    } else {
        // Fall back to non-encrypted
        return install_grub(cmd, config, device, install_root);
    }

    // Use SecureBoot-aware install if SecureBoot is enabled
    if config.system.secureboot {
        run_grub_install_with_secureboot(cmd, config, device, install_root)?;
    } else {
        run_grub_install(cmd, device, install_root)?;
    }

    info!("GRUB installation complete");
    Ok(())
}

/// Run grub-install, grub-mkconfig, and create EFI boot entry
fn run_grub_install(cmd: &CommandRunner, device: &str, install_root: &str) -> Result<()> {
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

/// Run grub-install with SecureBoot signing
///
/// For sbctl method with encryption, uses grub-mkstandalone to create a self-contained
/// EFI binary that avoids GRUB's internal verifier issues.
pub fn run_grub_install_with_secureboot(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    install_root: &str,
) -> Result<()> {
    // For sbctl method with encryption, use standalone GRUB to avoid verification errors
    let use_standalone = config.system.secureboot
        && config.system.secureboot_method == SecureBootMethod::Sbctl
        && config.disk.encryption;

    if use_standalone {
        info!("Using standalone GRUB for SecureBoot with encryption");
        run_grub_mkstandalone(cmd, device, install_root)?;
    } else {
        // Standard GRUB install for non-encrypted or shim-based SecureBoot
        run_grub_install(cmd, device, install_root)?;
    }

    // Sign the EFI binaries if SecureBoot is enabled
    if config.system.secureboot {
        info!("Signing GRUB for SecureBoot");
        crate::configure::secureboot::sign_boot_files(cmd, config, install_root)?;
    }

    Ok(())
}

/// Create standalone GRUB EFI binary with embedded modules and config
///
/// This method creates a self-contained GRUB that:
/// - Has all modules embedded (no external module loading)
/// - Has grub.cfg embedded in a memdisk
/// - Uses --disable-shim-lock for sbctl-based signing
/// - Avoids "verification requested but nobody cares" errors
fn run_grub_mkstandalone(cmd: &CommandRunner, device: &str, install_root: &str) -> Result<()> {
    info!("Creating standalone GRUB EFI binary");

    if cmd.is_dry_run() {
        println!("  [dry-run] grub-mkconfig -o /boot/grub/grub.cfg");
        println!("  [dry-run] grub-mkstandalone --format=x86_64-efi --output=/boot/efi/EFI/BOOT/BOOTX64.EFI --disable-shim-lock --modules=\"...\" boot/grub/grub.cfg=/boot/grub/grub.cfg");
        println!("  [dry-run] efibootmgr --create --disk {} --part 1 --loader /EFI/BOOT/BOOTX64.EFI --label 'Artix-SB'", device);
        return Ok(());
    }

    // Ensure /boot/grub directory exists inside chroot
    // (normally created by grub-install, but standalone mode skips grub-install)
    let grub_dir = format!("{}/boot/grub", install_root);
    fs::create_dir_all(&grub_dir)?;

    // Generate GRUB config
    cmd.run_in_chroot(install_root, "grub-mkconfig -o /boot/grub/grub.cfg")?;

    // Ensure EFI directory exists
    let efi_boot_dir = format!("{}/boot/efi/EFI/BOOT", install_root);
    fs::create_dir_all(&efi_boot_dir)?;

    // Create standalone GRUB with embedded config and modules
    let grub_mkstandalone_cmd = format!(
        "grub-mkstandalone \
            --format=x86_64-efi \
            --output=/boot/efi/EFI/BOOT/BOOTX64.EFI \
            --disable-shim-lock \
            --modules=\"{}\" \
            \"boot/grub/grub.cfg=/boot/grub/grub.cfg\"",
        GRUB_STANDALONE_MODULES
    );
    cmd.run_in_chroot(install_root, &grub_mkstandalone_cmd)?;

    // Create EFI boot entry with SecureBoot label
    create_efi_boot_entry(cmd, device, 1, "Artix-SB")?;

    info!("Standalone GRUB created successfully");
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
    info!(
        "Creating EFI boot entry for {} on {} partition {}",
        label, device, efi_partition
    );

    if cmd.is_dry_run() {
        println!("  [dry-run] efibootmgr --create --disk {} --part {} --loader /EFI/BOOT/BOOTX64.EFI --label '{}'",
            device, efi_partition, label);
        return Ok(());
    }

    // Create boot entry pointing to GRUB's EFI binary
    // --removable flag in grub-install places it at /EFI/BOOT/BOOTX64.EFI
    cmd.run(
        "efibootmgr",
        &[
            "--create",
            "--disk",
            device,
            "--part",
            &efi_partition.to_string(),
            "--loader",
            "/EFI/BOOT/BOOTX64.EFI",
            "--label",
            label,
        ],
    )?;

    info!("EFI boot entry '{}' created successfully", label);
    Ok(())
}

/// Set the EFI boot order to prioritize the given entry
///
/// This finds the boot entry with the given label and moves it to the front
/// of the boot order.
#[allow(dead_code)]
pub fn set_efi_boot_order_priority(cmd: &CommandRunner, label: &str) -> Result<()> {
    info!("Setting EFI boot order priority for '{}'", label);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would set boot order priority for '{}'", label);
        return Ok(());
    }

    // Get current boot entries to find the entry number for our label
    let output = crate::utils::command::run_command_output("efibootmgr", &[])?;

    // Parse output to find our entry number
    // Format: Boot0014* Artix-SB	HD(...)
    let mut our_entry: Option<String> = None;
    let mut other_entries: Vec<String> = Vec::new();

    for line in output.lines() {
        if line.starts_with("Boot") && line.contains('*') {
            // Extract entry number (e.g., "0014" from "Boot0014*")
            if let Some(entry_num) = line.strip_prefix("Boot").and_then(|s| s.split('*').next()) {
                if line.contains(label) {
                    our_entry = Some(entry_num.to_string());
                } else {
                    other_entries.push(entry_num.to_string());
                }
            }
        }
    }

    if let Some(entry) = our_entry {
        // Build new boot order with our entry first
        let mut new_order = vec![entry];
        new_order.extend(other_entries);
        let order_str = new_order.join(",");

        cmd.run("efibootmgr", &["-o", &order_str])?;
        info!("Boot order set: {}", order_str);
    } else {
        info!(
            "Boot entry '{}' not found, skipping boot order change",
            label
        );
    }

    Ok(())
}

/// Configure GRUB defaults
/// For encrypted systems, pass luks_uuid and mapper_name
/// uses_subvolumes indicates if the layout uses btrfs subvolumes (for rootflags)
fn configure_grub_defaults(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    root_or_luks_uuid: &str,
    mapper_name: Option<&str>,
    uses_subvolumes: bool,
    install_root: &str,
) -> Result<()> {
    let grub_default_path = format!("{}/etc/default/grub", install_root);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure /etc/default/grub");
        if mapper_name.is_some() {
            println!("    GRUB_ENABLE_CRYPTODISK=y");
            println!("    cryptdevice=UUID=<LUKS_UUID>:<mapper> root=/dev/mapper/<mapper>");
        }
        if uses_subvolumes {
            println!("    rootflags=subvol=@");
        }
        return Ok(());
    }

    // Build kernel cmdline
    let mut cmdline_parts = vec!["quiet".to_string()];

    if let Some(mapper) = mapper_name {
        // Encrypted system uses custom hooks (crypttab-unlock + mountcrypt)
        // NOT the standard encrypt hook, so we don't use cryptdevice= parameter.
        // The mountcrypt hook's mount_handler handles all mounting.
        // Set root= to the mapper device so mkinitcpio knows what to pass to mount_handler.
        cmdline_parts.push(format!("root=/dev/mapper/{}", mapper));
        cmdline_parts.push("rw".to_string());
    } else {
        // Non-encrypted system
        cmdline_parts.push(format!("root=UUID={}", root_or_luks_uuid));
        // Only add rootflags=subvol=@ if layout uses btrfs subvolumes
        if uses_subvolumes {
            cmdline_parts.push("rootflags=subvol=@".to_string());
        }
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

/// Configure GRUB defaults for LvmThin encrypted layout
///
/// LvmThin requires:
/// - `cryptdevice=UUID=<LUKS_UUID>:Crypt-LVM` for the encrypt hook
/// - `root=/dev/<vg>/<lv>` pointing to the root thin LV
/// - `GRUB_ENABLE_CRYPTODISK=y` only when boot encryption is enabled
fn configure_grub_defaults_lvm_thin(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    luks_uuid: &str,
    install_root: &str,
) -> Result<()> {
    let grub_default_path = format!("{}/etc/default/grub", install_root);
    let vg_name = &config.disk.lvm_vg_name;
    let root_lv = lvm::lv_path(vg_name, "root");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure /etc/default/grub for LvmThin");
        println!(
            "    cryptdevice=UUID={}:Crypt-LVM root={}",
            luks_uuid, root_lv
        );
        if config.disk.boot_encryption {
            println!("    GRUB_ENABLE_CRYPTODISK=y");
        }
        return Ok(());
    }

    // Build kernel cmdline
    let mut cmdline_parts = vec!["quiet".to_string()];

    // The encrypt hook reads cryptdevice= to know which LUKS device to decrypt
    cmdline_parts.push(format!("cryptdevice=UUID={}:Crypt-LVM", luks_uuid));
    cmdline_parts.push(format!("root={}", root_lv));

    // When boot_encryption is enabled, a keyfile for Crypt-LVM is embedded in
    // the initramfs (at /etc/cryptsetup-keys.d/cryptlvm.key).  Tell the encrypt
    // hook to use it so the LUKS container is opened automatically without
    // prompting for a second password at early boot.
    if config.disk.boot_encryption {
        cmdline_parts.push("cryptkey=rootfs:/etc/cryptsetup-keys.d/cryptlvm.key".to_string());
    }

    cmdline_parts.push("rw".to_string());

    // Add resume for hibernation
    if config.system.hibernation {
        // TODO: Get swap UUID and add resume parameter
    }

    let cmdline = cmdline_parts.join(" ");

    let mut content = format!(
        r#"# GRUB boot loader configuration
# Generated by Deploytix - LVM Thin Provisioning layout

GRUB_DEFAULT=0
GRUB_TIMEOUT=5
GRUB_DISTRIBUTOR="Artix"
GRUB_CMDLINE_LINUX_DEFAULT="{}"
"#,
        cmdline
    );

    // Enable cryptodisk only when boot partition is encrypted (LUKS1)
    // GRUB needs this to decrypt /boot at early boot stage
    if config.disk.boot_encryption {
        content.push_str("GRUB_ENABLE_CRYPTODISK=y\n");
    }

    fs::create_dir_all(format!("{}/etc/default", install_root))?;
    fs::write(&grub_default_path, content)?;

    info!("GRUB defaults (LvmThin) written to /etc/default/grub");
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
    // Encrypted systems should use GRUB instead
    if config.disk.encryption {
        return Err(crate::utils::error::DeploytixError::ConfigError(
            "systemd-boot is not supported with encrypted layouts (use GRUB)".to_string(),
        ));
    }

    let root_partition_num = match config.disk.layout {
        PartitionLayout::Standard => 4, // EFI(1), Boot(2), Swap(3), Root(4)
        PartitionLayout::Minimal => 4,  // EFI(1), Boot(2), Swap(3), Root(4)
        PartitionLayout::LvmThin => {
            // LvmThin requires encryption which uses GRUB, so this shouldn't be reached
            return Err(crate::utils::error::DeploytixError::ConfigError(
                "systemd-boot is not supported with LvmThin layout (use GRUB)".to_string(),
            ));
        }
        PartitionLayout::Custom => {
            // Root partition number depends on whether a swap partition is present.
            let has_swap = config.disk.swap_type == SwapType::Partition;
            let first_user_num: u32 = if has_swap { 4 } else { 3 };
            match config
                .disk
                .custom_partitions
                .as_ref()
                .and_then(|parts| parts.iter().position(|p| p.mount_point == "/"))
            {
                Some(idx) => first_user_num + idx as u32,
                None => {
                    return Err(crate::utils::error::DeploytixError::ConfigError(
                        "Custom layout has no root (/) partition defined".to_string(),
                    ))
                }
            }
        }
    };

    let root_part = partition_path(device, root_partition_num);
    let root_uuid = if cmd.is_dry_run() {
        "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string()
    } else {
        get_partition_uuid(&root_part)?
    };

    if cmd.is_dry_run() {
        println!("  [dry-run] bootctl install");
        println!(
            "  [dry-run] Would create loader.conf and artix.conf with UUID={}",
            root_uuid
        );
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
