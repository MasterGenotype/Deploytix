//! Bootloader installation and configuration

use crate::config::{Bootloader, DeploymentConfig, SecureBootMethod};
use crate::configure::encryption::get_luks_uuid;
use crate::disk::detection::partition_path;
use crate::disk::formatting::get_partition_uuid;
use crate::disk::layouts::ComputedLayout;
use crate::disk::lvm;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::info;

/// GRUB modules to embed for standalone EFI binary
/// Includes crypto modules for LUKS encryption support
const GRUB_STANDALONE_MODULES: &str = "all_video boot btrfs cat chain configfile echo \
    efifwsetup efinet ext2 fat font gettext gfxmenu gfxterm gfxterm_background \
    gzio halt help hfsplus iso9660 jpeg keystatus loadenv loopback linux ls \
    lsefi lsefimmap lsefisystab lssal memdisk minicmd normal ntfs part_apple \
    part_msdos part_gpt password_pbkdf2 png probe reboot regexp search \
    search_fs_uuid search_fs_file search_label sleep smbios squash4 test true \
    video xfs zfs zstd cryptodisk luks luks2 gcry_rijndael gcry_sha256 gcry_sha512";

/// Install and configure the bootloader
pub fn install_bootloader(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    layout: &ComputedLayout,
    install_root: &str,
) -> Result<()> {
    match config.system.bootloader {
        Bootloader::Grub => install_grub(cmd, config, device, layout, install_root),
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
    }
}

/// Install GRUB bootloader (non-encrypted, uses layout detection)
fn install_grub(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    layout: &ComputedLayout,
    install_root: &str,
) -> Result<()> {
    info!("Installing GRUB bootloader to {} (x86_64-efi)", device);

    // If encryption or LVM thin is active, should use install_grub_with_layout
    if config.disk.encryption || config.disk.use_lvm_thin {
        return Err(crate::utils::error::DeploytixError::ConfigError(
            "Encrypted or LVM thin systems require install_bootloader_with_layout".to_string(),
        ));
    }

    // Find root partition from layout instead of hardcoding partition number
    let root_part_def = layout
        .partitions
        .iter()
        .find(|p| p.mount_point.as_deref() == Some("/"))
        .ok_or_else(|| {
            crate::utils::error::DeploytixError::ConfigError(
                "No root partition found in layout".to_string(),
            )
        })?;

    let root_part = partition_path(device, root_part_def.number);
    let root_uuid = if cmd.is_dry_run() {
        "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string()
    } else {
        get_partition_uuid(&root_part)?
    };

    // Configure GRUB defaults
    let uses_subvolumes = config.disk.use_subvolumes;
    configure_grub_defaults(
        cmd,
        config,
        &root_uuid,
        None,
        uses_subvolumes,
        false,
        install_root,
    )?;

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

    if config.disk.use_lvm_thin && config.disk.encryption {
        // LVM thin + encryption: encrypt hook needs cryptdevice= parameter,
        // root is on an LVM LV, not a mapper device
        let luks = luks_part.ok_or_else(|| {
            crate::utils::error::DeploytixError::ConfigError(
                "LVM thin + encryption: no LUKS partition found in layout".to_string(),
            )
        })?;
        let luks_device = partition_path(device, luks.number);
        let luks_uuid = if cmd.is_dry_run() {
            "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string()
        } else {
            get_luks_uuid(&luks_device)?
        };
        configure_grub_defaults_lvm_thin(cmd, config, &luks_uuid, install_root)?;
    } else if config.disk.use_lvm_thin {
        // LVM thin without encryption: root is on an LVM LV
        let vg_name = &config.disk.lvm_vg_name;
        let root_lv = lvm::lv_path(vg_name, "root");
        let root_uuid = if cmd.is_dry_run() {
            "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string()
        } else {
            get_partition_uuid(&root_lv)?
        };
        configure_grub_defaults(
            cmd,
            config,
            &root_uuid,
            None,
            layout.uses_subvolumes(),
            false,
            install_root,
        )?;
    } else if let Some(luks) = luks_part {
        // Multi-LUKS: configure with mapper name for root
        let luks_device = partition_path(device, luks.number);
        let luks_uuid = if cmd.is_dry_run() {
            "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string()
        } else {
            get_luks_uuid(&luks_device)?
        };
        configure_grub_defaults(
            cmd,
            config,
            &luks_uuid,
            Some(&config.disk.luks_mapper_name),
            layout.uses_subvolumes(),
            config.disk.boot_encryption,
            install_root,
        )?;
    } else {
        // No LUKS, no LVM thin — should not reach here from install_bootloader_with_layout
        return install_grub(cmd, config, device, layout, install_root);
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

/// Create a pacman hook that reinstalls GRUB after kernel or GRUB package updates.
///
/// This is essential for systems with encrypted boot, standalone GRUB
/// (SecureBoot + encryption), or LVM thin provisioning, where running
/// `grub-mkconfig` alone after a kernel update is not sufficient.
///
/// For standalone GRUB (SecureBoot + encryption), the grub.cfg is embedded
/// inside the EFI binary.  A kernel update changes the vmlinuz path in the
/// generated config, but the old config is still baked into BOOTX64.EFI.
/// The entire standalone binary must be rebuilt with `grub-mkstandalone`.
///
/// For standard GRUB with encrypted boot (`GRUB_ENABLE_CRYPTODISK=y`),
/// `grub-install` must be re-run to ensure the correct crypto modules are
/// embedded in the GRUB core image.
///
/// Creates:
/// - `/etc/pacman.d/hooks/95-grub-reinstall.hook`
/// - `/usr/local/bin/reinstall-grub`
///
/// The hook runs after `90-mkinitcpio-install.hook` (initramfs) and before
/// `99-secureboot.hook` (signing).
pub fn create_grub_reinstall_hook(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    install_root: &str,
) -> Result<()> {
    info!("Creating pacman hook for automatic GRUB reinstallation");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would create /etc/pacman.d/hooks/95-grub-reinstall.hook");
        println!("  [dry-run] Would create /usr/local/bin/reinstall-grub");
        return Ok(());
    }

    let hooks_dir = format!("{}/etc/pacman.d/hooks", install_root);
    fs::create_dir_all(&hooks_dir)?;

    // Standalone GRUB is used when SecureBoot (sbctl) + encryption are both active
    let use_standalone = config.system.secureboot
        && config.system.secureboot_method == SecureBootMethod::Sbctl
        && config.disk.encryption;

    create_grub_reinstall_script(config, device, use_standalone, install_root)?;

    // Build Depends lines based on what the script needs
    let mut depends = vec!["grub"];
    if config.system.secureboot && config.system.secureboot_method == SecureBootMethod::Sbctl {
        depends.push("sbctl");
    }
    let depends_lines: String = depends
        .iter()
        .map(|d| format!("Depends = {}", d))
        .collect::<Vec<_>>()
        .join("\n");

    let hook_content = format!(
        r#"[Trigger]
Operation = Install
Operation = Upgrade
Type = Path
Target = usr/lib/modules/*/vmlinuz
Target = usr/lib/grub/*

[Action]
Description = Reinstalling GRUB for encrypted boot...
When = PostTransaction
Exec = /usr/local/bin/reinstall-grub
{}
"#,
        depends_lines
    );

    let hook_path = format!("{}/95-grub-reinstall.hook", hooks_dir);
    fs::write(&hook_path, hook_content)?;

    info!("Created GRUB reinstall hook at /etc/pacman.d/hooks/95-grub-reinstall.hook");
    Ok(())
}

/// Create the `/usr/local/bin/reinstall-grub` script.
///
/// Two variants are generated depending on `use_standalone`:
/// - **Standalone** (SecureBoot + encryption): rebuilds the self-contained EFI
///   binary via `grub-mkstandalone`, embedding the freshly generated grub.cfg
///   and all required crypto/btrfs/LVM modules.
/// - **Standard**: re-runs `grub-install` to update the core image with the
///   correct crypto modules, then regenerates grub.cfg.
fn create_grub_reinstall_script(
    config: &DeploymentConfig,
    device: &str,
    use_standalone: bool,
    install_root: &str,
) -> Result<()> {
    let script_dir = format!("{}/usr/local/bin", install_root);
    fs::create_dir_all(&script_dir)?;

    let secureboot_block = if config.system.secureboot {
        match config.system.secureboot_method {
            SecureBootMethod::Sbctl => {
                r#"
if command -v sbctl &>/dev/null; then
    echo "Re-signing SecureBoot files..."
    sbctl sign-all
fi
"#
            }
            _ => {
                r#"
if [ -x /usr/local/bin/sign-kernel ]; then
    echo "Re-signing SecureBoot files..."
    /usr/local/bin/sign-kernel
fi
"#
            }
        }
    } else {
        ""
    };

    let script = if use_standalone {
        format!(
            r#"#!/bin/bash
# Reinstall GRUB after kernel or GRUB package update
# Generated by Deploytix
#
# Configuration: encrypted boot + standalone GRUB (SecureBoot)
# The grub.cfg is embedded inside the EFI binary; a plain grub-mkconfig
# is NOT sufficient — the standalone binary must be rebuilt.

set -e

MODULES="{modules}"

echo "Regenerating GRUB config..."
grub-mkconfig -o /boot/grub/grub.cfg

echo "Rebuilding standalone GRUB EFI binary..."
grub-mkstandalone \
    --format=x86_64-efi \
    --output=/boot/efi/EFI/BOOT/BOOTX64.EFI \
    --disable-shim-lock \
    --modules="$MODULES" \
    "boot/grub/grub.cfg=/boot/grub/grub.cfg"
{secureboot}
echo "GRUB reinstallation complete"
"#,
            modules = GRUB_STANDALONE_MODULES,
            secureboot = secureboot_block,
        )
    } else {
        format!(
            r#"#!/bin/bash
# Reinstall GRUB after kernel or GRUB package update
# Generated by Deploytix
#
# Configuration: encrypted boot (standard grub-install)

set -e

echo "Regenerating GRUB config..."
grub-mkconfig -o /boot/grub/grub.cfg

echo "Reinstalling GRUB to EFI..."
grub-install --target=x86_64-efi --boot-directory=/boot --efi-directory=/boot/efi --removable {device}
{secureboot}
echo "GRUB reinstallation complete"
"#,
            device = device,
            secureboot = secureboot_block,
        )
    };

    let script_path = format!("{}/reinstall-grub", script_dir);
    fs::write(&script_path, &script)?;

    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    info!("Created GRUB reinstall script at /usr/local/bin/reinstall-grub");
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
    boot_encryption: bool,
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
    } else if config.disk.filesystem == crate::config::Filesystem::Zfs {
        // ZFS root: the zfs hook reads the root dataset from the kernel cmdline
        cmdline_parts.push(format!(
            "root=ZFS={}",
            crate::disk::formatting::ZFS_ROOT_DATASET
        ));
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

    // Add cryptodisk support — only needed when /boot itself is encrypted
    // (LUKS1), so GRUB must decrypt the boot partition at early boot stage.
    if boot_encryption {
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
