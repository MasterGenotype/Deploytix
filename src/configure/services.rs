//! Service management for different init systems

use crate::config::{DeploymentConfig, DesktopEnvironment, InitSystem, NetworkBackend};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

/// Enable necessary services based on configuration
pub fn enable_services(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let services = build_service_list(config);
    info!("Enabling {} services for {} init system: [{}]", services.len(), config.system.init, services.join(", "));

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

/// Enable a runit service by creating symlink from runsvdir/default to sv/
fn enable_runit_service(service: &str, install_root: &str) -> Result<()> {
    // Path to check if service exists (within install_root)
    let service_dir_check = format!("{}/etc/runit/sv/{}", install_root, service);
    // Symlink target - path relative to installed system root (not install_root)
    let service_dir_target = format!("/etc/runit/sv/{}", service);
    // Directory where symlinks are created
    let enabled_dir = format!("{}/etc/runit/runsvdir/default", install_root);
    let link_path = format!("{}/{}", enabled_dir, service);

    // Check if service exists in installed system
    if !Path::new(&service_dir_check).exists() {
        warn!("Service {} not found at {}, skipping", service, service_dir_check);
        return Ok(());
    }

    // Create enabled directory if needed
    fs::create_dir_all(&enabled_dir)?;

    // Create symlink pointing to path relative to installed system root
    if !Path::new(&link_path).exists() {
        std::os::unix::fs::symlink(&service_dir_target, &link_path)?;
        info!("Created symlink {} -> {}", link_path, service_dir_target);
    }

    Ok(())
}

/// Enable an OpenRC service
fn enable_openrc_service(cmd: &CommandRunner, service: &str, install_root: &str) -> Result<()> {
    let service_path = format!("{}/etc/init.d/{}", install_root, service);

    if !Path::new(&service_path).exists() {
        warn!("Service {} not found at {}, skipping", service, service_path);
        return Ok(());
    }

    cmd.run_in_chroot(install_root, &format!("rc-update add {} default", service))?;
    info!("Enabled OpenRC service {}", service);

    Ok(())
}

/// Enable an s6 service
fn enable_s6_service(service: &str, install_root: &str) -> Result<()> {
    let service_dir = format!("{}/etc/s6/sv/{}", install_root, service);
    let enabled_dir = format!("{}/etc/s6/adminsv/default/contents.d", install_root);
    let link_path = format!("{}/{}", enabled_dir, service);

    // If the service directory is missing, try to create a minimal one
    if !Path::new(&service_dir).exists() {
        warn!("Service {} not found at {}, attempting to auto-create", service, service_dir);
        maybe_create_builtin_s6_service(service, install_root)?;
    }

    // If it's still missing after auto-create, skip with a warning
    if !Path::new(&service_dir).exists() {
        warn!("Service {} is not available and could not be auto-created; skipping enable.", service);
        return Ok(());
    }

    fs::create_dir_all(&enabled_dir)?;

    // s6 uses touch files to declare wanted services in a bundle
    fs::write(&link_path, "")?;
    info!("Enabled s6 service {}", service);

    Ok(())
}

/// Create a minimal s6 service directory for well-known services if not present
fn maybe_create_builtin_s6_service(service: &str, install_root: &str) -> Result<()> {
    let run_cmd = match service {
        "greetd" => Some("exec /usr/bin/greetd -c /etc/greetd/config.toml"),
        "seatd" => Some("exec /usr/bin/seatd -g video"),
        "iwd" => Some("exec /usr/bin/iwd"),
        "NetworkManager" | "networkmanager" => Some("exec /usr/bin/NetworkManager"),
        _ => None,
    };

    if let Some(cmdline) = run_cmd {
        let svc_dir = format!("{}/etc/s6/sv/{}", install_root, service);
        let run_path = format!("{}/run", svc_dir);
        fs::create_dir_all(&svc_dir)?;
        fs::write(&run_path, format!("#!/bin/sh\nexec {}
", cmdline))?;
        // make executable
        let mut perms = fs::metadata(&run_path)?.permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        fs::set_permissions(&run_path, perms)?;
        info!("Auto-created s6 service for {} at {}", service, svc_dir);
    }

    Ok(())
}

/// Enable a dinit service
fn enable_dinit_service(service: &str, install_root: &str) -> Result<()> {
    let service_file_check = format!("{}/etc/dinit.d/{}", install_root, service);
    // Symlink target - path relative to installed system root
    let service_file_target = format!("/etc/dinit.d/{}", service);
    let enabled_dir = format!("{}/etc/dinit.d/boot.d", install_root);
    let link_path = format!("{}/{}", enabled_dir, service);

    if !Path::new(&service_file_check).exists() {
        warn!("Service {} not found at {}, skipping", service, service_file_check);
        return Ok(());
    }

    fs::create_dir_all(&enabled_dir)?;

    if !Path::new(&link_path).exists() {
        std::os::unix::fs::symlink(&service_file_target, &link_path)?;
        info!("Created symlink {} -> {}", link_path, service_file_target);
    }

    Ok(())
}
