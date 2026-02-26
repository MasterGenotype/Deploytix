//! mkinitcpio configuration and hook construction

use crate::config::{DeploymentConfig, Filesystem, PartitionLayout};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// Construct MODULES array based on configuration
pub fn construct_modules(config: &DeploymentConfig) -> Vec<String> {
    let mut modules = Vec::new();

    // Data filesystem modules
    match config.disk.filesystem {
        Filesystem::Btrfs => modules.push("btrfs".to_string()),
        Filesystem::Ext4 => modules.push("ext4".to_string()),
        Filesystem::Xfs => modules.push("xfs".to_string()),
        Filesystem::Zfs => {} // ZFS is loaded via the zfs hook, not as a static module
        Filesystem::F2fs => modules.push("f2fs".to_string()),
    }

    // Boot filesystem modules (if different from data filesystem)
    match config.disk.boot_filesystem {
        Filesystem::Btrfs if config.disk.filesystem != Filesystem::Btrfs => {
            modules.push("btrfs".to_string());
        }
        Filesystem::Ext4 if config.disk.filesystem != Filesystem::Ext4 => {
            modules.push("ext4".to_string());
        }
        Filesystem::Xfs if config.disk.filesystem != Filesystem::Xfs => {
            modules.push("xfs".to_string());
        }
        Filesystem::F2fs if config.disk.filesystem != Filesystem::F2fs => {
            modules.push("f2fs".to_string());
        }
        _ => {} // same as data filesystem or ZFS (hook-based)
    }

    // Always include vfat and dependencies for EFI partition mounting in initramfs
    modules.extend([
        "vfat".to_string(),
        "fat".to_string(),
        "nls_cp437".to_string(),
        "nls_iso8859_1".to_string(),
    ]);

    // Encryption modules
    if config.disk.encryption {
        modules.extend(["dm_crypt".to_string(), "dm_mod".to_string()]);

        // dm-integrity module for per-sector integrity protection
        if config.disk.integrity {
            modules.push("dm_integrity".to_string());
        }
    }

    // LVM thin provisioning modules (feature-driven)
    if config.disk.use_lvm_thin {
        modules.extend(["dm_thin_pool".to_string()]);
    }

    modules
}

/// Construct the HOOKS array based on configuration.
///
/// Hook selection is feature-driven, not layout-driven:
/// - `encryption` → `lvm2` + either `encrypt` (single LUKS) or `crypttab-unlock` + `mountcrypt` (multi-LUKS)
/// - `use_lvm_thin` → `lvm2` hook (already added by encryption or standalone)
/// - `boot_encryption` → `crypttab-unlock` (if not already added)
/// - `btrfs` → `btrfs` hook
pub fn construct_hooks(config: &DeploymentConfig) -> Vec<String> {
    let uses_lvm_thin = config.disk.use_lvm_thin;
    let uses_encryption = config.disk.encryption;
    let uses_multi_luks = uses_encryption && !uses_lvm_thin;

    let mut hooks = vec![
        "base".to_string(),
        "udev".to_string(),
        "autodetect".to_string(),
        "modconf".to_string(),
        "block".to_string(),
    ];

    // Keyboard/console hooks
    hooks.extend([
        "keyboard".to_string(),
        "keymap".to_string(),
        "consolefont".to_string(),
    ]);

    // lvm2 hook provides device-mapper support required by encryption and LVM
    if uses_encryption || uses_lvm_thin {
        hooks.push("lvm2".to_string());
    }

    if uses_multi_luks {
        // Multi-LUKS: custom hooks handle encryption and mounting
        hooks.push("crypttab-unlock".to_string());
        hooks.push("mountcrypt".to_string());
        // Note: filesystems hook is NOT needed when using mountcrypt
        // as mountcrypt handles all mounting
    } else if uses_lvm_thin {
        // LVM Thin: LUKS unlock (single container), then LVM activates, then filesystems
        if uses_encryption {
            hooks.push("encrypt".to_string());
        }

        // When boot encryption is enabled, add crypttab-unlock to unlock
        // the LUKS1 /boot container. The encrypt hook handles the main
        // Crypt-LVM container; crypttab-unlock handles Crypt-Boot.
        if config.disk.boot_encryption {
            hooks.push("crypttab-unlock".to_string());
        }

        // Filesystem-specific hooks
        if config.disk.filesystem == Filesystem::Btrfs {
            hooks.push("btrfs".to_string());
        }

        hooks.push("filesystems".to_string());
        hooks.push("fsck".to_string());

        // LVM thin has a separate /usr thin volume that must be mounted before init.
        hooks.push("usr".to_string());

        // Resume hook for hibernation
        if config.system.hibernation {
            let filesystems_idx = hooks
                .iter()
                .position(|h| h == "filesystems")
                .unwrap_or(hooks.len());
            hooks.insert(filesystems_idx, "resume".to_string());
        }
    } else {
        // No encryption, no LVM thin: standard hooks
        if config.disk.filesystem == Filesystem::Btrfs {
            hooks.push("btrfs".to_string());
        }

        hooks.push("filesystems".to_string());
        hooks.push("fsck".to_string());

        // Separate /usr partition hook (Standard layout has separate /usr)
        if config.disk.layout == PartitionLayout::Standard {
            hooks.push("usr".to_string());
        }

        // Resume hook for hibernation
        if config.system.hibernation {
            let filesystems_idx = hooks
                .iter()
                .position(|h| h == "filesystems")
                .unwrap_or(hooks.len());
            hooks.insert(filesystems_idx, "resume".to_string());
        }
    }

    hooks
}

/// Construct BINARIES array
pub fn construct_binaries(_config: &DeploymentConfig) -> Vec<String> {
    vec!["lsblk".to_string()]
}

/// Construct FILES array.
///
/// Includes crypttab and keyfiles in the initramfs whenever encryption is enabled,
/// regardless of the chosen layout.
pub fn construct_files(config: &DeploymentConfig) -> Vec<String> {
    let uses_lvm_thin = config.disk.use_lvm_thin;
    let uses_multi_luks = config.disk.encryption && !uses_lvm_thin;

    let mut files = Vec::new();

    // Multi-LUKS: include crypttab and per-volume keyfiles
    if uses_multi_luks {
        files.push("/etc/crypttab".to_string());

        files.push("/etc/cryptsetup-keys.d/cryptroot.key".to_string());
        files.push("/etc/cryptsetup-keys.d/cryptusr.key".to_string());
        files.push("/etc/cryptsetup-keys.d/cryptvar.key".to_string());
        files.push("/etc/cryptsetup-keys.d/crypthome.key".to_string());

        if config.disk.boot_encryption {
            files.push("/etc/cryptsetup-keys.d/cryptboot.key".to_string());
        }
    }

    // LVM thin with encryption + boot encryption: include LVM and boot keyfiles
    if uses_lvm_thin && config.disk.encryption && config.disk.boot_encryption {
        files.push("/etc/cryptsetup-keys.d/cryptlvm.key".to_string());
        files.push("/etc/crypttab".to_string());
        files.push("/etc/cryptsetup-keys.d/cryptboot.key".to_string());
    }

    files
}

/// Construct FILES array with dynamic keyfile paths
#[allow(dead_code)]
pub fn construct_files_with_keyfiles(
    config: &DeploymentConfig,
    keyfile_paths: &[String],
) -> Vec<String> {
    let mut files = Vec::new();

    // Include /etc/crypttab in the initramfs
    if config.disk.encryption {
        files.push("/etc/crypttab".to_string());

        // Include all provided keyfiles
        for keyfile in keyfile_paths {
            files.push(keyfile.clone());
        }
    }

    files
}

/// Generate mkinitcpio.conf content
pub fn generate_mkinitcpio_conf(config: &DeploymentConfig) -> String {
    let modules = construct_modules(config);
    let binaries = construct_binaries(config);
    let files = construct_files(config);
    let hooks = construct_hooks(config);

    format!(
        r#"# mkinitcpio.conf - Generated by Deploytix
# See mkinitcpio(8) for details

MODULES=({})
BINARIES=({})
FILES=({})
HOOKS=({})

# Compression
COMPRESSION="zstd"
COMPRESSION_OPTIONS=(-T0)
"#,
        modules.join(" "),
        binaries.join(" "),
        files.join(" "),
        hooks.join(" ")
    )
}

/// Configure mkinitcpio
pub fn configure_mkinitcpio(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let mkinitcpio_conf = generate_mkinitcpio_conf(config);
    let hooks = construct_hooks(config);
    info!(
        "Configuring mkinitcpio with {} hooks: [{}]",
        hooks.len(),
        hooks.join(", ")
    );
    let conf_path = format!("{}/etc/mkinitcpio.conf", install_root);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would write mkinitcpio.conf:");
        for line in mkinitcpio_conf.lines() {
            println!("    {}", line);
        }
        return Ok(());
    }

    // Backup original
    let backup_path = format!("{}.bak", conf_path);
    if fs::metadata(&conf_path).is_ok() {
        fs::copy(&conf_path, &backup_path)?;
    }

    // Write new config
    fs::write(&conf_path, mkinitcpio_conf)?;

    info!("mkinitcpio.conf written to {}", conf_path);
    Ok(())
}

/// Regenerate initramfs
#[allow(dead_code)]
pub fn regenerate_initramfs(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Regenerating initramfs");

    cmd.run_in_chroot(install_root, "mkinitcpio -P")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DeploymentConfig;

    fn config_with(layout: PartitionLayout, encryption: bool) -> DeploymentConfig {
        let mut cfg = DeploymentConfig::sample();
        cfg.disk.layout = layout;
        cfg.disk.encryption = encryption;
        if encryption {
            cfg.disk.encryption_password = Some("test".to_string());
        }
        cfg
    }

    #[test]
    fn standard_encrypted_uses_custom_hooks() {
        let cfg = config_with(PartitionLayout::Standard, true);
        let hooks = construct_hooks(&cfg);
        assert!(hooks.contains(&"crypttab-unlock".to_string()));
        assert!(hooks.contains(&"mountcrypt".to_string()));
        // Must NOT include the standard encrypt or filesystems hooks
        assert!(!hooks.contains(&"encrypt".to_string()));
        assert!(!hooks.contains(&"filesystems".to_string()));
    }

    #[test]
    fn minimal_unencrypted_has_filesystems_hook() {
        let cfg = config_with(PartitionLayout::Minimal, false);
        let hooks = construct_hooks(&cfg);
        assert!(hooks.contains(&"filesystems".to_string()));
        assert!(!hooks.contains(&"crypttab-unlock".to_string()));
        assert!(!hooks.contains(&"mountcrypt".to_string()));
        assert!(!hooks.contains(&"usr".to_string())); // Minimal doesn't have separate /usr
    }

    #[test]
    fn unencrypted_standard_has_no_encrypt_hooks() {
        let cfg = config_with(PartitionLayout::Standard, false);
        let hooks = construct_hooks(&cfg);
        assert!(!hooks.contains(&"encrypt".to_string()));
        assert!(!hooks.contains(&"crypttab-unlock".to_string()));
        assert!(!hooks.contains(&"lvm2".to_string()));
        assert!(hooks.contains(&"filesystems".to_string()));
    }

    #[test]
    fn standard_encrypted_hook_ordering() {
        let cfg = config_with(PartitionLayout::Standard, true);
        let hooks = construct_hooks(&cfg);
        let lvm2_pos = hooks.iter().position(|h| h == "lvm2").unwrap();
        let unlock_pos = hooks.iter().position(|h| h == "crypttab-unlock").unwrap();
        let mount_pos = hooks.iter().position(|h| h == "mountcrypt").unwrap();
        // lvm2 must come before crypttab-unlock, which must come before mountcrypt
        assert!(lvm2_pos < unlock_pos, "lvm2 must precede crypttab-unlock");
        assert!(
            unlock_pos < mount_pos,
            "crypttab-unlock must precede mountcrypt"
        );
    }

    #[test]
    fn standard_encrypted_files_include_crypttab_and_keyfiles() {
        let cfg = config_with(PartitionLayout::Standard, true);
        let files = construct_files(&cfg);
        assert!(files.contains(&"/etc/crypttab".to_string()));
        assert!(files.contains(&"/etc/cryptsetup-keys.d/cryptroot.key".to_string()));
        assert!(files.contains(&"/etc/cryptsetup-keys.d/cryptusr.key".to_string()));
        assert!(files.contains(&"/etc/cryptsetup-keys.d/cryptvar.key".to_string()));
        assert!(files.contains(&"/etc/cryptsetup-keys.d/crypthome.key".to_string()));
    }

    #[test]
    fn standard_encrypted_boot_encryption_includes_boot_keyfile() {
        let mut cfg = config_with(PartitionLayout::Standard, true);
        cfg.disk.boot_encryption = true;
        let files = construct_files(&cfg);
        assert!(files.contains(&"/etc/cryptsetup-keys.d/cryptboot.key".to_string()));
    }

    #[test]
    fn minimal_unencrypted_no_files() {
        let cfg = config_with(PartitionLayout::Minimal, false);
        let files = construct_files(&cfg);
        assert!(
            files.is_empty(),
            "Unencrypted Minimal layout should not embed crypttab/keyfiles"
        );
    }

    #[test]
    fn standard_encrypted_with_integrity_includes_dm_integrity_module() {
        let mut cfg = config_with(PartitionLayout::Standard, true);
        cfg.disk.integrity = true;
        let modules = construct_modules(&cfg);
        assert!(
            modules.contains(&"dm_integrity".to_string()),
            "Integrity-enabled config must include dm_integrity module"
        );
    }

    #[test]
    fn standard_encrypted_without_integrity_excludes_dm_integrity_module() {
        let cfg = config_with(PartitionLayout::Standard, true);
        let modules = construct_modules(&cfg);
        assert!(
            !modules.contains(&"dm_integrity".to_string()),
            "Non-integrity config must not include dm_integrity module"
        );
    }

    #[test]
    fn lvm_thin_encrypted_uses_encrypt_hook() {
        let mut cfg = config_with(PartitionLayout::Standard, true);
        cfg.disk.use_lvm_thin = true;
        let hooks = construct_hooks(&cfg);
        assert!(
            hooks.contains(&"encrypt".to_string()),
            "LVM thin encrypted must include encrypt hook"
        );
        assert!(
            hooks.contains(&"lvm2".to_string()),
            "LVM thin must include lvm2 hook"
        );
        assert!(
            !hooks.contains(&"crypttab-unlock".to_string()),
            "LVM thin without boot encryption should not include crypttab-unlock"
        );
    }

    #[test]
    fn lvm_thin_boot_encryption_adds_crypttab_unlock_hook() {
        let mut cfg = config_with(PartitionLayout::Standard, true);
        cfg.disk.use_lvm_thin = true;
        cfg.disk.boot_encryption = true;
        let hooks = construct_hooks(&cfg);
        assert!(
            hooks.contains(&"encrypt".to_string()),
            "LVM thin with boot encryption must still include encrypt hook"
        );
        assert!(
            hooks.contains(&"crypttab-unlock".to_string()),
            "LVM thin with boot encryption must include crypttab-unlock hook"
        );
    }

    #[test]
    fn lvm_thin_boot_encryption_includes_crypttab_and_keyfiles() {
        let mut cfg = config_with(PartitionLayout::Standard, true);
        cfg.disk.use_lvm_thin = true;
        cfg.disk.boot_encryption = true;
        let files = construct_files(&cfg);
        assert!(
            files.contains(&"/etc/cryptsetup-keys.d/cryptlvm.key".to_string()),
            "LVM thin must include LVM keyfile"
        );
        assert!(
            files.contains(&"/etc/crypttab".to_string()),
            "LVM thin with boot encryption must include crypttab"
        );
        assert!(
            files.contains(&"/etc/cryptsetup-keys.d/cryptboot.key".to_string()),
            "LVM thin with boot encryption must include boot keyfile"
        );
    }

    #[test]
    fn lvm_thin_without_boot_encryption_no_files_in_initramfs() {
        let mut cfg = config_with(PartitionLayout::Standard, true);
        cfg.disk.use_lvm_thin = true;
        let files = construct_files(&cfg);
        // Without boot_encryption, cryptlvm.key is never generated by
        // setup_lvm_thin_keyfiles, so it must not be referenced in FILES
        // (mkinitcpio would fail if the file doesn't exist on disk).
        assert!(
            !files.contains(&"/etc/cryptsetup-keys.d/cryptlvm.key".to_string()),
            "LVM thin without boot encryption must not embed non-existent cryptlvm.key"
        );
        assert!(
            !files.contains(&"/etc/crypttab".to_string()),
            "LVM thin without boot encryption should not include crypttab in initramfs"
        );
        assert!(
            !files.contains(&"/etc/cryptsetup-keys.d/cryptboot.key".to_string()),
            "LVM thin without boot encryption should not include boot keyfile"
        );
    }

    #[test]
    fn lvm_thin_includes_usr_hook() {
        let mut cfg = config_with(PartitionLayout::Standard, true);
        cfg.disk.use_lvm_thin = true;
        let hooks = construct_hooks(&cfg);
        assert!(
            hooks.contains(&"usr".to_string()),
            "LVM thin must include usr hook to mount /usr before init"
        );
        // usr hook must come after filesystems
        let filesystems_pos = hooks.iter().position(|h| h == "filesystems").unwrap();
        let usr_pos = hooks.iter().position(|h| h == "usr").unwrap();
        assert!(
            filesystems_pos < usr_pos,
            "usr hook must come after filesystems hook"
        );
    }
}
