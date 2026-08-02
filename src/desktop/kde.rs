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

    // Display manager packages and configuration are handled centrally
    // (configure::services / configure::display_manager) based on
    // desktop.display_manager.
    let mut packages: Vec<&str> = KDE_PACKAGES.to_vec();

    // Add s6-specific service packages
    if config.system.init == InitSystem::S6 {
        packages.extend(KDE_S6_PACKAGES);
    }

    if cmd.is_dry_run() {
        println!("  [dry-run] Would install KDE packages: {:?}", packages);
        if config.system.init == InitSystem::S6 {
            println!(
                "  [dry-run] Would install s6 service packages: {:?}",
                KDE_S6_PACKAGES
            );
        }
        return Ok(());
    }

    // Build the full package list (including init-specific service
    // pkgs) up front so we can preflight resolution before pacman runs.
    let mut all_pkgs: Vec<String> = packages.iter().map(|s| (*s).to_string()).collect();
    if config.system.init != InitSystem::S6 {
        all_pkgs.push(format!("bluez-{}", config.system.init));
        all_pkgs.push(format!("power-profiles-daemon-{}", config.system.init));
    }

    let install_cmd = format!("pacman -S --noconfirm {}", all_pkgs.join(" "));
    crate::configure::packages::pacman_install_chroot(cmd, install_root, &install_cmd)?;

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
