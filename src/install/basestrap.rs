//! Basestrap wrapper for base system installation

use crate::config::{DeploymentConfig, DesktopEnvironment, DnsProvider, Filesystem, NetworkBackend};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use tracing::info;

/// Build the package list for basestrap
pub fn build_package_list(config: &DeploymentConfig) -> Vec<String> {
    let mut packages = Vec::new();

    // Base system
    packages.extend([
        "base".to_string(),
        "base-devel".to_string(),
        config.system.init.base_package().to_string(),
    ]);

    // Kernel and firmware
    packages.extend([
        "linux-firmware".to_string(),
        "linux-zen".to_string(),
        "linux-zen-headers".to_string(),
    ]);

    // Filesystem tools
    packages.push("btrfs-progs".to_string());
    match config.disk.filesystem {
        Filesystem::Ext4 => packages.push("e2fsprogs".to_string()),
        Filesystem::Xfs => packages.push("xfsprogs".to_string()),
        Filesystem::F2fs => packages.push("f2fs-tools".to_string()),
        Filesystem::Btrfs => {} // Already added
    }

    // Bootloader
    packages.extend(["efibootmgr".to_string(), "grub".to_string()]);

    // Essential tools
    packages.extend([
        "git".to_string(),
        "nano".to_string(),
        "curl".to_string(),
        "wget".to_string(),
        "mkinitcpio".to_string(),
        "openssl".to_string(),
    ]);

    // Build tools
    packages.extend([
        "gcc".to_string(),
        "rustup".to_string(),
    ]);

    // Network packages based on config
    match config.network.backend {
        NetworkBackend::Iwd => {
            packages.push("iwd".to_string());
            // Add init-specific service package
            let service_pkg = format!("iwd-{}", config.system.init);
            packages.push(service_pkg);
        }
        NetworkBackend::NetworkManager => {
            packages.extend([
                "networkmanager".to_string(),
                "iwd".to_string(),
            ]);
            let service_pkg = format!("networkmanager-{}", config.system.init);
            packages.push(service_pkg);
        }
        NetworkBackend::Connman => {
            packages.push("connman".to_string());
            let service_pkg = format!("connman-{}", config.system.init);
            packages.push(service_pkg);
        }
    }

    // DNS
    match config.network.dns {
        DnsProvider::DnscryptProxy => {
            packages.push("dnscrypt-proxy".to_string());
            let service_pkg = format!("dnscrypt-proxy-{}", config.system.init);
            packages.push(service_pkg);
            packages.push("openresolv".to_string());
        }
        DnsProvider::Systemd => {
            // systemd-resolved is part of systemd, not available on Artix
        }
        DnsProvider::None => {}
    }

    // Seat management
    packages.push("seatd".to_string());
    let seatd_service = format!("seatd-{}", config.system.init);
    packages.push(seatd_service);

    // Desktop environment prerequisites (display server, etc.)
    if config.desktop.environment != DesktopEnvironment::None {
        packages.extend([
            "xorg-server".to_string(),
            "xorg-xinit".to_string(),
        ]);
    }

    // Encryption tools (if enabled)
    if config.disk.encryption {
        packages.push("cryptsetup".to_string());
    }

    packages
}

/// Run basestrap to install the base system
pub fn run_basestrap(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let packages = build_package_list(config);

    info!("Installing {} packages with basestrap", packages.len());

    // Build argument list
    let mut args = vec![install_root];
    let pkg_refs: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();
    args.extend(pkg_refs);

    cmd.run("basestrap", &args)
        .map(|_| ())
        .map_err(|e| DeploytixError::CommandFailed {
            command: "basestrap".to_string(),
            stderr: e.to_string(),
        })
}
