//! Service management for different init systems

use crate::config::{DeploymentConfig, DesktopEnvironment, InitSystem, NetworkBackend};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
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

    // The service stores just changed: `-s6` packages were installed above
    // and the greetd definition was written into /etc/s6/adminsv earlier in
    // the configure phase.  Rebuild the reference database so the enables
    // below can see every definition (s6-only; no-op otherwise).
    sync_service_repository(cmd, &config.system.init, install_root)?;

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
            // iwd first: NetworkManager fronts it via `wifi.backend=iwd`, and an
            // NM that comes up before iwd owns its D-Bus name leaves the Wi-Fi
            // device unmanaged for the rest of the boot. Enable order is not a
            // hard guarantee under runit's parallel start, which is why the
            // credentials are also seeded into iwd's own store
            // (configure::network::preseed_wifi) — but there is no reason to
            // enable them in the order that loses the race.
            services.push("iwd".to_string());
            services.push("NetworkManager".to_string());
        }
        NetworkBackend::NetworkManagerWpa => {
            services.push("NetworkManager".to_string());
            services.push("wpa_supplicant".to_string());
        }
    }

    // Display manager — selected via desktop.display_manager (greetd is the
    // default; DisplayManager::None boots to a TTY login with no DM service).
    // No official greetd-s6 package exists, so for S6 we write the service
    // directory ourselves in configure_greetd(); enable_s6_service() will
    // then find it and add it to the default bundle via `s6 set enable`.
    if config.desktop.environment != DesktopEnvironment::None {
        if let Some(dm_service) = config.desktop.display_manager.service_name() {
            services.push(dm_service.to_string());
        }
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
        // LightDM needs a greeter; lightdm-gtk-greeter is its compiled-in
        // default on Artix and is not pulled in as a hard dependency.
        if base == "lightdm" {
            packages.push("lightdm-gtk-greeter".to_string());
        }
        // Some services have no `{base}-{init}` package for a given init —
        // the init's own module says which, so asking pacman for one that
        // does not exist cannot fail the whole transaction.
        if crate::init::module(init).no_service_package.contains(&base) {
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
    crate::install::packages::pacman_install_chroot(cmd, install_root, &install_cmd).map_err(
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

    (crate::init::module(init).enable)(cmd, service, install_root)
}

/// Rebuild the init's service database after definitions changed on disk.
///
/// Deploytix writes service definitions by hand (greetd, zram, hhd,
/// plugin_loader, evdevhook2) as well as installing packaged ones, and an init
/// that keeps an index has to be told. Call this after any such change, before
/// the corresponding enable. A no-op for inits with nothing to index, so call
/// sites do not need to guard.
pub(crate) fn sync_service_repository(
    cmd: &CommandRunner,
    init: &InitSystem,
    install_root: &str,
) -> Result<()> {
    match crate::init::module(init).sync_repository {
        Some(sync) => sync(cmd, install_root),
        None => Ok(()),
    }
}

/// Persist staged service changes as the boot database.
///
/// Call once after all services have been enabled — the installer does so in
/// the finalize phase. A no-op for inits whose enable operations (symlinks,
/// `rc-update`) are immediately persistent.
pub(crate) fn commit_service_database(
    cmd: &CommandRunner,
    init: &InitSystem,
    install_root: &str,
) -> Result<()> {
    match crate::init::module(init).commit_database {
        Some(commit) => commit(cmd, install_root),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DisplayManager;

    fn config() -> DeploymentConfig {
        // sample(): KDE desktop, greetd display manager (default), runit,
        // iwd network backend
        DeploymentConfig::sample()
    }

    /// NetworkManager fronts iwd via `wifi.backend=iwd`; an NM that comes up
    /// before iwd owns its D-Bus name leaves the Wi-Fi device unmanaged for the
    /// rest of the boot. Enable order is not a hard guarantee under runit's
    /// parallel start, but there is no reason to enable them in the order that
    /// loses the race.
    #[test]
    fn nm_iwd_backend_enables_iwd_before_networkmanager() {
        let mut cfg = config();
        cfg.network.backend = crate::config::NetworkBackend::NetworkManager;
        let services = build_service_list(&cfg);

        let iwd = services
            .iter()
            .position(|s| s == "iwd")
            .expect("iwd enabled");
        let nm = services
            .iter()
            .position(|s| s == "NetworkManager")
            .expect("NetworkManager enabled");
        assert!(iwd < nm, "iwd must be enabled before NetworkManager");
    }

    #[test]
    fn greetd_default_enables_greetd_service() {
        let services = build_service_list(&config());
        assert!(services.contains(&"greetd".to_string()));
        assert!(services.contains(&"seatd".to_string()));
    }

    #[test]
    fn sddm_replaces_greetd_service() {
        let mut cfg = config();
        cfg.desktop.display_manager = DisplayManager::Sddm;
        let services = build_service_list(&cfg);
        assert!(services.contains(&"sddm".to_string()));
        assert!(!services.contains(&"greetd".to_string()));
    }

    #[test]
    fn display_manager_none_enables_no_dm_service() {
        let mut cfg = config();
        cfg.desktop.display_manager = DisplayManager::None;
        let services = build_service_list(&cfg);
        for dm in ["greetd", "sddm", "gdm", "lightdm"] {
            assert!(!services.contains(&dm.to_string()));
        }
    }

    #[test]
    fn headless_config_enables_no_dm_service() {
        let mut cfg = config();
        cfg.desktop.environment = DesktopEnvironment::None;
        let services = build_service_list(&cfg);
        assert!(!services.contains(&"greetd".to_string()));
        assert!(!services.contains(&"seatd".to_string()));
    }

    #[test]
    fn lightdm_packages_include_greeter_and_init_service() {
        let services = vec!["lightdm".to_string()];
        let packages = build_service_packages(&services, &InitSystem::Runit);
        assert!(packages.contains(&"lightdm".to_string()));
        assert!(packages.contains(&"lightdm-gtk-greeter".to_string()));
        assert!(packages.contains(&"lightdm-runit".to_string()));
    }

    #[test]
    fn greetd_s6_service_package_is_skipped() {
        // No official greetd-s6 package exists; the service directory is
        // written by hand in configure_greetd().
        let services = vec!["greetd".to_string()];
        let packages = build_service_packages(&services, &InitSystem::S6);
        assert!(packages.contains(&"greetd".to_string()));
        assert!(!packages.contains(&"greetd-s6".to_string()));
    }
}
