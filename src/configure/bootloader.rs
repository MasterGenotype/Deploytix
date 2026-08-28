//! Bootloader installation and configuration

use crate::config::{Bootloader, DeploymentConfig, SecureBootMethod};
use crate::configure::encryption::get_luks_uuid;
use crate::disk::detection::partition_path;
use crate::disk::formatting::get_partition_uuid;
use crate::disk::layouts::{is_root_partition, ComputedLayout};
use crate::disk::lvm;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::{info, warn};

/// GRUB modules to embed for standalone EFI binary
/// Includes crypto modules for LUKS encryption support
const GRUB_STANDALONE_MODULES: &str = "all_video boot btrfs cat chain configfile echo \
    efifwsetup efinet ext2 fat font gettext gfxmenu gfxterm gfxterm_background \
    gzio halt help hfsplus iso9660 jpeg keystatus loadenv loopback linux ls \
    lsefi lsefimmap lsefisystab lssal memdisk minicmd normal ntfs part_apple \
    part_msdos part_gpt password_pbkdf2 png probe reboot regexp search \
    search_fs_uuid search_fs_file search_label sleep smbios squash4 test true \
    video xfs zfs zstd cryptodisk luks luks2 gcry_rijndael gcry_sha256 gcry_sha512";

/// Whether this configuration boots through a standalone GRUB EFI binary
/// (grub.cfg embedded as a memdisk, rebuilt + re-signed on change) instead of
/// a standard grub-install with an on-disk grub.cfg.
pub fn uses_standalone_grub(config: &DeploymentConfig) -> bool {
    config.system.secureboot
        && config.system.secureboot_method == SecureBootMethod::Sbctl
        && config.disk.encryption
}

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

/// Get the swap partition UUID from the layout, if a swap partition exists.
fn get_swap_uuid_from_layout(
    cmd: &CommandRunner,
    device: &str,
    layout: &ComputedLayout,
) -> Result<Option<String>> {
    if let Some(swap_part) = layout.partitions.iter().find(|p| p.is_swap) {
        let swap_device = partition_path(device, swap_part.number);
        let uuid = if cmd.is_dry_run() {
            "XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string()
        } else {
            get_partition_uuid(&swap_device)?
        };
        Ok(Some(uuid))
    } else {
        Ok(None)
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

    // Find root partition from layout instead of hardcoding partition number.
    let root_part_def = layout
        .partitions
        .iter()
        .find(|p| is_root_partition(p))
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
    let swap_uuid = get_swap_uuid_from_layout(cmd, device, layout)?;
    configure_grub_defaults(
        cmd,
        config,
        &root_uuid,
        None,
        uses_subvolumes,
        false,
        install_root,
        swap_uuid.as_deref(),
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

    // Get swap UUID for hibernation resume parameter
    let swap_uuid = get_swap_uuid_from_layout(cmd, device, layout)?;

    if config.immutable_lvm_ab() {
        // LVM immutable A/B: the verity-ab hook mounts the active slot; GRUB only
        // supplies cryptdevice= (if encrypted) and the deploytix.slot/roothash
        // pointer (roothash is a placeholder here, sed-patched at finalize once
        // the slot's verity tree is built).
        let luks_uuid = match luks_part {
            Some(luks) if config.disk.encryption => {
                let luks_device = partition_path(device, luks.number);
                if cmd.is_dry_run() {
                    Some("XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string())
                } else {
                    Some(get_luks_uuid(&luks_device)?)
                }
            }
            _ => None,
        };
        configure_grub_defaults_lvm_ab(
            cmd,
            config,
            luks_uuid.as_deref(),
            install_root,
            swap_uuid.as_deref(),
        )?;
    } else if config.disk.use_lvm_thin && config.disk.encryption {
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
        configure_grub_defaults_lvm_thin(
            cmd,
            config,
            &luks_uuid,
            install_root,
            swap_uuid.as_deref(),
        )?;
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
            swap_uuid.as_deref(),
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
            swap_uuid.as_deref(),
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
    let use_standalone = uses_standalone_grub(config);

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

    // NVRAM entries require EFI variables, which are absent when the
    // installing host is BIOS-booted (common on VPSes) or the chroot has no
    // efivarfs. The loader sits at the removable fallback path
    // /EFI/BOOT/BOOTX64.EFI, which UEFI firmware boots without an NVRAM
    // entry, so the target stays bootable — skip registration instead of
    // failing the install.
    let efivars = std::path::Path::new("/sys/firmware/efi/efivars");
    let efivars_usable = fs::read_dir(efivars)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if !efivars_usable {
        warn!(
            "EFI variables unavailable on this host; skipping efibootmgr registration for '{}' \
             (the removable-path loader /EFI/BOOT/BOOTX64.EFI boots without an NVRAM entry)",
            label
        );
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
    let use_standalone = uses_standalone_grub(config);

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
Operation = Remove
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

/// Install the grub-btrfs compatibility patch for encrypted btrfs layouts.
///
/// On this installer's encrypted layouts, grub-btrfs's
/// `/etc/grub.d/41_snapshots-btrfs` generator misbehaves in two distinct
/// ways — one fatal, one a boot-time regression:
///
/// 1. **Fatal.** `grub-probe --target=fs_uuid` cannot walk device-mapper
///    stacks that contain a dm-integrity layer (`<name>_dif`) — and fails on
///    some plain LUKS2 setups too. The generator runs under `set -e`, so the
///    failing command substitution kills the whole script, aborting
///    `grub-mkconfig` and with it every kernel-update GRUB regeneration.
/// 2. **Degraded.** With `GRUB_BTRFS_ENABLE_CRYPTODISK="true"` the generator
///    resolves which LUKS container GRUB must unlock by grepping the cmdline
///    for `cryptdevice=…`, a token the stock `encrypt` hook consumes but
///    Deploytix's custom hooks never emit. That grep is `|| true`-guarded
///    upstream, so it does *not* abort `grub-mkconfig`; instead the generator
///    falls back to `cryptomount -a`, making GRUB prompt for every LUKS
///    container it can see (Boot, Root, Usr, Var, Home) before a snapshot
///    entry boots, rather than unlocking just the one holding `/boot`.
///
/// Creates:
/// - `/usr/local/bin/patch-grub-btrfs-integrity` — adds a `grub-probe ||
///   blkid` fallback chain to `41_snapshots-btrfs`, sets
///   `GRUB_BTRFS_ENABLE_CRYPTODISK` to match the layout's boot encryption,
///   and (when `/boot` is encrypted) pins `crypt_source` to the boot LUKS
///   UUID so snapshot entries emit `cryptomount -u <uuid>`
/// - `/etc/pacman.d/hooks/91-patch-grub-btrfs.hook` — re-applies the patch
///   whenever the grub-btrfs package (re)writes `41_snapshots-btrfs`;
///   numbered before `95-grub-reinstall.hook` so the patched generator is in
///   place when `grub-mkconfig` runs
///
/// Both are inert until grub-btrfs is installed. `boot_luks_uuid` is the LUKS
/// UUID of the container holding `/boot`, required when
/// `config.disk.boot_encryption` is set and ignored otherwise.
pub fn create_grub_btrfs_compat(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    boot_luks_uuid: Option<&str>,
    install_root: &str,
) -> Result<()> {
    info!("Installing grub-btrfs compatibility patch for encrypted btrfs layout");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would create /usr/local/bin/patch-grub-btrfs-integrity");
        println!("  [dry-run] Would create /etc/pacman.d/hooks/91-patch-grub-btrfs.hook");
        return Ok(());
    }

    // GRUB only needs cryptomount preludes in snapshot entries when /boot
    // itself is encrypted; otherwise it reads kernels via plain search.
    let cryptodisk = if config.disk.boot_encryption {
        "true"
    } else {
        "false"
    };

    // Only meaningful on the cryptodisk path. Empty disables the patch, which
    // leaves upstream's `cryptomount -a` fallback in place.
    let boot_luks_uuid = if config.disk.boot_encryption {
        let uuid = boot_luks_uuid.unwrap_or("");
        if uuid.is_empty() {
            warn!(
                "Encrypted /boot but no boot LUKS UUID available — grub-btrfs snapshot \
                 entries will fall back to `cryptomount -a` (prompts for every container)"
            );
        }
        uuid
    } else {
        ""
    };

    let script = format!(
        r##"#!/bin/bash
# Patch grub-btrfs for Deploytix encrypted layouts.
# Generated by Deploytix
#
# 1. /etc/grub.d/41_snapshots-btrfs: `grub-probe --target=fs_uuid` cannot read
#    through dm-crypt/dm-integrity mapper stacks (e.g. `<name>_dif`) and exits
#    non-zero — under the script's `set -e` that aborts grub-mkconfig entirely.
#    A `grub-probe || blkid` fallback restores UUID resolution: blkid reads the
#    filesystem superblock through the mapper device and has no opinion about
#    the device-mapper hierarchy above it.
# 2. /etc/default/grub-btrfs/config: GRUB_BTRFS_ENABLE_CRYPTODISK must match
#    whether /boot itself is encrypted, so the cryptodisk branch only runs
#    where GRUB actually has to unlock something.
# 3. On the cryptodisk branch the generator picks the container to unlock by
#    grepping the kernel cmdline for `cryptdevice=`, a token the stock
#    `encrypt` hook consumes but Deploytix's custom hooks never emit. That
#    grep is `|| true`-guarded upstream, so grub-mkconfig still succeeds --
#    but the generator then emits `cryptomount -a`, and GRUB prompts for
#    every LUKS container it can see before a snapshot entry boots. Pinning
#    crypt_source to the boot container's UUID restores `cryptomount -u`.
#
# Idempotent: markers record applied state. Re-run automatically whenever the
# grub-btrfs package writes 41_snapshots-btrfs (91-patch-grub-btrfs.hook).
# No-op when grub-btrfs is not installed.

set -euo pipefail

TARGET="/etc/grub.d/41_snapshots-btrfs"
MARKER="# DEPLOYTIX-INTEGRITY-PATCH-V1"
CONFIG="/etc/default/grub-btrfs/config"
CONFIG_MARKER="# DEPLOYTIX-CRYPTODISK-V1"
SOURCE_MARKER="# DEPLOYTIX-CRYPTSOURCE-V1"
CRYPTODISK="{cryptodisk}"
BOOT_LUKS_UUID="{boot_luks_uuid}"

patch_snapshots_script() {{
    if [ ! -f "$TARGET" ]; then
        echo "grub-btrfs script not found at $TARGET; nothing to patch." >&2
        return 0
    fi

    if grep -qF "$MARKER" "$TARGET"; then
        echo "Integrity patch already applied to $TARGET."
        return 0
    fi

    # root_uuid: grub-probe -> blkid -> empty
    sed -i 's@^root_uuid=\$(\${{grub_probe}} --device \${{root_device}} --target="fs_uuid" 2>/dev/null).*$@root_uuid=$(${{grub_probe}} --device ${{root_device}} --target="fs_uuid" 2>/dev/null || blkid -s UUID -o value "${{root_device}}" 2>/dev/null || true) # patched: integrity fallback@' "$TARGET"

    # boot_uuid: same chain
    sed -i 's@^boot_uuid=\$(\${{grub_probe}} --device \${{boot_device}} --target="fs_uuid" 2>/dev/null).*$@boot_uuid=$(${{grub_probe}} --device ${{boot_device}} --target="fs_uuid" 2>/dev/null || blkid -s UUID -o value "${{boot_device}}" 2>/dev/null || true) # patched: integrity fallback@' "$TARGET"

    # boot_hs: hints_string has no good fallback; just absorb failure
    sed -i 's@^boot_hs=\$(\${{grub_probe}} --device \${{boot_device}} --target="hints_string" 2>/dev/null).*$@boot_hs=$(${{grub_probe}} --device ${{boot_device}} --target="hints_string" 2>/dev/null || true) # patched: integrity fallback@' "$TARGET"

    # boot_fs: fall back to blkid TYPE
    sed -i 's@^boot_fs=\$(\${{grub_probe}} --device \${{boot_device}} --target="fs" 2>/dev/null).*$@boot_fs=$(${{grub_probe}} --device ${{boot_device}} --target="fs" 2>/dev/null || blkid -s TYPE -o value "${{boot_device}}" 2>/dev/null || true) # patched: integrity fallback@' "$TARGET"

    # Insert marker right after the shebang so future invocations skip
    sed -i "1a $MARKER" "$TARGET"

    # Sanity check: all four patches landed
    if ! grep -q "patched: integrity fallback" "$TARGET" \
       || [ "$(grep -c 'patched: integrity fallback' "$TARGET")" -lt 4 ]; then
        echo "ERROR: integrity patch did not apply cleanly to $TARGET" >&2
        exit 1
    fi

    echo "Patched $TARGET for LUKS2/integrity (grub-probe -> blkid fallback)."
}}

# Applied once; the marker preserves any later manual edits by the user.
ensure_cryptodisk_flag() {{
    if [ ! -f "$CONFIG" ]; then
        return 0
    fi

    if grep -qF "$CONFIG_MARKER" "$CONFIG"; then
        echo "GRUB_BTRFS_ENABLE_CRYPTODISK already managed in $CONFIG."
        return 0
    fi

    if grep -Eq '^#?[[:space:]]*GRUB_BTRFS_ENABLE_CRYPTODISK=' "$CONFIG"; then
        sed -i -E "s|^#?[[:space:]]*GRUB_BTRFS_ENABLE_CRYPTODISK=.*|GRUB_BTRFS_ENABLE_CRYPTODISK=\"$CRYPTODISK\" $CONFIG_MARKER|" "$CONFIG"
    else
        printf '\nGRUB_BTRFS_ENABLE_CRYPTODISK="%s" %s\n' "$CRYPTODISK" "$CONFIG_MARKER" >>"$CONFIG"
    fi

    echo "Set GRUB_BTRFS_ENABLE_CRYPTODISK=\"$CRYPTODISK\" in $CONFIG."
}}

# Pin the LUKS container GRUB unlocks for snapshot entries. Appends a default
# to the existing extraction rather than rewriting it, so an upstream rewrite
# of that line degrades to `cryptomount -a` instead of corrupting the script.
ensure_crypt_source_fallback() {{
    [ -n "$BOOT_LUKS_UUID" ] || return 0
    [ -f "$TARGET" ] || return 0

    if grep -qF "$SOURCE_MARKER" "$TARGET"; then
        echo "crypt_source fallback already applied to $TARGET."
        return 0
    fi

    if ! grep -q "cryptdevice=" "$TARGET"; then
        echo "No cryptdevice extraction in $TARGET; skipping crypt_source fallback." >&2
        return 0
    fi

    sed -i "/grep -o -P .cryptdevice=/a crypt_source=\"\${{crypt_source:-UUID=$BOOT_LUKS_UUID}}\" $SOURCE_MARKER" "$TARGET"

    if ! grep -qF "$SOURCE_MARKER" "$TARGET"; then
        echo "ERROR: crypt_source fallback did not apply to $TARGET" >&2
        exit 1
    fi

    echo "Pinned snapshot cryptomount to UUID=$BOOT_LUKS_UUID in $TARGET."
}}

patch_snapshots_script
ensure_cryptodisk_flag
ensure_crypt_source_fallback
"##,
        cryptodisk = cryptodisk,
        boot_luks_uuid = boot_luks_uuid,
    );

    let script_dir = format!("{}/usr/local/bin", install_root);
    fs::create_dir_all(&script_dir)?;
    let script_path = format!("{}/patch-grub-btrfs-integrity", script_dir);
    fs::write(&script_path, &script)?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    let hook_content = r#"[Trigger]
Operation = Install
Operation = Upgrade
Type = Path
Target = etc/grub.d/41_snapshots-btrfs

[Action]
Description = Adapting grub-btrfs to Deploytix encrypted layout...
When = PostTransaction
Exec = /usr/local/bin/patch-grub-btrfs-integrity
Depends = grub-btrfs
"#;

    let hooks_dir = format!("{}/etc/pacman.d/hooks", install_root);
    fs::create_dir_all(&hooks_dir)?;
    let hook_path = format!("{}/91-patch-grub-btrfs.hook", hooks_dir);
    fs::write(&hook_path, hook_content)?;

    info!("Created grub-btrfs compat patch at /usr/local/bin/patch-grub-btrfs-integrity");
    info!("Created pacman hook at /etc/pacman.d/hooks/91-patch-grub-btrfs.hook");
    Ok(())
}

/// Configure GRUB defaults
/// For encrypted systems, pass luks_uuid and mapper_name
/// uses_subvolumes indicates if the layout uses btrfs subvolumes (for rootflags)
#[allow(clippy::too_many_arguments)]
fn configure_grub_defaults(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    root_or_luks_uuid: &str,
    mapper_name: Option<&str>,
    uses_subvolumes: bool,
    boot_encryption: bool,
    install_root: &str,
    swap_uuid: Option<&str>,
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
        if uses_subvolumes {
            cmdline_parts.push("rootflags=subvol=@".to_string());
        }
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
        if let Some(uuid) = swap_uuid {
            cmdline_parts.push(format!("resume=UUID={}", uuid));
        }
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
    swap_uuid: Option<&str>,
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
        if let Some(uuid) = swap_uuid {
            cmdline_parts.push(format!("resume=UUID={}", uuid));
        }
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

/// Placeholder root hash written into the GRUB cmdline at install time; the real
/// per-slot verity root hash is sed-patched in during finalize.
pub const AB_ROOTHASH_PLACEHOLDER: &str = "0";

/// Configure `/etc/default/grub` for the LVM immutable A/B (dm-verity) layout.
///
/// The `verity-ab` initramfs hook mounts the active slot's dm-verity root, so the
/// cmdline carries `deploytix.slot=A` and a `deploytix.roothash=` placeholder
/// (patched at finalize) plus `cryptdevice=`/`cryptkey=` when encrypted. `root=`
/// names the verity mapper for compatibility even though the hook does the mount.
fn configure_grub_defaults_lvm_ab(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    luks_uuid: Option<&str>,
    install_root: &str,
    swap_uuid: Option<&str>,
) -> Result<()> {
    let grub_default_path = format!("{}/etc/default/grub", install_root);
    let verity_mapper = crate::configure::verity::VERITY_MAPPER_NAME;

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure /etc/default/grub for LVM immutable A/B");
        println!(
            "    deploytix.slot=A deploytix.roothash={} root=/dev/mapper/{}",
            AB_ROOTHASH_PLACEHOLDER, verity_mapper
        );
        return Ok(());
    }

    let mut cmdline_parts = vec!["quiet".to_string()];
    if let Some(uuid) = luks_uuid {
        cmdline_parts.push(format!("cryptdevice=UUID={}:Crypt-LVM", uuid));
        if config.disk.boot_encryption {
            cmdline_parts.push("cryptkey=rootfs:/etc/cryptsetup-keys.d/cryptlvm.key".to_string());
        }
    }
    // Nominal root device (the verity-ab hook performs the real mount).
    cmdline_parts.push(format!("root=/dev/mapper/{}", verity_mapper));
    // A/B boot pointer — starts on slot A; roothash patched at finalize.
    cmdline_parts.push("deploytix.slot=A".to_string());
    cmdline_parts.push(format!("deploytix.roothash={}", AB_ROOTHASH_PLACEHOLDER));
    // Immutable root.
    cmdline_parts.push("ro".to_string());
    if config.system.hibernation {
        if let Some(uuid) = swap_uuid {
            cmdline_parts.push(format!("resume=UUID={}", uuid));
        }
    }

    let cmdline = cmdline_parts.join(" ");
    let mut content = format!(
        r#"# GRUB boot loader configuration
# Generated by Deploytix - LVM immutable A/B (dm-verity) layout

GRUB_DEFAULT=0
GRUB_TIMEOUT=5
GRUB_DISTRIBUTOR="Artix"
GRUB_CMDLINE_LINUX_DEFAULT="{}"
"#,
        cmdline
    );
    if config.disk.boot_encryption {
        content.push_str("GRUB_ENABLE_CRYPTODISK=y\n");
    }

    fs::create_dir_all(format!("{}/etc/default", install_root))?;
    fs::write(&grub_default_path, content)?;
    info!("GRUB defaults (immutable A/B) written to /etc/default/grub");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_install_root(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "deploytix-bootloader-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn uses_standalone_grub_requires_sbctl_secureboot_and_encryption() {
        let mut cfg = DeploymentConfig::sample();
        cfg.system.secureboot = true;
        cfg.system.secureboot_method = SecureBootMethod::Sbctl;
        cfg.disk.encryption = true;
        assert!(uses_standalone_grub(&cfg));

        cfg.disk.encryption = false;
        assert!(!uses_standalone_grub(&cfg));

        cfg.disk.encryption = true;
        cfg.system.secureboot = false;
        assert!(!uses_standalone_grub(&cfg));

        cfg.system.secureboot = true;
        cfg.system.secureboot_method = SecureBootMethod::Shim;
        assert!(!uses_standalone_grub(&cfg));
    }

    #[test]
    fn grub_btrfs_compat_creates_patch_script_and_hook() {
        let root = temp_install_root("compat");
        let cmd = CommandRunner::new(false);
        let mut cfg = DeploymentConfig::sample();
        cfg.disk.encryption = true;
        cfg.disk.boot_encryption = false;

        create_grub_btrfs_compat(&cmd, &cfg, None, &root).unwrap();

        let script_path = format!("{}/usr/local/bin/patch-grub-btrfs-integrity", root);
        let script = fs::read_to_string(&script_path).unwrap();
        assert!(script.contains("DEPLOYTIX-INTEGRITY-PATCH-V1"));
        assert!(
            script.contains(r#"CRYPTODISK="false""#),
            "Unencrypted /boot must disable the grub-btrfs cryptodisk branch"
        );
        assert!(
            script.contains(r#"BOOT_LUKS_UUID="""#),
            "Unencrypted /boot must leave the crypt_source fallback inert"
        );
        assert!(
            script.contains(r#"|| blkid -s UUID -o value "${root_device}""#),
            "root_uuid must gain a blkid fallback"
        );
        let mode = fs::metadata(&script_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "patch script must be executable");

        // Skip silently when no bash is available (syntax check is best-effort)
        let status = std::process::Command::new("bash")
            .arg("-n")
            .arg(&script_path)
            .status();
        if let Ok(status) = status {
            assert!(
                status.success(),
                "generated patch script fails bash -n syntax check"
            );
        }

        let hook = fs::read_to_string(format!(
            "{}/etc/pacman.d/hooks/91-patch-grub-btrfs.hook",
            root
        ))
        .unwrap();
        assert!(hook.contains("Target = etc/grub.d/41_snapshots-btrfs"));
        assert!(hook.contains("Depends = grub-btrfs"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn grub_btrfs_compat_enables_cryptodisk_for_encrypted_boot() {
        let root = temp_install_root("compat-cryptoboot");
        let cmd = CommandRunner::new(false);
        let mut cfg = DeploymentConfig::sample();
        cfg.disk.encryption = true;
        cfg.disk.boot_encryption = true;

        const BOOT_UUID: &str = "11111111-2222-3333-4444-555555555555";
        create_grub_btrfs_compat(&cmd, &cfg, Some(BOOT_UUID), &root).unwrap();

        let script_path = format!("{}/usr/local/bin/patch-grub-btrfs-integrity", root);
        let script = fs::read_to_string(&script_path).unwrap();
        assert!(
            script.contains(r#"CRYPTODISK="true""#),
            "Encrypted /boot requires the grub-btrfs cryptodisk branch"
        );

        // Without this the generator falls back to `cryptomount -a`, prompting
        // for every LUKS container on the disk before a snapshot entry boots.
        assert!(
            script.contains(&format!(r#"BOOT_LUKS_UUID="{}""#, BOOT_UUID)),
            "Encrypted /boot must pin the boot container UUID"
        );
        assert!(
            script.contains("DEPLOYTIX-CRYPTSOURCE-V1"),
            "crypt_source fallback must carry its idempotence marker"
        );
        assert!(
            script.contains(r#"crypt_source=\"\${crypt_source:-UUID=$BOOT_LUKS_UUID}\""#),
            "fallback must default crypt_source without clobbering the extraction"
        );

        let status = std::process::Command::new("bash")
            .arg("-n")
            .arg(&script_path)
            .status();
        if let Ok(status) = status {
            assert!(
                status.success(),
                "generated patch script fails bash -n syntax check"
            );
        }

        let _ = fs::remove_dir_all(&root);
    }
}
