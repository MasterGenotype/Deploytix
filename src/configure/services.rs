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
        // The init-specific elogind service package is blacklisted in
        // build_service_packages() because it conflicts with seatd-<init>,
        // so no elogind service file lands on disk — skip the enable to
        // avoid a misleading "service not found" warning.  The base elogind
        // package is still installed for pam_elogind.
        if service == "elogind" {
            continue;
        }
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
        NetworkBackend::NetworkManagerWpa => {
            services.push("NetworkManager".to_string());
            services.push("wpa_supplicant".to_string());
        }
    }

    // Display manager — greetd on all init systems.
    // No official greetd-s6 package exists, so for S6 we write the service
    // directory ourselves in configure_greetd(); enable_s6_service() will
    // then find it and add it to the default bundle via `s6 set enable`.
    if config.desktop.environment != DesktopEnvironment::None {
        services.push("greetd".to_string());
    }

    // elogind — must be running before greetd so PAM pam_elogind can
    // create the seat session that grants gamescope DRM/input ACLs.
    // elogind-s6 exists in Artix repos alongside all other init variants.
    if config.packages.install_session_switching
        && config.desktop.environment != DesktopEnvironment::None
    {
        services.push("elogind".to_string());
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
        // No greetd-s6 package exists in Artix repos; we write the service
        // directory ourselves in configure_greetd().  All other services
        // (including elogind-s6) have proper Artix packages.
        if *init == InitSystem::S6 && base == "greetd" {
            continue;
        }
        // elogind-<init> conflicts with seatd-<init>: the two service packages
        // both ship a `org.freedesktop.login1`-style seat manager unit and
        // pacman refuses to install both.  pam_elogind from the base elogind
        // package is enough for greetd's PAM stack, so the init-specific
        // elogind service package is blacklisted from installation.
        if base == "elogind" {
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
    crate::configure::packages::pacman_install_chroot(cmd, install_root, &install_cmd).map_err(
        |e| {
            warn!("Failed to install service packages: {}", e);
            e
        },
    )?;

    Ok(())
}

/// Enable a service for the configured init system.
///
/// Also called directly from `installer.rs` for services whose package and
/// service-file setup is handled by a dedicated install function (HHD, Decky
/// Loader) rather than by the generic `install_service_packages()` path.
pub(crate) fn enable_service(
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
        InitSystem::S6 => enable_s6_service(cmd, service, install_root),
        InitSystem::Dinit => enable_dinit_service(service, install_root),
    }
}

/// Persist pending s6 service changes by committing the service database.
///
/// With the s6-frontend tooling, `s6 set enable <service>` stages a change
/// to the default bundle; the boot database is only recompiled once
/// `s6 set commit` runs.  Call this once after all services have been
/// enabled — the installer does so in the finalize phase.  `s6 live
/// install` is deliberately not run: the chroot has no live s6 state, and
/// the freshly committed database is picked up on first boot.
///
/// No-op for the other init systems, whose enable operations (symlinks,
/// `rc-update`) are immediately persistent.
pub(crate) fn commit_service_database(
    cmd: &CommandRunner,
    init: &InitSystem,
    install_root: &str,
) -> Result<()> {
    if *init != InitSystem::S6 {
        return Ok(());
    }

    info!("Committing s6 service database (s6 set commit)");
    cmd.run_in_chroot(install_root, "s6 set commit")?;

    Ok(())
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

/// Locate the s6 service definition for `service` on the target system.
///
/// Definitions from official `-s6` packages live in `/etc/s6/sv`; custom
/// services written by deploytix live in `/etc/s6/adminsv` (the directory
/// reserved for admin-defined s6-rc services).  Since the move to
/// s6-frontend, Artix packages ship service directories under the plain
/// service name; the legacy in-house `{name}-srv` layout is still checked
/// as a fallback for transition-era packages.
///
/// Returns the name to pass to `s6 set enable`, or `None` when no
/// definition exists.
fn resolve_s6_service_name(service: &str, install_root: &str) -> Option<String> {
    let legacy = format!("{}-srv", service);
    for name in [service, legacy.as_str()] {
        for base in ["etc/s6/sv", "etc/s6/adminsv"] {
            if Path::new(&format!("{}/{}/{}", install_root, base, name)).exists() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Enable an s6 service via the s6-frontend CLI.
///
/// Artix manages s6 with upstream's s6-frontend: `s6 set enable <service>`
/// adds the service to the default bundle, replacing the old in-house
/// scheme of touching empty files in `/etc/s6/adminsv/default/contents.d/`.
/// The staged change is made persistent by a single `s6 set commit` in the
/// finalize phase (see [`commit_service_database`]).
///
/// Service definitions come from official `-s6` packages (e.g. `seatd-s6`,
/// `iwd-s6`) or are written by deploytix into `/etc/s6/adminsv`.  If no
/// definition is found the corresponding package was not installed and we
/// skip with a warning.
fn enable_s6_service(cmd: &CommandRunner, service: &str, install_root: &str) -> Result<()> {
    let Some(s6_service_name) = resolve_s6_service_name(service, install_root) else {
        warn!(
            "Service {} not found under /etc/s6/sv or /etc/s6/adminsv \
             (is the corresponding -s6 package installed?), skipping",
            service
        );
        return Ok(());
    };

    cmd.run_in_chroot(install_root, &format!("s6 set enable {}", s6_service_name))?;
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
