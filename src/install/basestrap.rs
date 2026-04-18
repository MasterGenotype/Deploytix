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

    // Deploytix — install itself (CLI + GUI) and tkg-gui on the target
    // system so they remain available after first boot for re-deployment
    // and kernel builds.
    // dosfstools is always required for the FAT32 EFI partition.
    packages.extend([
        "deploytix-git".to_string(),
        "deploytix-gui-git".to_string(),
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

    // Seat management — always include seatd and its init service package
    // to resolve provider conflicts (e.g. elogind vs seatd) deterministically.
    packages.push("seatd".to_string());
    if config.system.init != crate::config::InitSystem::S6 {
        let seatd_service = format!("seatd-{}", config.system.init);
        packages.push(seatd_service);
    }

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

    // Desktop environment prerequisites (display server, display manager, audio)
    if config.desktop.environment != DesktopEnvironment::None {
        packages.extend([
            // Display
            "xorg-server".to_string(),
            "xorg-xinit".to_string(),
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
            let greetd_service = format!("greetd-{}", config.system.init);
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

    // yay AUR helper build dependency
    if config.packages.install_yay {
        packages.push("go".to_string());
    }

    // Modular mod manager (optional)
    if config.packages.install_modular {
        packages.push("modular-git".to_string());
    }

    // Decky Loader is installed from the `decky-loader-bin` AUR package in
    // a later phase (yay handles the download); no extra basestrap packages
    // are needed for it.

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
const CUSTOM_PKG_PREFIXES: &[&str] = &[
    "deploytix-git-",
    "deploytix-gui-git-",
    "tkg-gui-git-",
    "modular-git-",
];

/// All custom package names that may live in the [deploytix] repo.
const CUSTOM_PACKAGE_NAMES: &[&str] = &[
    "deploytix-git",
    "deploytix-gui-git",
    "tkg-gui-git",
    "modular-git",
];

/// Path where the ISO live-overlay embeds the deploytix repo.
const ISO_REPO_PATH: &str = "/var/lib/deploytix-repo";

/// Temporary repo the installer creates when no repo is configured.
const TEMP_REPO_DIR: &str = "/tmp/deploytix-local-repo";

/// Temporary pacman.conf that adds the [deploytix] repo.
const TEMP_PACMAN_CONF: &str = "/tmp/deploytix-pacman.conf";

// === Arch Linux [extra] repository support ===
//
// Some packages required by deploytix may live in Arch Linux's
// [extra] repository, which is not enabled by default on Artix.
// The functions below detect this and append the repo to the
// pacman.conf used by basestrap.

/// Geo-balanced Arch Linux mirror used as a fallback when the
/// `mirrorlist-arch` file is not available on the host.
const ARCH_MIRROR_URL: &str = "https://geo.mirror.pkgbuild.com/$repo/os/$arch";

/// Path to the Arch Linux mirrorlist installed by
/// `artix-archlinux-support`.
const ARCH_MIRRORLIST_PATH: &str = "/etc/pacman.d/mirrorlist-arch";

/// Check whether the system's `/etc/pacman.conf` already contains a
/// `[deploytix]` repository section.
fn pacman_conf_has_deploytix_repo() -> bool {
    std::fs::read_to_string("/etc/pacman.conf")
        .map(|contents| contents.lines().any(|line| line.trim() == "[deploytix]"))
        .unwrap_or(false)
}

/// Determine which custom package names from `CUSTOM_PACKAGE_NAMES`
/// actually appear in the basestrap package list and must therefore be
/// resolvable via pacman.
fn needed_custom_packages(package_list: &[String]) -> Vec<&'static str> {
    CUSTOM_PACKAGE_NAMES
        .iter()
        .copied()
        .filter(|name| package_list.iter().any(|p| p == name))
        .collect()
}

/// Check whether the given custom packages are resolvable in the
/// currently configured pacman sync databases.
///
/// If the `[deploytix]` repo is configured but the sync DB hasn't been
/// refreshed yet (common on first boot of the live ISO), this will
/// refresh the deploytix database before checking.
fn custom_packages_in_sync_db(needed: &[&str]) -> bool {
    if needed.is_empty() {
        return true;
    }

    // Quick check without refresh.
    let all_found = |pkgs: &[&str]| {
        pkgs.iter().all(|pkg| {
            std::process::Command::new("pacman")
                .args(["-Si", pkg])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
    };

    if all_found(needed) {
        return true;
    }

    // If the [deploytix] repo is in pacman.conf but packages aren't in
    // the sync DB, it likely means the DB hasn't been downloaded yet
    // (first boot of the live ISO).  Refresh just the deploytix database.
    if pacman_conf_has_deploytix_repo() {
        info!("[deploytix] repo found in pacman.conf; refreshing sync database");
        let _ = std::process::Command::new("pacman")
            .args(["-Sy", "--noconfirm"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        return all_found(needed);
    }

    false
}

/// Resolve the home directory of the user who invoked the installer.
///
/// The installer runs as root, either via `sudo` (sets `SUDO_USER`) or
/// `pkexec`/polkit (sets `PKEXEC_UID`).  This function tries both and
/// falls back to scanning `/home` for a Deploytix checkout.
fn resolve_invoking_user_home() -> Option<PathBuf> {
    // 1. SUDO_USER — set by sudo.
    if let Ok(user) = std::env::var("SUDO_USER") {
        let home = PathBuf::from(format!("/home/{}", user));
        if home.is_dir() {
            return Some(home);
        }
    }

    // 2. PKEXEC_UID — set by pkexec (polkit).
    if let Ok(uid_str) = std::env::var("PKEXEC_UID") {
        if let Ok(uid) = uid_str.parse::<u32>() {
            if let Some(home) = home_dir_for_uid(uid) {
                return Some(home);
            }
        }
    }

    // 3. Scan /home for a directory containing .gitrepos/Deploytix/pkg.
    if let Ok(entries) = std::fs::read_dir("/home") {
        for entry in entries.flatten() {
            let candidate = entry.path();
            if candidate.join(".gitrepos/Deploytix/pkg").is_dir() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Map a numeric UID to its home directory by parsing `/etc/passwd`.
fn home_dir_for_uid(uid: u32) -> Option<PathBuf> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 6 {
            if let Ok(line_uid) = fields[2].parse::<u32>() {
                if line_uid == uid {
                    let home = PathBuf::from(fields[5]);
                    if home.is_dir() {
                        return Some(home);
                    }
                }
            }
        }
    }
    None
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
            .and_then(|p| p.parent())
        // repo root
        {
            search_dirs.push(repo_root.join("pkg"));
            // Sibling tkg-gui and Modular-1 repos.
            if let Some(parent) = repo_root.parent() {
                search_dirs.push(parent.join("tkg-gui/pkg"));
                search_dirs.push(parent.join("Modular-1/pkg"));
            }
        }
    }

    // 2. Invoking user's home (works for both sudo and pkexec/polkit).
    if let Some(home) = resolve_invoking_user_home() {
        search_dirs.push(home.join(".gitrepos/Deploytix/pkg"));
        search_dirs.push(home.join(".gitrepos/tkg-gui/pkg"));
        search_dirs.push(home.join(".gitrepos/Modular-1/pkg"));
        search_dirs.push(home.join("artools-workspace/tkg-gui-src/pkg"));
    }

    // 3. Current working directory (might be repo root).
    search_dirs.push(PathBuf::from("pkg"));

    // 4. System pacman cache — packages previously installed via pacman
    //    will have their archive here.
    search_dirs.push(PathBuf::from("/var/cache/pacman/pkg"));

    // 5. Local artools repo that build-deploytix-iso.sh creates.
    search_dirs.push(PathBuf::from("/var/lib/artools/repos/deploytix"));

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

// === On-demand package building ===

/// Map a custom package name to the repository directory name that
/// contains its PKGBUILD.
fn repo_dir_for_package(pkg_name: &str) -> &'static str {
    match pkg_name {
        "deploytix-git" | "deploytix-gui-git" => "Deploytix",
        "tkg-gui-git" => "tkg-gui",
        "modular-git" => "Modular-1",
        _ => "",
    }
}

/// Search well-known locations for the PKGBUILD directory of a custom
/// package.  Returns the directory containing the PKGBUILD if found.
fn find_pkgbuild_dir(pkg_name: &str) -> Option<PathBuf> {
    let repo_name = repo_dir_for_package(pkg_name);
    if repo_name.is_empty() {
        return None;
    }

    let mut candidates: Vec<PathBuf> = Vec::new();

    // Relative to running binary.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(repo_root) = exe
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
        {
            if repo_name == "Deploytix" {
                candidates.push(repo_root.join("pkg"));
            } else if let Some(parent) = repo_root.parent() {
                candidates.push(parent.join(repo_name).join("pkg"));
            }
        }
    }

    // Invoking user's home.
    if let Some(home) = resolve_invoking_user_home() {
        candidates.push(home.join(format!(".gitrepos/{}/pkg", repo_name)));
        if repo_name == "tkg-gui" {
            candidates.push(home.join("artools-workspace/tkg-gui-src/pkg"));
        }
    }

    // CWD for Deploytix itself.
    if repo_name == "Deploytix" {
        candidates.push(PathBuf::from("pkg"));
    }

    candidates.into_iter().find(|d| d.join("PKGBUILD").exists())
}

/// Resolve the username of the user who invoked the installer (via
/// sudo or pkexec).  `makepkg` refuses to run as root, so we need the
/// original user.
fn resolve_invoking_username() -> Option<String> {
    if let Ok(user) = std::env::var("SUDO_USER") {
        if !user.is_empty() && user != "root" {
            return Some(user);
        }
    }
    if let Ok(uid_str) = std::env::var("PKEXEC_UID") {
        if let Ok(uid) = uid_str.parse::<u32>() {
            return username_for_uid(uid);
        }
    }
    None
}

/// Map a numeric UID to a username by parsing `/etc/passwd`.
fn username_for_uid(uid: u32) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() >= 6 {
            if let Ok(line_uid) = fields[2].parse::<u32>() {
                if line_uid == uid {
                    return Some(fields[0].to_string());
                }
            }
        }
    }
    None
}

/// Build a custom package from its PKGBUILD.
///
/// Runs `makepkg` as the invoking user in the PKGBUILD directory.
/// Returns paths to any `.pkg.tar.zst` files produced.
fn build_package_from_source(pkg_name: &str) -> Vec<PathBuf> {
    let pkgbuild_dir = match find_pkgbuild_dir(pkg_name) {
        Some(dir) => dir,
        None => {
            warn!("No PKGBUILD found for {}", pkg_name);
            return Vec::new();
        }
    };

    let username = match resolve_invoking_username() {
        Some(u) => u,
        None => {
            warn!("Cannot determine invoking user for makepkg (need SUDO_USER or PKEXEC_UID)");
            return Vec::new();
        }
    };

    info!(
        "Building {} from {} as user {}",
        pkg_name,
        pkgbuild_dir.display(),
        username
    );

    let status = std::process::Command::new("sudo")
        .args([
            "-u",
            &username,
            "makepkg",
            "-s",
            "--noconfirm",
            "--needed",
            "--clean",
        ])
        .current_dir(&pkgbuild_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => {
            info!("Successfully built {}", pkg_name);
        }
        Ok(s) => {
            warn!(
                "makepkg for {} exited with code {}",
                pkg_name,
                s.code().unwrap_or(-1)
            );
            return Vec::new();
        }
        Err(e) => {
            warn!("Failed to run makepkg for {}: {}", pkg_name, e);
            return Vec::new();
        }
    }

    // Collect built packages.
    let mut built = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&pkgbuild_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if name.ends_with(".pkg.tar.zst") {
                let is_custom = CUSTOM_PKG_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix));
                if is_custom {
                    built.push(path);
                }
            }
        }
    }

    built
}

/// Identify which of the needed custom packages are not yet covered by
/// the pre-built package files already located.
fn find_missing_packages<'a>(needed: &[&'a str], found: &[PathBuf]) -> Vec<&'a str> {
    needed
        .iter()
        .copied()
        .filter(|name| {
            let prefix = format!("{}-", name);
            !found.iter().any(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(&prefix))
                    .unwrap_or(false)
            })
        })
        .collect()
}

/// Try to build any missing custom packages from source.  Returns
/// newly built package paths that should be added to the repo.
fn build_missing_packages(missing: &[&str]) -> Vec<PathBuf> {
    let mut built = Vec::new();

    // Deduplicate PKGBUILD dirs — deploytix-git and deploytix-gui-git
    // share a single PKGBUILD.
    let mut attempted_dirs = HashSet::new();

    for pkg_name in missing {
        let repo = repo_dir_for_package(pkg_name);
        if !attempted_dirs.insert(repo) {
            // Already built from this PKGBUILD directory.
            continue;
        }
        built.extend(build_package_from_source(pkg_name));
    }

    built
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
    let system_conf = std::fs::read_to_string("/etc/pacman.conf").map_err(DeploytixError::Io)?;

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
/// `package_list` is the full list of package names that basestrap will
/// install — only custom packages that actually appear in this list
/// need to be available.
///
/// Returns `Some(path)` to a custom `pacman.conf` if one was created
/// (use with `basestrap -C`), or `None` if the packages are already in
/// a configured repository.
pub fn prepare_deploytix_repo(
    cmd: &CommandRunner,
    package_list: &[String],
) -> Result<Option<String>> {
    if cmd.is_dry_run() {
        info!("[dry-run] Would ensure deploytix local repo is available");
        return Ok(None);
    }

    let needed = needed_custom_packages(package_list);
    if needed.is_empty() {
        info!("No custom packages in package list; skipping repo preparation");
        return Ok(None);
    }

    info!("Custom packages needed: {}", needed.to_vec().join(", "));

    // Fast-path: packages already resolvable in configured repos.
    if custom_packages_in_sync_db(&needed) {
        info!("Deploytix custom packages found in configured repositories");
        return Ok(None);
    }

    info!("Deploytix custom packages not in repos; preparing local repository");

    // 1. ISO-embedded repo already has a database — use it.
    let iso_db = Path::new(ISO_REPO_PATH).join("deploytix.db.tar.zst");
    if iso_db.exists() {
        if pacman_conf_has_deploytix_repo() {
            info!("ISO-embedded repo exists and [deploytix] in pacman.conf; retrying sync");
            let _ = std::process::Command::new("pacman")
                .args(["-Sy", "--noconfirm"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if custom_packages_in_sync_db(&needed) {
                return Ok(None);
            }
        }
        info!("Using ISO-embedded repo at {}", ISO_REPO_PATH);
        return write_custom_pacman_conf(ISO_REPO_PATH);
    }

    // 2. Search for pre-built package files (includes pacman cache and
    //    artools local repo in addition to source tree locations).
    let mut packages = locate_prebuilt_packages();

    // 3. Identify packages still missing and attempt to build them
    //    from source if PKGBUILDs are available.
    let missing = find_missing_packages(&needed, &packages);
    if !missing.is_empty() {
        info!(
            "Missing pre-built packages: {}; attempting to build from source",
            missing.join(", ")
        );
        let newly_built = build_missing_packages(&missing);
        if !newly_built.is_empty() {
            info!("Built {} package file(s) from source", newly_built.len());
            packages.extend(newly_built);
        }
    }

    // 4. Final check — are all needed packages now available?
    let still_missing = find_missing_packages(&needed, &packages);
    if packages.is_empty() || !still_missing.is_empty() {
        let missing_str = if still_missing.is_empty() {
            needed.join(", ")
        } else {
            still_missing.join(", ")
        };
        return Err(DeploytixError::ConfigError(format!(
            "Cannot resolve custom packages: {}\n\
             These packages are not in any configured pacman repository, \
             not found as pre-built .pkg.tar.zst files, and could not be \
             built from source.\n\n\
             To fix, try one of:\n\
             - Run from the Deploytix live ISO (has all packages embedded)\n\
             - Build packages first: cd pkg && makepkg -s\n\
             - Clone sibling repos (tkg-gui, Modular-1) and build \
               their PKGBUILDs\n\
             - Run iso/build-deploytix-iso.sh to build everything at once",
            missing_str
        )));
    }

    info!(
        "Found {} pre-built package file(s); creating temporary repo",
        packages.len()
    );
    create_temp_repo(cmd, &packages)?;
    write_custom_pacman_conf(TEMP_REPO_DIR)
}

// === Arch Linux [extra] repository detection / injection ===

/// Check whether a pacman.conf string contains the Arch `[extra]` repo.
fn conf_has_arch_extra(conf: &str) -> bool {
    conf.lines().any(|line| line.trim() == "[extra]")
}

/// Ensure the Arch Linux `[extra]` repository is available in the
/// pacman configuration used by basestrap.
///
/// Some packages live in Arch's `[extra]` repo and are not mirrored
/// in the Artix repositories.  If the effective config already
/// contains `[extra]` this is a no-op; otherwise a custom pacman.conf
/// is written (or updated) with the repo appended.
fn ensure_arch_repos(existing_conf: Option<String>, cmd: &CommandRunner) -> Result<Option<String>> {
    if cmd.is_dry_run() {
        return Ok(existing_conf);
    }

    let conf_path = existing_conf.as_deref().unwrap_or("/etc/pacman.conf");

    let conf_content = std::fs::read_to_string(conf_path).map_err(DeploytixError::Io)?;

    if conf_has_arch_extra(&conf_content) {
        return Ok(existing_conf);
    }

    info!("Arch [extra] repository not configured; adding it");

    let mirror_entry = if Path::new(ARCH_MIRRORLIST_PATH).exists() {
        format!("Include = {}", ARCH_MIRRORLIST_PATH)
    } else {
        format!("Server = {}", ARCH_MIRROR_URL)
    };

    let updated = format!(
        "{}\n\n\
         # Arch Linux [extra] repository (auto-added by deploytix installer)\n\
         [extra]\n\
         SigLevel = Optional TrustAll\n\
         {}\n",
        conf_content.trim_end(),
        mirror_entry,
    );

    std::fs::write(TEMP_PACMAN_CONF, &updated).map_err(DeploytixError::Io)?;

    info!(
        "Updated pacman.conf at {} with Arch [extra] repository",
        TEMP_PACMAN_CONF,
    );

    Ok(Some(TEMP_PACMAN_CONF.to_string()))
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
    // Build the package list first so we know exactly which custom
    // packages need to be resolved.
    let packages = build_package_list(config);

    // Ensure the custom [deploytix] packages are available.
    let custom_conf = prepare_deploytix_repo(cmd, &packages)?;

    // Ensure the Arch [extra] repo is available for packages that
    // are not mirrored in the Artix repositories.
    let custom_conf = ensure_arch_repos(custom_conf, cmd)?;

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
