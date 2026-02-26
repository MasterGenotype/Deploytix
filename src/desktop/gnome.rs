//! GNOME desktop environment installer

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// GNOME packages
const GNOME_PACKAGES: &[&str] = &["gnome", "gnome-extra", "gdm"];

/// Install GNOME desktop environment
pub fn install(cmd: &CommandRunner, config: &DeploymentConfig, install_root: &str) -> Result<()> {
    info!("Installing GNOME desktop environment");

    // Get init-specific gdm package
    let gdm_service = format!("gdm-{}", config.system.init);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install GNOME packages: {:?}",
            GNOME_PACKAGES
        );
        println!("  [dry-run] Would install gdm service: {}", gdm_service);
        return Ok(());
    }

    // Install packages
    let pkg_list = GNOME_PACKAGES.join(" ");
    let install_cmd = format!("pacman -S --noconfirm {} {}", pkg_list, gdm_service);
    cmd.run_in_chroot(install_root, &install_cmd)?;

    // Create .xinitrc for startx fallback
    let username = &config.user.name;
    let xinitrc_path = format!("{}/home/{}/.xinitrc", install_root, username);
    fs::write(&xinitrc_path, "exec gnome-session\n")?;

    info!("GNOME installation complete");
    Ok(())
}

/// Generate GNOME-specific desktop file content
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
Categories=System;Settings;GNOME;GTK;
Keywords=linux;installer;artix;deployment;gnome;
X-GNOME-UsesNotifications=true
X-GNOME-Autostart-Phase=Application
"#,
        bindir
    )
}
