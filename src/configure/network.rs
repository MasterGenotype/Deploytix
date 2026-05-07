//! Network configuration

use crate::config::{DeploymentConfig, NetworkBackend};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// Configure network settings
pub fn configure_network(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    info!("Configuring network (backend: {})", config.network.backend);

    // Configure network backend
    match config.network.backend {
        NetworkBackend::Iwd => configure_iwd(cmd, install_root)?,
        NetworkBackend::NetworkManager => configure_nm_with_backend(cmd, install_root, "iwd")?,
        NetworkBackend::NetworkManagerWpa => {
            configure_nm_with_backend(cmd, install_root, "wpa_supplicant")?
        }
    }

    Ok(())
}

/// Configure iwd
fn configure_iwd(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Configuring iwd");

    let iwd_conf_dir = format!("{}/etc/iwd", install_root);
    let iwd_conf_path = format!("{}/main.conf", iwd_conf_dir);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure iwd at {}", iwd_conf_path);
        return Ok(());
    }

    fs::create_dir_all(&iwd_conf_dir)?;

    let iwd_config = r#"[General]
EnableNetworkConfiguration=true

[Network]
NameResolvingService=resolvconf
RoutePriorityOffset=300
EnableIPv6=true

[Scan]
DisablePeriodicScan=false
"#;

    fs::write(&iwd_conf_path, iwd_config)?;

    info!("iwd configuration written");
    Ok(())
}

/// Configure NetworkManager with the given wifi backend ("iwd" or "wpa_supplicant").
fn configure_nm_with_backend(
    cmd: &CommandRunner,
    install_root: &str,
    wifi_backend: &str,
) -> Result<()> {
    info!("Configuring NetworkManager with {} backend", wifi_backend);

    let nm_conf_dir = format!("{}/etc/NetworkManager/conf.d", install_root);
    let nm_conf_path = format!("{}/wifi-backend.conf", nm_conf_dir);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would configure NetworkManager at {} (wifi.backend={})",
            nm_conf_path, wifi_backend
        );
        return Ok(());
    }

    fs::create_dir_all(&nm_conf_dir)?;

    let nm_config = format!("[device]\nwifi.backend={}\n", wifi_backend);
    fs::write(&nm_conf_path, nm_config)?;

    info!("NetworkManager configuration written");
    Ok(())
}
