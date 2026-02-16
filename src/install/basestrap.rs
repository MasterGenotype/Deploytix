//! Basestrap wrapper for base system installation

use crate::config::{DeploymentConfig, DesktopEnvironment, Filesystem, NetworkBackend};
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

    // For s6, pre-select providers to avoid interactive prompts
    if config.system.init == crate::config::InitSystem::S6 {
        // D-Bus provider for s6; no elogind, use seatd for seats
        packages.push("dbus-s6".to_string());
        // no elogind-s6
        // Core s6 service packages
        packages.push("networkmanager-s6".to_string());
        packages.push("seatd-s6".to_string());
        packages.push("iwd-s6".to_string());
    }

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
    packages.extend(["gcc".to_string(), "rustup".to_string()]);

    // Network packages based on config
    match config.network.backend {
        NetworkBackend::Iwd => {
            packages.extend(["iwd".to_string(), "openresolv".to_string()]);
            if config.system.init != crate::config::InitSystem::S6 {
                let service_pkg = format!("iwd-{}", config.system.init);
                packages.push(service_pkg);
            }
        }
        NetworkBackend::NetworkManager => {
            packages.extend([
                "networkmanager".to_string(),
                // Default to iwd backend; wpa_supplicant can be added later if desired
                "iwd".to_string(),
                "openresolv".to_string(),
            ]);
            if config.system.init != crate::config::InitSystem::S6 {
                let nm_service_pkg = format!("networkmanager-{}", config.system.init);
                let iwd_service_pkg = format!("iwd-{}", config.system.init);
                packages.push(nm_service_pkg);
                packages.push(iwd_service_pkg);
            }
            // Add nm-applet for desktop environments
            if config.desktop.environment != DesktopEnvironment::None {
                packages.push("network-manager-applet".to_string());
            }
        }
    }

    // Desktop environment prerequisites (display server, seat management, display manager, audio)
    if config.desktop.environment != DesktopEnvironment::None {
        packages.extend([
            // Display
            "xorg-server".to_string(),
            "xorg-xinit".to_string(),
            "seatd".to_string(),
            // Audio - ALSA base
            "alsa-utils".to_string(),
            "alsa-tools".to_string(),
            // Audio - PipeWire (modern audio server)
            "pipewire".to_string(),
            "wireplumber".to_string(),
            "pipewire-pulse".to_string(),
            "pipewire-alsa".to_string(),
        ]);
        if config.system.init == crate::config::InitSystem::S6 {
            // Official s6 service packages from Artix repos
            packages.push("alsa-utils-s6".to_string());
        } else {
            let seatd_service = format!("seatd-{}", config.system.init);
            let greetd_service = format!("greetd-{}", config.system.init);
            packages.push(seatd_service);
            packages.push(greetd_service);
        }
    }

    // Encryption tools (if enabled)
    if config.disk.encryption {
        packages.push("cryptsetup".to_string());
        // lvm2 provides device-mapper, required by mkinitcpio encrypt/lvm2 hooks
        packages.push("lvm2".to_string());
    }

    // thin-provisioning-tools for LVM thin provisioning
    if config.disk.layout == crate::config::PartitionLayout::LvmThin {
        packages.push("thin-provisioning-tools".to_string());
    }

    // SecureBoot tools (if enabled)
    if config.system.secureboot {
        match config.system.secureboot_method {
            crate::config::SecureBootMethod::Sbctl => {
                packages.push("sbctl".to_string());
            }
            crate::config::SecureBootMethod::ManualKeys | crate::config::SecureBootMethod::Shim => {
                packages.push("sbsigntools".to_string());
                packages.push("efitools".to_string());
            }
        }
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

    info!(
        "Installing {} packages with basestrap to {}",
        packages.len(),
        install_root
    );

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
