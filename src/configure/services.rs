//! Service management for different init systems

use crate::config::{DeploymentConfig, DesktopEnvironment, DnsProvider, InitSystem, NetworkBackend};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::path::Path;
use tracing::info;

/// Enable necessary services based on configuration
pub fn enable_services(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    info!("Enabling services for {}", config.system.init);

    let services = build_service_list(config);

    for service in services {
        enable_service(cmd, &config.system.init, &service, install_root)?;
    }

    Ok(())
}

/// Build list of services to enable based on configuration
fn build_service_list(config: &DeploymentConfig) -> Vec<String> {
    let mut services = Vec::new();

    // Seat management (only needed for desktop environments with Wayland support)
    if config.desktop.environment != DesktopEnvironment::None {
        services.push("seatd".to_string());
    }

    // Network backend
    match config.network.backend {
        NetworkBackend::Iwd => services.push("iwd".to_string()),
        NetworkBackend::NetworkManager => {
            services.push("NetworkManager".to_string());
            services.push("iwd".to_string());
        }
        NetworkBackend::Connman => services.push("connmand".to_string()),
    }

    // DNS
    if config.network.dns == DnsProvider::DnscryptProxy {
        services.push("dnscrypt-proxy".to_string());
    }

    // Display manager - use greetd for all desktop environments
    // greetd is configured separately via configure::greetd
    if config.desktop.environment != DesktopEnvironment::None {
        services.push("greetd".to_string());
    }

    services
}

/// Enable a service for the configured init system
fn enable_service(
    cmd: &CommandRunner,
    init: &InitSystem,
    service: &str,
    install_root: &str,
) -> Result<()> {
    info!("Enabling service: {} ({})", service, init);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would enable service {} for {}", service, init);
        return Ok(());
    }

    match init {
        InitSystem::Runit => enable_runit_service(service, install_root),
        InitSystem::OpenRC => enable_openrc_service(cmd, service, install_root),
        InitSystem::S6 => enable_s6_service(service, install_root),
        InitSystem::Dinit => enable_dinit_service(service, install_root),
    }
}

/// Enable a runit service by creating symlink
fn enable_runit_service(service: &str, install_root: &str) -> Result<()> {
    let service_dir = format!("{}/etc/runit/sv/{}", install_root, service);
    let enabled_dir = format!("{}/etc/runit/runsvdir/default", install_root);
    let link_path = format!("{}/{}", enabled_dir, service);

    // Check if service exists
    if !Path::new(&service_dir).exists() {
        info!("Service {} not found, skipping", service);
        return Ok(());
    }

    // Create enabled directory if needed
    fs::create_dir_all(&enabled_dir)?;

    // Create symlink
    if !Path::new(&link_path).exists() {
        std::os::unix::fs::symlink(&service_dir, &link_path)?;
    }

    Ok(())
}

/// Enable an OpenRC service
fn enable_openrc_service(cmd: &CommandRunner, service: &str, install_root: &str) -> Result<()> {
    let service_path = format!("{}/etc/init.d/{}", install_root, service);

    if !Path::new(&service_path).exists() {
        info!("Service {} not found, skipping", service);
        return Ok(());
    }

    cmd.run_in_chroot(install_root, &format!("rc-update add {} default", service))?;

    Ok(())
}

/// Enable an s6 service
fn enable_s6_service(service: &str, install_root: &str) -> Result<()> {
    let service_dir = format!("{}/etc/s6/sv/{}", install_root, service);
    let enabled_dir = format!("{}/etc/s6/adminsv/default/contents.d", install_root);
    let link_path = format!("{}/{}", enabled_dir, service);

    if !Path::new(&service_dir).exists() {
        info!("Service {} not found, skipping", service);
        return Ok(());
    }

    fs::create_dir_all(&enabled_dir)?;

    // s6 uses touch files instead of symlinks for some configurations
    fs::write(&link_path, "")?;

    Ok(())
}

/// Enable a dinit service
fn enable_dinit_service(service: &str, install_root: &str) -> Result<()> {
    let service_file = format!("{}/etc/dinit.d/{}", install_root, service);
    let enabled_dir = format!("{}/etc/dinit.d/boot.d", install_root);
    let link_path = format!("{}/{}", enabled_dir, service);

    if !Path::new(&service_file).exists() {
        info!("Service {} not found, skipping", service);
        return Ok(());
    }

    fs::create_dir_all(&enabled_dir)?;

    if !Path::new(&link_path).exists() {
        std::os::unix::fs::symlink(&service_file, &link_path)?;
    }

    Ok(())
}
