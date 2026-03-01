//! Basestrap wrapper for base system installation

use crate::config::{DeploymentConfig, DesktopEnvironment, Filesystem, NetworkBackend};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use tracing::{info, warn};

/// Build the package list for basestrap
pub fn build_package_list(config: &DeploymentConfig) -> Vec<String> {
    let mut packages = Vec::new();

    // Base system
    packages.extend([
        "base".to_string(),
        "base-devel".to_string(),
        config.system.init.base_package().to_string(),
    ]);

    // For s6, pre-select providers to avoid interactive prompts
    if config.system.init == crate::config::InitSystem::S6 {
        // D-Bus provider for s6; no elogind, use seatd for seats
        packages.push("dbus-s6".to_string());
        // no elogind-s6
        // Core s6 service packages
        packages.push("networkmanager-s6".to_string());
        packages.push("seatd-s6".to_string());
        packages.push("iwd-s6".to_string());
    }

    // Kernel and firmware
    packages.extend([
        "linux-firmware".to_string(),
        "linux-zen".to_string(),
        "linux-zen-headers".to_string(),
    ]);

    // Filesystem tools — always include btrfs-progs as it is commonly needed
    packages.push("btrfs-progs".to_string());
    // Data filesystem tools
    match config.disk.filesystem {
        Filesystem::Ext4 => packages.push("e2fsprogs".to_string()),
        Filesystem::Xfs => packages.push("xfsprogs".to_string()),
        Filesystem::F2fs => packages.push("f2fs-tools".to_string()),
        Filesystem::Zfs => {
            packages.push("zfs-utils".to_string());
            // Kernel module is separate from userspace tools
            packages.push("zfs-linux-zen".to_string());
        }
        Filesystem::Btrfs => {} // Already added above
    }
    // Boot filesystem tools (if different from data filesystem)
    match config.disk.boot_filesystem {
        Filesystem::Ext4 if config.disk.filesystem != Filesystem::Ext4 => {
            packages.push("e2fsprogs".to_string());
        }
        Filesystem::Xfs if config.disk.filesystem != Filesystem::Xfs => {
            packages.push("xfsprogs".to_string());
        }
        Filesystem::F2fs if config.disk.filesystem != Filesystem::F2fs => {
            packages.push("f2fs-tools".to_string());
        }
        Filesystem::Zfs if config.disk.filesystem != Filesystem::Zfs => {
            packages.push("zfs-utils".to_string());
            packages.push("zfs-linux-zen".to_string());
        }
        _ => {} // same as data filesystem or btrfs (already added)
    }

    // Bootloader
    packages.extend(["efibootmgr".to_string(), "grub".to_string()]);

    // Deploytix — install itself and tkg-gui on the target system so they
    // remain available after first boot for re-deployment and kernel builds.
    // dosfstools is always required for the FAT32 EFI partition.
    packages.extend([
        "deploytix-git".to_string(),
        "tkg-gui-git".to_string(),
        "dosfstools".to_string(),
    ]);

    // Essential tools
    packages.extend([
        "git".to_string(),
        "nano".to_string(),
        "curl".to_string(),
        "wget".to_string(),
        "mkinitcpio".to_string(),
        "openssl".to_string(),
    ]);

    // Build tools
    packages.extend(["gcc".to_string(), "rustup".to_string()]);

    // Network packages based on config
    match config.network.backend {
        NetworkBackend::Iwd => {
            packages.extend(["iwd".to_string(), "openresolv".to_string()]);
            if config.system.init != crate::config::InitSystem::S6 {
                let service_pkg = format!("iwd-{}", config.system.init);
                packages.push(service_pkg);
            }
        }
        NetworkBackend::NetworkManager => {
            packages.extend([
                "networkmanager".to_string(),
                // Default to iwd backend; wpa_supplicant can be added later if desired
                "iwd".to_string(),
                "openresolv".to_string(),
            ]);
            if config.system.init != crate::config::InitSystem::S6 {
                let nm_service_pkg = format!("networkmanager-{}", config.system.init);
                let iwd_service_pkg = format!("iwd-{}", config.system.init);
                packages.push(nm_service_pkg);
                packages.push(iwd_service_pkg);
            }
            // Add nm-applet for desktop environments
            if config.desktop.environment != DesktopEnvironment::None {
                packages.push("network-manager-applet".to_string());
            }
        }
    }

    // Desktop environment prerequisites (display server, seat management, display manager, audio)
    if config.desktop.environment != DesktopEnvironment::None {
        packages.extend([
            // Display
            "xorg-server".to_string(),
            "xorg-xinit".to_string(),
            "seatd".to_string(),
            // Audio - ALSA base
            "alsa-utils".to_string(),
            "alsa-tools".to_string(),
            // Audio - PipeWire (modern audio server)
            "pipewire".to_string(),
            "wireplumber".to_string(),
            "pipewire-pulse".to_string(),
            "pipewire-alsa".to_string(),
        ]);
        if config.system.init == crate::config::InitSystem::S6 {
            // Official s6 service packages from Artix repos
            packages.push("alsa-utils-s6".to_string());
        } else {
            let seatd_service = format!("seatd-{}", config.system.init);
            let greetd_service = format!("greetd-{}", config.system.init);
            packages.push(seatd_service);
            packages.push(greetd_service);
        }
    }

    // Encryption tools (if enabled)
    if config.disk.encryption {
        packages.push("cryptsetup".to_string());
    }

    // lvm2 provides device-mapper, required by mkinitcpio encrypt/lvm2 hooks
    if config.disk.encryption || config.disk.use_lvm_thin {
        packages.push("lvm2".to_string());
    }

    // thin-provisioning-tools for LVM thin provisioning (feature-driven)
    if config.disk.use_lvm_thin {
        packages.push("thin-provisioning-tools".to_string());
    }

    // gocryptfs encrypted home directory (if enabled)
    if config.user.encrypt_home {
        packages.extend([
            "gocryptfs".to_string(),
            "pam_mount".to_string(),
            "fuse2".to_string(),
        ]);
    }

    // SecureBoot tools (if enabled)
    if config.system.secureboot {
        match config.system.secureboot_method {
            crate::config::SecureBootMethod::Sbctl => {
                packages.push("sbctl".to_string());
            }
            crate::config::SecureBootMethod::ManualKeys | crate::config::SecureBootMethod::Shim => {
                packages.push("sbsigntools".to_string());
                packages.push("efitools".to_string());
            }
        }
    }

    packages
}

// === Custom [deploytix] repository preparation ===
//
// The deploytix-git and tkg-gui-git packages live in a custom pacman
// repository rather than in the standard Artix mirrors.  On the live ISO
// this repo is embedded at /var/lib/deploytix-repo and referenced in
// /etc/pacman.conf.  When the installer runs outside that environment we
// create a temporary local repo from any pre-built .pkg.tar.zst files
// we can locate and pass `-C <config>` to basestrap.

/// Filename prefixes (with trailing dash) for package archives that
/// belong to the custom [deploytix] repository.
const CUSTOM_PKG_PREFIXES: &[&str] = &["deploytix-git-", "deploytix-gui-git-", "tkg-gui-git-"];

/// Packages from the [deploytix] repo that are in the basestrap list
/// and must be resolvable.
const REQUIRED_CUSTOM_PACKAGES: &[&str] = &["deploytix-git", "tkg-gui-git"];

/// Path where the ISO live-overlay embeds the deploytix repo.
const ISO_REPO_PATH: &str = "/var/lib/deploytix-repo";

/// Temporary repo the installer creates when no repo is configured.
const TEMP_REPO_DIR: &str = "/tmp/deploytix-local-repo";

/// Temporary pacman.conf that adds the [deploytix] repo.
const TEMP_PACMAN_CONF: &str = "/tmp/deploytix-pacman.conf";

/// Check whether all required custom packages are resolvable in the
/// currently configured pacman sync databases.
fn custom_packages_in_sync_db() -> bool {
    REQUIRED_CUSTOM_PACKAGES.iter().all(|pkg| {
        std::process::Command::new("pacman")
            .args(["-Si", pkg])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Search well-known directories for pre-built `.pkg.tar.zst` files
/// that belong to the deploytix custom packages.
fn locate_prebuilt_packages() -> Vec<PathBuf> {
    let mut search_dirs: Vec<PathBuf> = Vec::new();

    // 1. Relative to the running binary (repo_root/target/release/deploytix).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(repo_root) = exe
            .parent() // target/release/
            .and_then(|p| p.parent()) // target/
            .and_then(|p| p.parent()) // repo root
        {
            search_dirs.push(repo_root.join("pkg"));
        }
    }

    // 2. Paths based on SUDO_USER (installer runs as root via sudo).
    if let Ok(user) = std::env::var("SUDO_USER") {
        let home = format!("/home/{}", user);
        search_dirs.push(PathBuf::from(&home).join(".gitrepos/Deploytix/pkg"));
        search_dirs.push(PathBuf::from(&home).join("artools-workspace/tkg-gui-src/pkg"));
    }

    // 3. Current working directory (might be repo root).
    search_dirs.push(PathBuf::from("pkg"));

    let mut found = Vec::new();
    let mut seen_names = HashSet::new();

    for dir in &search_dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if !name.ends_with(".pkg.tar.zst") {
                continue;
            }
            let is_custom = CUSTOM_PKG_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix));
            if is_custom && seen_names.insert(name) {
                found.push(path);
            }
        }
    }

    found
}

/// Create a temporary local pacman repository from the given package
/// files and generate a repo database with `repo-add`.
fn create_temp_repo(cmd: &CommandRunner, packages: &[PathBuf]) -> Result<()> {
    let repo = Path::new(TEMP_REPO_DIR);

    // Clean previous run.
    if repo.is_dir() {
        std::fs::remove_dir_all(repo).map_err(DeploytixError::Io)?;
    }
    std::fs::create_dir_all(repo).map_err(DeploytixError::Io)?;

    for pkg in packages {
        let dest = repo.join(pkg.file_name().unwrap());
        std::fs::copy(pkg, &dest).map_err(DeploytixError::Io)?;
        info!("  Copied {} into temp repo", dest.display());
    }

    // Build the pacman database.
    let db_path = format!("{}/deploytix.db.tar.zst", TEMP_REPO_DIR);
    let pkg_paths: Vec<String> = std::fs::read_dir(repo)
        .map_err(DeploytixError::Io)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let s = p.to_string_lossy();
            s.ends_with(".pkg.tar.zst") && !s.contains("deploytix.db")
        })
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    let mut args: Vec<&str> = vec![&db_path];
    let refs: Vec<&str> = pkg_paths.iter().map(|s| s.as_str()).collect();
    args.extend(refs);

    cmd.run("repo-add", &args)?;

    info!("Created temporary deploytix repo at {}", TEMP_REPO_DIR);
    Ok(())
}

/// Write a temporary `pacman.conf` that extends the system config with
/// a `[deploytix]` repo section.  Returns the path to the temp file.
fn write_custom_pacman_conf(repo_dir: &str) -> Result<Option<String>> {
    let system_conf =
        std::fs::read_to_string("/etc/pacman.conf").map_err(DeploytixError::Io)?;

    let custom = format!(
        "{}\n\n\
         # Deploytix local repository (auto-configured by the installer)\n\
         [deploytix]\n\
         SigLevel = Optional TrustAll\n\
         Server = file://{}\n",
        system_conf.trim_end(),
        repo_dir,
    );

    std::fs::write(TEMP_PACMAN_CONF, &custom).map_err(DeploytixError::Io)?;

    info!(
        "Custom pacman.conf written to {} (repo: file://{})",
        TEMP_PACMAN_CONF, repo_dir
    );
    Ok(Some(TEMP_PACMAN_CONF.to_string()))
}

/// Ensure the deploytix custom packages are resolvable by pacman for
/// the upcoming basestrap invocation.
///
/// Returns `Some(path)` to a custom `pacman.conf` if one was created
/// (use with `basestrap -C`), or `None` if the packages are already in
/// a configured repository.
pub fn prepare_deploytix_repo(cmd: &CommandRunner) -> Result<Option<String>> {
    if cmd.is_dry_run() {
        info!("[dry-run] Would ensure deploytix local repo is available");
        return Ok(None);
    }

    // Fast-path: packages already resolvable.
    if custom_packages_in_sync_db() {
        info!("Deploytix custom packages found in configured repositories");
        return Ok(None);
    }

    info!("Deploytix custom packages not in repos; preparing local repository");

    // 1. ISO-embedded repo already has a database — just write a config.
    let iso_db = Path::new(ISO_REPO_PATH).join("deploytix.db.tar.zst");
    if iso_db.exists() {
        info!("Using ISO-embedded repo at {}", ISO_REPO_PATH);
        return write_custom_pacman_conf(ISO_REPO_PATH);
    }

    // 2. Search for pre-built package files.
    let packages = locate_prebuilt_packages();
    if packages.is_empty() {
        return Err(DeploytixError::ConfigError(
            "Cannot find deploytix-git / tkg-gui-git packages.\n\
             These custom packages are not in any configured pacman repository \
             and no pre-built .pkg.tar.zst files were found.\n\
             Please run the installer from the Deploytix live ISO, or build the \
             packages first with: iso/build-deploytix-iso.sh"
                .to_string(),
        ));
    }

    info!(
        "Found {} pre-built package file(s); creating temporary repo",
        packages.len()
    );
    create_temp_repo(cmd, &packages)?;
    write_custom_pacman_conf(TEMP_REPO_DIR)
}

/// Maximum number of retry attempts for basestrap on network failures
const BASESTRAP_MAX_RETRIES: u32 = 3;

/// Delay between retry attempts (in seconds)
const BASESTRAP_RETRY_DELAY_SECS: u64 = 5;

/// Check if an error message indicates a transient network failure
fn is_network_error(stderr: &str) -> bool {
    let network_error_patterns = [
        "Operation too slow",
        "failed retrieving file",
        "failed to retrieve some files",
        "Connection timed out",
        "Could not resolve host",
        "Network is unreachable",
        "Connection refused",
        "SSL connection timeout",
        "error: failed to synchronize",
    ];

    network_error_patterns
        .iter()
        .any(|pattern| stderr.contains(pattern))
}

/// Run basestrap to install the base system
pub fn run_basestrap(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    run_basestrap_with_retries(cmd, config, install_root, BASESTRAP_MAX_RETRIES)
}

/// Run basestrap with configurable retry count
pub fn run_basestrap_with_retries(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
    max_retries: u32,
) -> Result<()> {
    // Ensure the custom [deploytix] packages are available.
    let custom_conf = prepare_deploytix_repo(cmd)?;

    let packages = build_package_list(config);

    info!(
        "Installing {} packages with basestrap to {}",
        packages.len(),
        install_root
    );

    // Build argument list — prepend `-C <config>` when a custom
    // pacman.conf was generated for the [deploytix] repo.
    let mut args: Vec<&str> = Vec::new();
    if let Some(ref conf_path) = custom_conf {
        args.push("-C");
        args.push(conf_path.as_str());
    }
    args.push(install_root);
    let pkg_refs: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();
    args.extend(pkg_refs);

    let mut last_error = None;

    for attempt in 1..=max_retries {
        match cmd.run("basestrap", &args) {
            Ok(_) => {
                if attempt > 1 {
                    info!("basestrap succeeded on attempt {}", attempt);
                }
                return Ok(());
            }
            Err(e) => {
                let error_str = e.to_string();

                if is_network_error(&error_str) && attempt < max_retries {
                    warn!(
                        "basestrap failed due to network error (attempt {}/{}): {}",
                        attempt, max_retries, error_str
                    );
                    warn!("Retrying in {} seconds...", BASESTRAP_RETRY_DELAY_SECS);
                    thread::sleep(Duration::from_secs(BASESTRAP_RETRY_DELAY_SECS));
                    last_error = Some(error_str);
                } else {
                    // Non-network error or final attempt - fail immediately
                    return Err(DeploytixError::CommandFailed {
                        command: "basestrap".to_string(),
                        stderr: error_str,
                    });
                }
            }
        }
    }

    // Should not reach here, but handle it just in case
    Err(DeploytixError::CommandFailed {
        command: "basestrap".to_string(),
        stderr: last_error.unwrap_or_else(|| "Unknown error after retries".to_string()),
    })
}
