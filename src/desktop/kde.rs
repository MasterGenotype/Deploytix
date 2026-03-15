//! KDE Plasma desktop environment installer

use crate::config::{DeploymentConfig, InitSystem};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// KDE Plasma packages (individual packages instead of plasma-meta to avoid systemd conflicts on Artix)
const KDE_PACKAGES: &[&str] = &[
    "plasma-desktop",
    "plasma-workspace",
    "konsole",
    "dolphin",
    "sddm",
    // KDE audio integration
    "plasma-pa",
    "kpipewire",
    // Desktop integration
    "bluez",
    "power-profiles-daemon",
    // KDE system integration
    "powerdevil",
    "bluedevil",
    "kde-gtk-config",
    "kdeplasma-addons",
    "kscreen",
    "kwallet-pam",
    "xdg-desktop-portal-kde",
    // Application store
    "discover",
    "flatpak",
    "kate",
];

/// s6-specific service packages for KDE
const KDE_S6_PACKAGES: &[&str] = &["bluez-s6", "power-profiles-daemon-s6"];

/// Install KDE Plasma desktop environment
pub fn install(cmd: &CommandRunner, config: &DeploymentConfig, install_root: &str) -> Result<()> {
    info!("Installing KDE Plasma desktop environment");

    let use_session_switching = config.packages.install_session_switching;

    // Build package list, excluding sddm when session switching is active (greetd replaces it)
    let mut packages: Vec<&str> = KDE_PACKAGES
        .iter()
        .filter(|&&pkg| !(use_session_switching && pkg == "sddm"))
        .copied()
        .collect();

    // Add s6-specific service packages
    if config.system.init == InitSystem::S6 {
        packages.extend(KDE_S6_PACKAGES);
    }

    if cmd.is_dry_run() {
        println!("  [dry-run] Would install KDE packages: {:?}", packages);
        if !use_session_switching {
            let sddm_service = format!("sddm-{}", config.system.init);
            println!("  [dry-run] Would install sddm service: {}", sddm_service);
        } else {
            println!("  [dry-run] Skipping SDDM (session switching uses greetd)");
        }
        if config.system.init == InitSystem::S6 {
            println!(
                "  [dry-run] Would install s6 service packages: {:?}",
                KDE_S6_PACKAGES
            );
        }
        return Ok(());
    }

    // Install packages via pacman in chroot
    let pkg_list = packages.join(" ");
    let mut install_cmd = if use_session_switching {
        // No sddm or sddm-{init} when session switching is active
        format!("pacman -S --noconfirm {}", pkg_list)
    } else {
        let sddm_service = format!("sddm-{}", config.system.init);
        format!("pacman -S --noconfirm {} {}", pkg_list, sddm_service)
    };

    // Add init-specific service packages for non-s6 init systems
    if config.system.init != InitSystem::S6 {
        let bluez_service = format!("bluez-{}", config.system.init);
        let power_service = format!("power-profiles-daemon-{}", config.system.init);
        install_cmd = format!("{} {} {}", install_cmd, bluez_service, power_service);
    }

    cmd.run_in_chroot(install_root, &install_cmd)?;

    // Configure SDDM (skip when session switching uses greetd instead)
    if !use_session_switching {
        configure_sddm(cmd, install_root)?;
    }

    // Create .xinitrc for startx fallback
    let username = &config.user.name;
    let xinitrc_path = format!("{}/home/{}/.xinitrc", install_root, username);
    fs::write(&xinitrc_path, "exec startplasma-x11\n")?;

    info!("KDE Plasma installation complete");
    Ok(())
}

/// Generate KDE-specific desktop file content
pub fn desktop_file_content(bindir: &str) -> String {
    format!(
        r#"[Desktop Entry]
Type=Application
Name=Deploytix
GenericName=Artix Linux Installer
Comment=Automated Artix Linux deployment installer
Exec=pkexec {}/deploytix-gui
Icon=system-software-install
NoDisplay=false
StartupNotify=true
Terminal=false
Categories=System;Settings;Qt;KDE;
Keywords=linux;installer;artix;deployment;kde;plasma;
X-KDE-SubstituteUID=false
X-DBUS-StartupType=
X-KDE-StartupNotify=true
"#,
        bindir
    )
}

/// Configure SDDM display manager
fn configure_sddm(_cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Configuring SDDM");

    let sddm_conf_dir = format!("{}/etc/sddm.conf.d", install_root);
    fs::create_dir_all(&sddm_conf_dir)?;

    // Basic SDDM configuration
    let sddm_conf = r#"[Theme]
Current=breeze

[Users]
MaximumUid=60000
MinimumUid=1000
"#;

    fs::write(format!("{}/kde_settings.conf", sddm_conf_dir), sddm_conf)?;

    Ok(())
}
