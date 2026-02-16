//! mkinitcpio configuration and hook construction

use crate::config::{DeploymentConfig, Filesystem, PartitionLayout};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// Construct the MODULES array based on configuration
pub fn construct_modules(config: &DeploymentConfig) -> Vec<String> {
    let mut modules = Vec::new();

    // Filesystem modules
    match config.disk.filesystem {
        Filesystem::Btrfs => modules.push("btrfs".to_string()),
        Filesystem::Ext4 => modules.push("ext4".to_string()),
        Filesystem::Xfs => modules.push("xfs".to_string()),
        Filesystem::F2fs => modules.push("f2fs".to_string()),
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
        modules.extend([
            "dm_crypt".to_string(),
            "dm_mod".to_string(),
        ]);
    }

    modules
}

/// Construct the HOOKS array based on configuration
pub fn construct_hooks(config: &DeploymentConfig) -> Vec<String> {
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

    // lvm2 hook provides device-mapper support required by encryption hooks
    if config.disk.encryption {
        hooks.push("lvm2".to_string());
    }

    // For CryptoSubvolume layout, use custom hooks
    if config.disk.layout == PartitionLayout::CryptoSubvolume && config.disk.encryption {
        // Custom hooks handle encryption and mounting
        hooks.push("crypttab-unlock".to_string());
        hooks.push("mountcrypt".to_string());
        // Note: filesystems hook is NOT needed when using mountcrypt
        // as mountcrypt handles all mounting
    } else {
        // Standard encryption hook for other layouts
        if config.disk.encryption {
            hooks.push("encrypt".to_string());
        }

        // Filesystem-specific hooks
        if config.disk.filesystem == Filesystem::Btrfs {
            hooks.push("btrfs".to_string());
        }

        // Core hooks
        hooks.push("filesystems".to_string());
        hooks.push("fsck".to_string());

        // Separate /usr partition hook
        if config.disk.layout == PartitionLayout::Standard {
            hooks.push("usr".to_string());
        }

        // Resume hook for hibernation
        if config.system.hibernation {
            // Insert resume before filesystems
            let filesystems_idx = hooks.iter().position(|h| h == "filesystems").unwrap_or(hooks.len());
            hooks.insert(filesystems_idx, "resume".to_string());
        }
    }

    hooks
}

/// Construct BINARIES array
pub fn construct_binaries(_config: &DeploymentConfig) -> Vec<String> {
    vec!["lsblk".to_string()]
}

/// Construct FILES array
pub fn construct_files(config: &DeploymentConfig) -> Vec<String> {
    let mut files = Vec::new();

    // Include /etc/crypttab in the initramfs so the crypttab-unlock hook can
    // parse it at early boot and open LUKS containers.
    if config.disk.encryption && config.disk.layout == PartitionLayout::CryptoSubvolume {
        files.push("/etc/crypttab".to_string());

        // Include keyfiles for automatic unlocking during initramfs
        // These are referenced by /etc/crypttab entries
        files.push("/etc/cryptsetup-keys.d/cryptroot.key".to_string());
        files.push("/etc/cryptsetup-keys.d/cryptusr.key".to_string());
        files.push("/etc/cryptsetup-keys.d/cryptvar.key".to_string());
        files.push("/etc/cryptsetup-keys.d/crypthome.key".to_string());

        // Include boot keyfile when boot encryption is enabled
        if config.disk.boot_encryption {
            files.push("/etc/cryptsetup-keys.d/cryptboot.key".to_string());
        }
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
    if config.disk.encryption && config.disk.layout == PartitionLayout::CryptoSubvolume {
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
HOOKS="{}"

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
    info!("Configuring mkinitcpio with {} hooks: [{}]", hooks.len(), hooks.join(", "));
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
    fn crypto_subvolume_uses_custom_hooks() {
        let cfg = config_with(PartitionLayout::CryptoSubvolume, true);
        let hooks = construct_hooks(&cfg);
        assert!(hooks.contains(&"crypttab-unlock".to_string()));
        assert!(hooks.contains(&"mountcrypt".to_string()));
        // Must NOT include the standard encrypt or filesystems hooks
        assert!(!hooks.contains(&"encrypt".to_string()));
        assert!(!hooks.contains(&"filesystems".to_string()));
    }

    #[test]
    fn standard_encrypted_uses_encrypt_hook() {
        let cfg = config_with(PartitionLayout::Standard, true);
        let hooks = construct_hooks(&cfg);
        assert!(hooks.contains(&"encrypt".to_string()));
        assert!(hooks.contains(&"filesystems".to_string()));
        assert!(hooks.contains(&"usr".to_string()));
        // Must NOT include custom hooks
        assert!(!hooks.contains(&"crypttab-unlock".to_string()));
        assert!(!hooks.contains(&"mountcrypt".to_string()));
    }

    #[test]
    fn minimal_encrypted_uses_encrypt_hook() {
        let cfg = config_with(PartitionLayout::Minimal, true);
        let hooks = construct_hooks(&cfg);
        assert!(hooks.contains(&"encrypt".to_string()));
        assert!(hooks.contains(&"filesystems".to_string()));
        assert!(!hooks.contains(&"crypttab-unlock".to_string()));
        assert!(!hooks.contains(&"mountcrypt".to_string()));
        assert!(!hooks.contains(&"usr".to_string()));
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
    fn crypto_subvolume_hook_ordering() {
        let cfg = config_with(PartitionLayout::CryptoSubvolume, true);
        let hooks = construct_hooks(&cfg);
        let lvm2_pos = hooks.iter().position(|h| h == "lvm2").unwrap();
        let unlock_pos = hooks.iter().position(|h| h == "crypttab-unlock").unwrap();
        let mount_pos = hooks.iter().position(|h| h == "mountcrypt").unwrap();
        // lvm2 must come before crypttab-unlock, which must come before mountcrypt
        assert!(lvm2_pos < unlock_pos, "lvm2 must precede crypttab-unlock");
        assert!(unlock_pos < mount_pos, "crypttab-unlock must precede mountcrypt");
    }

    #[test]
    fn crypto_subvolume_files_include_crypttab_and_keyfiles() {
        let cfg = config_with(PartitionLayout::CryptoSubvolume, true);
        let files = construct_files(&cfg);
        assert!(files.contains(&"/etc/crypttab".to_string()));
        assert!(files.contains(&"/etc/cryptsetup-keys.d/cryptroot.key".to_string()));
        assert!(files.contains(&"/etc/cryptsetup-keys.d/cryptusr.key".to_string()));
        assert!(files.contains(&"/etc/cryptsetup-keys.d/cryptvar.key".to_string()));
        assert!(files.contains(&"/etc/cryptsetup-keys.d/crypthome.key".to_string()));
    }

    #[test]
    fn crypto_subvolume_boot_encryption_includes_boot_keyfile() {
        let mut cfg = config_with(PartitionLayout::CryptoSubvolume, true);
        cfg.disk.boot_encryption = true;
        let files = construct_files(&cfg);
        assert!(files.contains(&"/etc/cryptsetup-keys.d/cryptboot.key".to_string()));
    }

    #[test]
    fn standard_encrypted_no_files() {
        let cfg = config_with(PartitionLayout::Standard, true);
        let files = construct_files(&cfg);
        assert!(files.is_empty(), "Standard layout should not embed crypttab/keyfiles");
    }
}
