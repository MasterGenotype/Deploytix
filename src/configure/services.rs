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
    info!(
        "Enabling {} services for {} init system: [{}]",
        services.len(),
        config.system.init,
        services.join(", ")
    );

    // Install required packages for the services before enabling them
    install_service_packages(cmd, config, install_root, &services)?;

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

    // Display manager - greetd is enabled for non-s6 init systems.
    // Artix does not provide a greetd-s6 package/service.
    if config.desktop.environment != DesktopEnvironment::None
        && config.system.init != InitSystem::S6
    {
        services.push("greetd".to_string());
    }

    services
}

/// Map a service name to its base package name
fn service_base_package(service: &str) -> &str {
    match service {
        // Service name uses CamelCase but the package is lowercase
        "NetworkManager" => "networkmanager",
        other => other,
    }
}

/// Build the list of packages required for the given services and init system.
///
/// Each service needs its base daemon package (e.g. `seatd`) plus the
/// init-specific service package (e.g. `seatd-s6`).
fn build_service_packages(services: &[String], init: &InitSystem) -> Vec<String> {
    let mut packages = Vec::new();
    for service in services {
        let base = service_base_package(service);
        packages.push(base.to_string());
        if *init == InitSystem::S6 && base == "greetd" {
            continue;
        }
        let init_pkg = format!("{}-{}", base, init);
        packages.push(init_pkg);
    }
    packages
}

/// Install the packages required by the services that will be enabled.
///
/// Uses `pacman -S --needed` so already-installed packages are skipped.
fn install_service_packages(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
    services: &[String],
) -> Result<()> {
    let packages = build_service_packages(services, &config.system.init);
    if packages.is_empty() {
        return Ok(());
    }

    let pkg_list = packages.join(" ");
    info!("Installing service packages: {}", pkg_list);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would install service packages: {}", pkg_list);
        return Ok(());
    }

    let install_cmd = format!("pacman -S --noconfirm --needed {}", pkg_list);
    cmd.run_in_chroot(install_root, &install_cmd)
        .map(|_| ())
        .map_err(|e| {
            warn!("Failed to install service packages: {}", e);
            e
        })?;

    Ok(())
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
        warn!(
            "Service {} not found at {}, skipping",
            service, service_dir_check
        );
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
        warn!(
            "Service {} not found at {}, skipping",
            service, service_path
        );
        return Ok(());
    }

    cmd.run_in_chroot(install_root, &format!("rc-update add {} default", service))?;
    info!("Enabled OpenRC service {}", service);

    Ok(())
}

/// Map service names to their s6 service directory names.
///
/// Artix s6 packages use a `-srv` suffix for service directories.
/// For example, NetworkManager's s6 service directory is 'NetworkManager-srv'.
fn map_s6_service_name(service: &str) -> String {
    format!("{}-srv", service)
}

/// Enable an s6 service
///
/// Service directories are provided by official `-s6` packages from the Artix
/// repositories (e.g. `seatd-s6`, `iwd-s6`).  If the directory is missing the
/// corresponding package was not installed and we skip with a warning.
fn enable_s6_service(service: &str, install_root: &str) -> Result<()> {
    let s6_service_name = map_s6_service_name(service);
    let service_dir = format!("{}/etc/s6/sv/{}", install_root, &s6_service_name);
    let enabled_dir = format!("{}/etc/s6/adminsv/default/contents.d", install_root);
    let link_path = format!("{}/{}", enabled_dir, &s6_service_name);

    // Service directories come from official *-s6 packages; skip if missing
    if !Path::new(&service_dir).exists() {
        warn!(
            "Service {} not found at {} (is the corresponding -s6 package installed?), skipping",
            service, service_dir
        );
        return Ok(());
    }

    fs::create_dir_all(&enabled_dir)?;

    // s6 uses touch files to declare wanted services in a bundle
    fs::write(&link_path, "")?;
    info!("Enabled s6 service {}", service);

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
        warn!(
            "Service {} not found at {}, skipping",
            service, service_file_check
        );
        return Ok(());
    }

    fs::create_dir_all(&enabled_dir)?;

    if !Path::new(&link_path).exists() {
        std::os::unix::fs::symlink(&service_file_target, &link_path)?;
        info!("Created symlink {} -> {}", link_path, service_file_target);
    }

    Ok(())
}
