//! KDE Plasma desktop environment installer

use crate::config::DeploymentConfig;
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
    let sddm_service_ref: &str = &sddm_service;

    if cmd.is_dry_run() {
        println!("  [dry-run] Would install KDE packages: {:?}", packages);
        println!("  [dry-run] Would install sddm service: {}", sddm_service);
        return Ok(());
    }

    // Install packages via pacman in chroot
    let pkg_list = packages.join(" ");
    let install_cmd = format!("pacman -S --noconfirm {} {}", pkg_list, sddm_service);
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
