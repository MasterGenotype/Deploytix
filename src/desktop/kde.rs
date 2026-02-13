//! KDE Plasma desktop environment installer

use crate::config::{DeploymentConfig, InitSystem};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// KDE Plasma packages
const KDE_PACKAGES: &[&str] = &[
    "plasma-meta",
    "plasma-desktop",
    "konsole",
    "dolphin",
    "sddm",
    // KDE audio integration
    "plasma-pa",
    "kpipewire",
    // Desktop integration
    "bluez",
    "power-profiles-daemon",
];

/// s6-specific service packages for KDE
const KDE_S6_PACKAGES: &[&str] = &[
    "bluez-s6",
    "power-profiles-daemon-s6",
];

/// Install KDE Plasma desktop environment
pub fn install(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    info!("Installing KDE Plasma desktop environment");

    // Get init-specific sddm package
    let sddm_service = format!("sddm-{}", config.system.init);

    // Build package list
    let mut packages: Vec<&str> = KDE_PACKAGES.to_vec();

    // Add s6-specific service packages
    if config.system.init == InitSystem::S6 {
        packages.extend(KDE_S6_PACKAGES);
    } else {
        // Add init-specific service packages for other init systems
        let bluez_service = format!("bluez-{}", config.system.init);
        let power_service = format!("power-profiles-daemon-{}", config.system.init);
        // These will be added as owned strings below
        packages.push("bluez");
        packages.push("power-profiles-daemon");
    }

    if cmd.is_dry_run() {
        println!("  [dry-run] Would install KDE packages: {:?}", packages);
        println!("  [dry-run] Would install sddm service: {}", sddm_service);
        if config.system.init == InitSystem::S6 {
            println!("  [dry-run] Would install s6 service packages: {:?}", KDE_S6_PACKAGES);
        }
        return Ok(());
    }

    // Install packages via pacman in chroot
    let pkg_list = packages.join(" ");
    let mut install_cmd = format!("pacman -S --noconfirm {} {}", pkg_list, sddm_service);
    
    // Add init-specific service packages for non-s6 init systems
    if config.system.init != InitSystem::S6 {
        let bluez_service = format!("bluez-{}", config.system.init);
        let power_service = format!("power-profiles-daemon-{}", config.system.init);
        install_cmd = format!("{} {} {}", install_cmd, bluez_service, power_service);
    }
    
    cmd.run_in_chroot(install_root, &install_cmd)?;

    // Configure SDDM
    configure_sddm(cmd, install_root)?;

    // Create .xinitrc for startx fallback
    let username = &config.user.name;
    let xinitrc_path = format!("{}/home/{}/.xinitrc", install_root, username);
    fs::write(&xinitrc_path, "exec startplasma-x11\n")?;

    info!("KDE Plasma installation complete");
    Ok(())
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
