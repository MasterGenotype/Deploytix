//! XFCE desktop environment installer

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// XFCE packages
const XFCE_PACKAGES: &[&str] = &["xfce4", "xfce4-goodies", "lightdm", "lightdm-gtk-greeter"];

/// Install XFCE desktop environment
pub fn install(cmd: &CommandRunner, config: &DeploymentConfig, install_root: &str) -> Result<()> {
    info!("Installing XFCE desktop environment");

    // Get init-specific lightdm package
    let lightdm_service = format!("lightdm-{}", config.system.init);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install XFCE packages: {:?}",
            XFCE_PACKAGES
        );
        println!(
            "  [dry-run] Would install lightdm service: {}",
            lightdm_service
        );
        return Ok(());
    }

    // Install packages
    let pkg_list = XFCE_PACKAGES.join(" ");
    let install_cmd = format!("pacman -S --noconfirm {} {}", pkg_list, lightdm_service);
    cmd.run_in_chroot(install_root, &install_cmd)?;

    // Create .xinitrc for startx fallback
    let username = &config.user.name;
    let xinitrc_path = format!("{}/home/{}/.xinitrc", install_root, username);
    fs::write(&xinitrc_path, "exec startxfce4\n")?;

    info!("XFCE installation complete");
    Ok(())
}

/// Generate XFCE-specific desktop file content
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
Categories=System;Settings;XFCE;GTK;
Keywords=linux;installer;artix;deployment;xfce;
X-XFCE-Category=SystemSetup
"#,
        bindir
    )
}
