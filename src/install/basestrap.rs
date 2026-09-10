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
        crate::init::module(&config.system.init)
            .base_package
            .to_string(),
    ]);

    // For s6, pre-select providers to avoid interactive prompts
    if config.system.init == crate::config::InitSystem::S6 {
        // s6-frontend ships the `s6` one-stop CLI the installer uses to
        // enable services in the chroot (`s6 set enable` / `s6 set commit`).
        packages.push("s6-frontend".to_string());
        // D-Bus provider for s6; no elogind, use seatd for seats
        packages.push("dbus-s6".to_string());
        // no elogind-s6
        // Core s6 service packages
        packages.push("networkmanager-s6".to_string());
        packages.push("seatd-s6".to_string());
        packages.push("iwd-s6".to_string());
    }

    // Kernel and firmware.  linux-tkg replaces linux-zen rather than joining
    // it, so basestrap installs no kernel at all in that case — the prebuilt
    // packages come down over HTTP and go in with `pacman -U` immediately
    // after this phase, before mkinitcpio and grub-mkconfig run.  That step
    // is fatal on failure precisely because nothing here is a fallback.
    packages.push("linux-firmware".to_string());
    if !config.packages.install_tkg_kernel {
        packages.extend(["linux-zen".to_string(), "linux-zen-headers".to_string()]);
    }

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

    // The graphical updater drives `deploytix update`, which only exists on an
    // immutable root. On a mutable install there are no snapshots and pacman
    // works normally, so it is not installed at all — no binary, no desktop
    // entry, no polkit action.
    if config.packages.immutable_root {
        packages.push("deploytix-update-gui-git".to_string());
    }

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

    // elogind — installed alongside any desktop because greetd's PAM stack
    // (pam_elogind) needs the elogind D-Bus service to create the seat
    // session that grants DRM/input ACLs.  Only the base package is
    // installed; the init-specific elogind-<init> service package conflicts
    // with seatd-<init> (both ship a login1 seat manager unit) and is
    // blacklisted in configure::services::build_service_packages().
    if config.desktop.environment != crate::config::DesktopEnvironment::None {
        packages.push("elogind".to_string());
    }

    // Python 3 — the greetd-ipc helper script (greetd-ipc.py) that
    // deploytix-session-manager uses to create Class=user sessions
    // via the greetd IPC protocol is written in Python.  Only needed
    // when session switching is active.
    if config.packages.install_session_switching
        && config.desktop.environment != crate::config::DesktopEnvironment::None
    {
        packages.push("python".to_string());
    }

    // Network packages based on config.  AUR-only frontends (iwgtk, iwdgui,
    // iwqt) are NOT installed here — they ship via yay in install_iwd_frontend
    // after the user account exists.
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
        NetworkBackend::NetworkManagerWpa => {
            packages.extend([
                "networkmanager".to_string(),
                "wpa_supplicant".to_string(),
                "openresolv".to_string(),
            ]);
            if config.system.init != crate::config::InitSystem::S6 {
                let nm_service_pkg = format!("networkmanager-{}", config.system.init);
                let wpa_service_pkg = format!("wpa_supplicant-{}", config.system.init);
                packages.push(nm_service_pkg);
                packages.push(wpa_service_pkg);
            }
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
        } else if let Some(dm) = config.desktop.display_manager.service_name() {
            // Init service package for the selected display manager
            // (greetd-runit, sddm-dinit, …). The base DM package and the
            // s6 variants are handled by configure::services.
            let dm_service = format!("{}-{}", dm, config.system.init);
            packages.push(dm_service);
        }
    }

    // Encryption tools (if enabled). The LVM immutable A/B backend also needs
    // cryptsetup even when unencrypted: it provides veritysetup, which the
    // verity-ab initramfs hook runs at boot to open the dm-verity root.
    if config.disk.encryption || config.immutable_lvm_ab() {
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

    // Gamescope (Bazzite fork, pre-built) — installed via basestrap so the
    // custom [deploytix] repo is available.  Runtime dependencies are
    // declared in the PKGBUILD and pulled in automatically by pacman.
    if config.packages.install_gaming {
        packages.push("gamescope-git".to_string());
        // libliftoff: KMS plane management library used by gamescope
        packages.push("libliftoff".to_string());
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
// repository rather than in the standard Artix mirrors.  On the live ISO this
// repo is embedded at /var/lib/deploytix-repo and referenced in
// /etc/pacman.conf.  When the installer runs outside that environment we build
// the repo at that same path from any pre-built .pkg.tar.zst files we can
// locate, and pass `-C <config>` to basestrap.
//
// The repo is then copied into the system being installed
// (`install_local_repo_into_target`), which is what makes a deploytix-installed
// machine able to install another one.  Without that copy the chain dead-ends:
// the ISO's repo exists only in the live environment, so a machine installed
// from it has deploytix but no repo, no source clone, and no way to resolve
// deploytix-git — the one package that cannot be built standalone because its
// PKGBUILD builds from the repo tree itself.

/// Filename prefixes (with trailing dash) for package archives that
/// belong to the custom [deploytix] repository.
const CUSTOM_PKG_PREFIXES: &[&str] = &[
    "deploytix-git-",
    "deploytix-gui-git-",
    "deploytix-update-gui-git-",
    "gamescope-git-",
    "tkg-gui-git-",
];

/// All custom package names that may live in the [deploytix] repo.
const CUSTOM_PACKAGE_NAMES: &[&str] = &[
    "deploytix-git",
    "deploytix-gui-git",
    "deploytix-update-gui-git",
    "gamescope-git",
    "tkg-gui-git",
];

/// PKGBUILDs embedded in the binary so the installer can build the custom
/// packages with no clone of the deploytix repo present.
///
/// Only self-contained PKGBUILDs can live here — ones whose `source=()` is an
/// upstream git URL, so `makepkg` fetches everything itself from just this
/// file. `pkg/PKGBUILD` (deploytix-git/deploytix-gui-git) deliberately is not
/// among them: it builds from `$startdir/..` with an empty `source=()`, so it
/// needs the repo tree and cannot be materialised standalone.
///
/// Keyed by the repo directory name from [`repo_dir_for_package`].
const EMBEDDED_PKGBUILDS: &[(&str, &str)] = &[
    // Same file the deployed `deploytix-update-gamescope` rebuilds from, so a
    // freshly installed gamescope and a later in-place update are byte-for-byte
    // the same build configuration.
    (
        "gamescope",
        include_str!("../resources/gamescope_update/PKGBUILD"),
    ),
    (
        "tkg-gui",
        include_str!("../resources/custom_pkgbuilds/tkg-gui.PKGBUILD"),
    ),
];

/// Directory under which embedded PKGBUILDs are materialised for building.
///
/// `/var/tmp` rather than `/tmp`: these builds need several GB of scratch
/// space (gamescope compiles a whole compositor plus wlroots), and `/tmp` is a
/// RAM-backed tmpfs on the live ISO and on many desktop installs.
const EMBEDDED_BUILD_ROOT: &str = "/var/tmp/deploytix-pkgbuild";

/// Unprivileged account used for `makepkg` when the installer was not started
/// through sudo/pkexec (e.g. a root shell on the live ISO) and there is
/// therefore no invoking user to drop to.
const FALLBACK_BUILD_USER: &str = "nobody";

/// The one place a `[deploytix]` repo lives, on any host.
///
/// The ISO live-overlay embeds its pre-built packages here, an installer
/// creating a repo from source builds it here, and an installed system
/// receives a copy here (see [`install_local_repo_into_target`]). One path
/// everywhere means the lookup below does not need to know which kind of host
/// it is running on.
///
/// `/var` specifically, because of what an immutable root does to the
/// alternatives. `/` and `/usr` are mounted read-only, so nothing can be
/// written there at all; `/tmp` is a boot-wiped subvolume, so a repo left
/// there does not survive a reboot. `/var` is writable, is *not* snapshotted
/// — so a repo survives `deploytix rollback` — and is one of the mounts
/// rbind-mounted into every snapshot set's chroot
/// (`crate::immutable::SHARED_LIVE_MOUNTS`), which is what lets a
/// `file:///var/lib/deploytix-repo` server resolve inside a `deploytix update`
/// transaction as well as on the host.
pub const SHARED_REPO_DIR: &str = "/var/lib/deploytix-repo";

/// Where a repo is built when [`SHARED_REPO_DIR`] cannot be written — a live
/// medium mounted read-only, most likely. Good enough for the install in
/// progress, gone on the next boot.
///
/// Resolved by [`crate::utils::paths::cache_dir`] rather than being a `/tmp`
/// literal: on a deployed immutable host `/tmp` is read-only (the LVM A/B
/// backend seals `/` with dm-verity and overlays only `/etc`), so the fallback
/// would have failed in exactly the situation it exists for.
fn fallback_repo_dir() -> String {
    crate::utils::paths::cache_path("repo")
}

/// Generated pacman.conf that adds the [deploytix] repo.
///
/// Beside the fallback repo, and off `/tmp` for the same reason — note this one
/// is written on the **success** path too (`write_custom_pacman_conf` runs even
/// when the shared repo resolved cleanly), so a read-only `/tmp` failed the
/// install regardless of where the repo itself ended up.
fn temp_pacman_conf() -> String {
    crate::utils::paths::cache_path("pacman.conf")
}

/// The directory this process will build a local repo in.
///
/// [`SHARED_REPO_DIR`] when it can be created and written, otherwise
/// [`fallback_repo_dir`]. Resolved once: the answer cannot change during a
/// run, and re-probing risks two call sites disagreeing about where the repo
/// is.
fn local_repo_dir() -> &'static str {
    static DIR: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        if dir_is_writable(SHARED_REPO_DIR) {
            SHARED_REPO_DIR
        } else {
            // Leaked once, so the contract stays `&'static str` for the call
            // sites that compare it. The fallback path is probed, so it is not
            // a constant.
            let fallback: &'static str = Box::leak(fallback_repo_dir().into_boxed_str());
            warn!(
                "{} is not writable; building the local repo at {} instead. \
                 It will not survive a reboot.",
                SHARED_REPO_DIR, fallback
            );
            fallback
        }
    })
}

/// Whether `dir` exists (or can be created) *and* can be written to.
///
/// Existence is not enough: a live medium can carry a perfectly good
/// `/var/lib/deploytix-repo` on a read-only mount, where `create_dir_all`
/// succeeds trivially and the first `repo-add` then fails with EROFS.
fn dir_is_writable(dir: &str) -> bool {
    crate::utils::paths::dir_is_writable(Path::new(dir))
}

/// The package files already sitting in the shared repo.
///
/// On the live ISO these are the packages the image embedded; on a machine
/// deploytix installed they are the ones it was handed at install time.
fn packages_in_shared_repo() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(SHARED_REPO_DIR) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".pkg.tar.zst"))
        })
        .collect()
}

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
/// Wraps [`crate::utils::user::invoking_user_home`] with an installer-specific
/// last resort: scan `/home` for a deploytix checkout, which covers a plain
/// root shell where neither `SUDO_USER` nor `PKEXEC_UID` is set.
fn resolve_invoking_user_home() -> Option<PathBuf> {
    if let Some(home) = crate::utils::user::invoking_user_home() {
        return Some(home);
    }

    let scan_markers = [".gitrepos/deploytix/pkg"];
    if let Ok(entries) = std::fs::read_dir("/home") {
        for entry in entries.flatten() {
            let candidate = entry.path();
            if scan_markers.iter().any(|m| candidate.join(m).is_dir()) {
                return Some(candidate);
            }
        }
    }

    None
}

/// Search well-known directories for pre-built `.pkg.tar.zst` files
/// that belong to the deploytix custom packages.
fn locate_prebuilt_packages() -> Vec<PathBuf> {
    let mut search_dirs: Vec<PathBuf> = Vec::new();

    // Everything is anchored to the invoking user's deploytix repo clone:
    // pkg/ holds deploytix's own PKGBUILD/packages, vendor/ the vendored
    // submodules. No binary- or CWD-relative guessing.
    let invoking_home = resolve_invoking_user_home();
    if let Some(ref home) = invoking_home {
        info!("Resolved invoking user home: {}", home.display());
        search_dirs.push(home.join(".gitrepos/deploytix/pkg"));
        search_dirs.push(home.join(".gitrepos/deploytix/vendor/gamescope/pkg"));
        search_dirs.push(home.join(".gitrepos/deploytix/vendor/tkg-gui/pkg"));
    } else {
        warn!(
            "Could not resolve invoking user home (SUDO_USER={:?}, PKEXEC_UID={:?})",
            std::env::var("SUDO_USER").ok(),
            std::env::var("PKEXEC_UID").ok(),
        );
    }

    info!(
        "Package search directories: {:?}",
        search_dirs
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
    );

    let mut found = Vec::new();
    let mut seen_names = HashSet::new();

    for dir in &search_dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => {
                info!("  {} — not found / unreadable", dir.display());
                continue;
            }
        };
        info!("  {} — scanning", dir.display());
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
            if is_custom && seen_names.insert(name.clone()) {
                info!("    Found: {}", path.display());
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
        "deploytix-git" | "deploytix-gui-git" | "deploytix-update-gui-git" => "deploytix",
        "gamescope-git" => "gamescope",
        "tkg-gui-git" => "tkg-gui",
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

    // Anchored to the invoking user's deploytix repo clone: pkg/ for
    // deploytix's own PKGBUILD, vendor/<repo> for the vendored submodules
    // (whose PKGBUILD may live at the submodule root or in its pkg/
    // subdirectory).
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = resolve_invoking_user_home() {
        if repo_name == "deploytix" {
            candidates.push(home.join(".gitrepos/deploytix/pkg"));
        } else {
            candidates.push(home.join(format!(".gitrepos/deploytix/vendor/{}/pkg", repo_name)));
            candidates.push(home.join(format!(".gitrepos/deploytix/vendor/{}", repo_name)));
        }
    }

    candidates.into_iter().find(|d| d.join("PKGBUILD").exists())
}

/// The embedded PKGBUILD for `pkg_name`, if one exists.
fn embedded_pkgbuild_for(pkg_name: &str) -> Option<&'static str> {
    let repo = repo_dir_for_package(pkg_name);
    EMBEDDED_PKGBUILDS
        .iter()
        .find(|(name, _)| *name == repo)
        .map(|(_, contents)| *contents)
}

/// Write the embedded PKGBUILD for `pkg_name` into its own build directory
/// under [`EMBEDDED_BUILD_ROOT`], owned by `build_user` so `makepkg` (which
/// refuses to run as root) can write its `src`/`pkg` trees there.
///
/// Returns the directory containing the PKGBUILD.
fn materialize_embedded_pkgbuild(pkg_name: &str, build_user: &str) -> Option<PathBuf> {
    let contents = embedded_pkgbuild_for(pkg_name)?;
    let dir = Path::new(EMBEDDED_BUILD_ROOT).join(repo_dir_for_package(pkg_name));

    // Start from a clean directory so a previous failed run cannot leak
    // stale sources or packages into this build.
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!("Could not create build dir {}: {}", dir.display(), e);
        return None;
    }
    if let Err(e) = std::fs::write(dir.join("PKGBUILD"), contents) {
        warn!("Could not write PKGBUILD into {}: {}", dir.display(), e);
        return None;
    }

    // makepkg writes srcdir/pkgdir alongside the PKGBUILD, so the build user
    // needs to own the tree.  chown -R is the portable way to do this without
    // pulling in a users/groups crate.
    let status = std::process::Command::new("chown")
        .args(["-R", &format!("{build_user}:"), &dir.to_string_lossy()])
        .status();
    match status {
        Ok(s) if s.success() => {}
        _ => {
            warn!(
                "Could not chown {} to {}; makepkg may fail",
                dir.display(),
                build_user
            );
        }
    }

    info!(
        "Materialised embedded PKGBUILD for {} at {}",
        pkg_name,
        dir.display()
    );
    Some(dir)
}

/// Account to run `makepkg` as: the user who invoked the installer, or
/// [`FALLBACK_BUILD_USER`] when it was started directly as root (no
/// `SUDO_USER`/`PKEXEC_UID`), which is the common case on the live ISO.
///
/// Returns `None` only when neither is usable, which makes building impossible
/// since `makepkg` refuses to run as root.
fn resolve_build_user() -> Option<String> {
    if let Some(user) = crate::utils::user::invoking_username() {
        return Some(user);
    }
    if crate::utils::user::user_exists(FALLBACK_BUILD_USER) {
        info!(
            "No invoking user (not started via sudo/pkexec); building as {}",
            FALLBACK_BUILD_USER
        );
        return Some(FALLBACK_BUILD_USER.to_string());
    }
    None
}

/// Create a writable scratch home for `build_user` and return it.
///
/// Only used for [`FALLBACK_BUILD_USER`], whose real home is not writable (or
/// does not exist). Returns `None` if it could not be prepared, in which case
/// the build simply runs with whatever HOME sudo provides.
fn ensure_scratch_home(build_user: &str) -> Option<PathBuf> {
    let home = Path::new(EMBEDDED_BUILD_ROOT).join(".home");
    if let Err(e) = std::fs::create_dir_all(&home) {
        warn!("Could not create scratch home {}: {}", home.display(), e);
        return None;
    }
    let ok = std::process::Command::new("chown")
        .args(["-R", &format!("{build_user}:"), &home.to_string_lossy()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        warn!("Could not chown scratch home {}", home.display());
        return None;
    }
    Some(home)
}

/// Build a custom package from its PKGBUILD.
///
/// Prefers a PKGBUILD from a local clone of the deploytix repo (so a
/// developer's local edits win), and otherwise materialises the copy embedded
/// in this binary — which is what lets a plain `deploytix install` succeed with
/// no clone, no submodules and no pre-built packages.
///
/// Runs `makepkg` as an unprivileged user, since it refuses to run as root.
/// Returns paths to any `.pkg.tar.zst` files produced.
fn build_package_from_source(pkg_name: &str) -> Vec<PathBuf> {
    let username = match resolve_build_user() {
        Some(u) => u,
        None => {
            warn!(
                "Cannot determine an unprivileged user for makepkg \
                 (no SUDO_USER/PKEXEC_UID and no '{}' account)",
                FALLBACK_BUILD_USER
            );
            return Vec::new();
        }
    };

    let pkgbuild_dir = match find_pkgbuild_dir(pkg_name) {
        Some(dir) => {
            info!("Using PKGBUILD from local repo: {}", dir.display());
            dir
        }
        None => match materialize_embedded_pkgbuild(pkg_name, &username) {
            Some(dir) => dir,
            None => {
                warn!(
                    "No PKGBUILD found for {} — none in a local repo clone and \
                     none embedded (it needs the repo tree to build)",
                    pkg_name
                );
                return Vec::new();
            }
        },
    };

    info!(
        "Building {} from {} as user {} — this can take a while",
        pkg_name,
        pkgbuild_dir.display(),
        username
    );

    // The fallback account has no usable home (`nobody` is typically
    // /nonexistent), but every one of these packages builds with cargo, which
    // needs a writable HOME for ~/.cargo. Give it a scratch home rather than
    // letting the build fail on an unwritable registry path. The invoking
    // user's real home is left alone so their cargo cache is reused.
    let scratch_home = (username == FALLBACK_BUILD_USER)
        .then(|| ensure_scratch_home(&username))
        .flatten();

    // sudo's env_reset drops the caller's environment, so HOME has to be set
    // on the far side of it via `env`.
    let mut command = std::process::Command::new("sudo");
    command.args(["-u", &username]);
    if let Some(ref home) = scratch_home {
        command.args([
            "env",
            &format!("HOME={}", home.display()),
            &format!("CARGO_HOME={}/.cargo", home.display()),
        ]);
    }
    command.args(["makepkg", "-s", "--noconfirm", "--needed", "--clean"]);

    // Build output is inherited rather than discarded: these builds compile
    // Rust and (for gamescope) a full compositor, so silence for many minutes
    // reads as a hang.
    let status = command
        .current_dir(&pkgbuild_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
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

/// Add the given package files to the local pacman repository, creating it if
/// it does not exist, and rebuild its database with `repo-add`.
///
/// Additive rather than destructive: the repo is a shared, persistent location
/// that may already hold packages this run did not build — ones embedded by
/// the ISO, or left by an earlier install. The database is rebuilt from
/// whatever files are present afterwards, so the two never drift apart.
fn create_local_repo(cmd: &CommandRunner, packages: &[PathBuf]) -> Result<&'static str> {
    let repo_dir = local_repo_dir();
    let repo = Path::new(repo_dir);
    std::fs::create_dir_all(repo).map_err(DeploytixError::Io)?;

    for pkg in packages {
        let dest = repo.join(pkg.file_name().unwrap());
        // Already in the repo (a rebuild of the same version, or a package the
        // ISO embedded); copying a file onto itself would truncate it.
        if dest == *pkg {
            continue;
        }
        std::fs::copy(pkg, &dest).map_err(DeploytixError::Io)?;
        info!("  Copied {} into the local repo", dest.display());
    }

    // Build the pacman database.
    let db_path = format!("{}/deploytix.db.tar.zst", repo_dir);
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

    info!("Local deploytix repo ready at {}", repo_dir);
    Ok(repo_dir)
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

    let conf_path = temp_pacman_conf();
    if let Some(parent) = Path::new(&conf_path).parent() {
        std::fs::create_dir_all(parent).map_err(DeploytixError::Io)?;
    }
    std::fs::write(&conf_path, &custom).map_err(DeploytixError::Io)?;

    info!(
        "Custom pacman.conf written to {} (repo: file://{})",
        conf_path, repo_dir
    );
    Ok(Some(conf_path))
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

    // 1. A repo already exists at the shared location, with a database. This
    //    is the ISO's embedded repo when running from the live medium, and the
    //    copy `install_local_repo_into_target` left behind when running from a
    //    system deploytix installed — including an immutable one, where /var
    //    is the only place such a repo could have survived.
    let shared_db = Path::new(SHARED_REPO_DIR).join("deploytix.db.tar.zst");
    let in_shared_repo = packages_in_shared_repo();
    if shared_db.exists() && find_missing_packages(&needed, &in_shared_repo).is_empty() {
        if pacman_conf_has_deploytix_repo() {
            info!("Local repo exists and [deploytix] is in pacman.conf; retrying sync");
            let _ = std::process::Command::new("pacman")
                .args(["-Sy", "--noconfirm"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if custom_packages_in_sync_db(&needed) {
                return Ok(None);
            }
        }
        info!("Using the local repo at {}", SHARED_REPO_DIR);
        return write_custom_pacman_conf(SHARED_REPO_DIR);
    }

    // 2. Search for pre-built package files (includes pacman cache and
    //    artools local repo in addition to source tree locations). Whatever
    //    the shared repo already holds counts: a repo missing only the newest
    //    package should have the rest of it reused, not rebuilt.
    let mut packages = locate_prebuilt_packages();
    packages.extend(in_shared_repo);

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
        // Split the report: packages with an embedded PKGBUILD were actually
        // attempted and failed (network, makedepends, a broken build), which is
        // a different problem from one that can only come from the repo tree.
        let (buildable, needs_tree): (Vec<&str>, Vec<&str>) = still_missing
            .iter()
            .copied()
            .partition(|p| embedded_pkgbuild_for(p).is_some());

        let mut hint = String::new();
        if !buildable.is_empty() {
            hint.push_str(&format!(
                "\n{} can be built from source without a repo clone, so the \
                 build itself failed. Check the makepkg output above; the usual \
                 causes are no network access to fetch the sources, or missing \
                 makedepends that pacman could not install.\n",
                buildable.join(", ")
            ));
        }
        if !needs_tree.is_empty() {
            hint.push_str(&format!(
                "\n{} is built from the deploytix repo tree itself and cannot be \
                 fetched standalone. It has to come from a repository:\n  \
                 - the Deploytix live ISO has it pre-built, or\n  \
                 - {} on a machine deploytix installed (this one appears to be \
                 missing or incomplete — deploytix copies it there during an \
                 install, so a machine installed by an older version will not \
                 have it), or\n  \
                 - run the installer as a user whose ~/.gitrepos/deploytix \
                 holds the repo tree.\n",
                needs_tree.join(", "),
                SHARED_REPO_DIR,
            ));
        }

        return Err(DeploytixError::ConfigError(format!(
            "Cannot resolve custom packages: {}\n\
             These packages are not in any configured pacman repository, were \
             not found as pre-built .pkg.tar.zst files, and could not be built.\n\
             {}",
            missing_str, hint
        )));
    }

    info!(
        "Found {} pre-built package file(s); creating local repo",
        packages.len()
    );
    let repo_dir = create_local_repo(cmd, &packages)?;
    write_custom_pacman_conf(repo_dir)
}

/// The `[deploytix]` section to add to a pacman.conf, pointing at the repo on
/// the system that config belongs to.
fn deploytix_repo_section() -> String {
    format!(
        "\n# Deploytix local repository (packages built for this machine)\n\
         [deploytix]\n\
         SigLevel = Optional TrustAll\n\
         Server = file://{}\n",
        SHARED_REPO_DIR
    )
}

/// Copy the local repo into the system being installed and point its
/// pacman.conf at it.
///
/// This is what stops the repo chain dead-ending at the live ISO. The ISO's
/// embedded repo exists only in the live environment, so before this a machine
/// installed by deploytix had the deploytix packages installed but no repo to
/// resolve them from again — and `deploytix-git` cannot be rebuilt from
/// nothing, because its PKGBUILD builds from a checkout of the repo tree. The
/// second install from such a machine therefore failed at
/// [`prepare_deploytix_repo`] with "Cannot resolve custom packages".
///
/// On an immutable target this lands on `/var`, which is writable, is not
/// snapshotted (so a rollback cannot take the repo with it), and is
/// rbind-mounted into every snapshot set's chroot, so `deploytix update` can
/// resolve from it too.
///
/// Best-effort by design: a missing repo means this install did not need one
/// (every custom package came from a configured repository), which is not a
/// failure. The install has already succeeded by the time this runs, and
/// nothing here is worth failing it over.
pub fn install_local_repo_into_target(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    if cmd.is_dry_run() {
        info!(
            "[dry-run] Would copy the local repo to {}{}",
            install_root, SHARED_REPO_DIR
        );
        return Ok(());
    }

    install_repo_from(Path::new(local_repo_dir()), install_root)
}

/// The body of [`install_local_repo_into_target`], with the source directory
/// passed in so it can be exercised against a fixture.
fn install_repo_from(source: &Path, install_root: &str) -> Result<()> {
    if !source.join("deploytix.db.tar.zst").exists() {
        info!(
            "No local repo at {} to hand to the installed system",
            source.display()
        );
        return Ok(());
    }

    let dest = format!("{}{}", install_root, SHARED_REPO_DIR);
    std::fs::create_dir_all(&dest).map_err(DeploytixError::Io)?;

    let mut copied = 0usize;
    for entry in std::fs::read_dir(source)
        .map_err(DeploytixError::Io)?
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        match std::fs::copy(&path, Path::new(&dest).join(name)) {
            Ok(_) => copied += 1,
            Err(e) => warn!("Could not copy {} into the target: {}", path.display(), e),
        }
    }

    if copied == 0 {
        warn!("Local repo at {} held no files to copy", source.display());
        return Ok(());
    }

    // Only now that the target actually holds packages is it safe to name the
    // repo in its pacman.conf: a `file://` server pointing at a directory that
    // does not exist makes every later `pacman -Sy` on that machine fail.
    let conf_path = format!("{}/etc/pacman.conf", install_root);
    match std::fs::read_to_string(&conf_path) {
        Ok(conf) if conf.lines().any(|l| l.trim() == "[deploytix]") => {
            info!("[deploytix] already present in the target's pacman.conf");
        }
        Ok(conf) => {
            let updated = format!("{}\n{}", conf.trim_end(), deploytix_repo_section());
            std::fs::write(&conf_path, updated).map_err(DeploytixError::Io)?;
            info!("Added [deploytix] to {}", conf_path);
        }
        Err(e) => {
            warn!("Could not read {} to add [deploytix]: {}", conf_path, e);
            return Ok(());
        }
    }

    info!(
        "Handed {} repo file(s) to the installed system at {}",
        copied, SHARED_REPO_DIR
    );
    Ok(())
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

    let conf_path = temp_pacman_conf();
    if let Some(parent) = Path::new(&conf_path).parent() {
        std::fs::create_dir_all(parent).map_err(DeploytixError::Io)?;
    }
    std::fs::write(&conf_path, &updated).map_err(DeploytixError::Io)?;

    info!(
        "Updated pacman.conf at {} with Arch [extra] repository",
        conf_path,
    );

    Ok(Some(conf_path))
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
    let mut packages = build_package_list(config);

    // Submit to the interactive policy (no-op when none attached).  The
    // user may edit the package list, skip the install, or cancel.
    let inv = crate::utils::interactive::PacmanInvocation::basestrap(install_root, packages);
    let Some(inv) = cmd.review_pacman(inv)? else {
        info!("basestrap skipped by interactive policy");
        return Ok(());
    };
    packages = inv.packages;
    let extra_flags = inv.extra_flags;

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
    // pacman.conf was generated for the [deploytix] repo, then any
    // user-supplied extra flags from the policy edit, then the install
    // root and packages.
    let mut args: Vec<&str> = Vec::new();
    if let Some(ref conf_path) = custom_conf {
        args.push("-C");
        args.push(conf_path.as_str());
    }
    for f in &extra_flags {
        args.push(f.as_str());
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Neither the fallback repo nor the generated pacman.conf may be a `/tmp`
    /// literal. `SHARED_REPO_DIR` covers the normal case, but the fallback runs
    /// precisely when the normal case failed, and `write_custom_pacman_conf`
    /// runs on the success path too — so a read-only `/tmp` (a deployed
    /// immutable host, where `/` is a sealed dm-verity image and only `/etc` is
    /// overlaid) failed the install either way.
    #[test]
    fn the_fallback_repo_and_the_generated_config_are_not_on_tmp() {
        for p in [fallback_repo_dir(), temp_pacman_conf()] {
            assert!(
                !p.starts_with("/tmp/"),
                "{p} is on /tmp, which a deployed immutable host mounts read-only"
            );
        }
        // Same directory, so the `Server = file://` line and the file carrying
        // it can never land on different filesystems.
        assert_eq!(
            std::path::Path::new(&fallback_repo_dir()).parent(),
            std::path::Path::new(&temp_pacman_conf()).parent()
        );
    }

    /// linux-tkg replaces the stock kernel rather than joining it, so
    /// basestrap must not also pull linux-zen in: two kernels is not the
    /// configured outcome, and on the ZFS path `zfs-linux-zen` would quietly
    /// drag the zen kernel back as a dependency.
    #[test]
    fn the_tkg_kernel_replaces_linux_zen_in_the_package_list() {
        let mut config = DeploymentConfig::sample();

        let stock = build_package_list(&config);
        assert!(stock.contains(&"linux-zen".to_string()));
        assert!(stock.contains(&"linux-zen-headers".to_string()));

        config.packages.install_tkg_kernel = true;
        let tkg = build_package_list(&config);
        assert!(!tkg.contains(&"linux-zen".to_string()), "{tkg:?}");
        assert!(!tkg.contains(&"linux-zen-headers".to_string()), "{tkg:?}");

        // Firmware is kernel-independent and must survive the swap; without it
        // the target boots without microcode or device firmware.
        assert!(tkg.contains(&"linux-firmware".to_string()), "{tkg:?}");

        // basestrap installs no kernel at all here — the tkg packages arrive
        // over HTTP in the phase immediately after, before mkinitcpio and
        // grub-mkconfig run.
        assert!(
            !tkg.iter()
                .any(|p| p.starts_with("linux") && p.contains("-tkg")),
            "the tkg kernel is not a repository package: {tkg:?}"
        );
    }

    // ── the local [deploytix] repo ───────────────────────────────────────

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dtx-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A repo directory holding a database and one package file.
    fn fake_repo(tag: &str) -> PathBuf {
        let dir = scratch_dir(tag);
        std::fs::write(dir.join("deploytix.db.tar.zst"), b"db").unwrap();
        std::fs::write(
            dir.join("deploytix-git-1.5.1-1-x86_64.pkg.tar.zst"),
            b"package",
        )
        .unwrap();
        dir
    }

    /// A target with the pacman.conf basestrap would have left behind.
    fn fake_target(tag: &str) -> PathBuf {
        let root = scratch_dir(tag);
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::fs::write(
            root.join("etc/pacman.conf"),
            "[options]\nHoldPkg = pacman glibc\n\n[system]\nInclude = /etc/pacman.d/mirrorlist\n",
        )
        .unwrap();
        root
    }

    /// The whole point of the copy: a machine deploytix installed must be able
    /// to install the next one, which means leaving it both the packages and a
    /// pacman.conf that names them.
    #[test]
    fn the_installed_system_receives_the_repo_and_a_pacman_conf_entry() {
        let repo = fake_repo("repo");
        let root = fake_target("root");

        install_repo_from(&repo, root.to_str().unwrap()).unwrap();

        let landed = root.join(SHARED_REPO_DIR.trim_start_matches('/'));
        assert!(landed.join("deploytix.db.tar.zst").is_file());
        assert!(landed
            .join("deploytix-git-1.5.1-1-x86_64.pkg.tar.zst")
            .is_file());

        let conf = std::fs::read_to_string(root.join("etc/pacman.conf")).unwrap();
        assert!(conf.contains("[deploytix]"), "{conf}");
        assert!(
            conf.contains(&format!("Server = file://{}", SHARED_REPO_DIR)),
            "{conf}"
        );
        // The config it was added to must survive intact.
        assert!(conf.contains("[system]"), "{conf}");

        std::fs::remove_dir_all(&repo).ok();
        std::fs::remove_dir_all(&root).ok();
    }

    /// Re-running an install over the same target must not stack up repo
    /// sections; pacman treats a duplicated section as a duplicate database.
    #[test]
    fn adding_the_repo_twice_leaves_one_section() {
        let repo = fake_repo("repo-twice");
        let root = fake_target("root-twice");
        let path = root.to_str().unwrap();

        install_repo_from(&repo, path).unwrap();
        install_repo_from(&repo, path).unwrap();

        let conf = std::fs::read_to_string(root.join("etc/pacman.conf")).unwrap();
        assert_eq!(conf.matches("[deploytix]").count(), 1, "{conf}");

        std::fs::remove_dir_all(&repo).ok();
        std::fs::remove_dir_all(&root).ok();
    }

    /// An install that needed no custom packages has no repo to hand over, and
    /// must not leave the target pointing at a `file://` directory that does
    /// not exist — that breaks every later `pacman -Sy` on that machine.
    #[test]
    fn no_repo_means_no_pacman_conf_entry() {
        let empty = scratch_dir("repo-empty");
        let root = fake_target("root-empty");

        install_repo_from(&empty, root.to_str().unwrap()).unwrap();

        let conf = std::fs::read_to_string(root.join("etc/pacman.conf")).unwrap();
        assert!(!conf.contains("[deploytix]"), "{conf}");
        assert!(!root.join(SHARED_REPO_DIR.trim_start_matches('/')).exists());

        std::fs::remove_dir_all(&empty).ok();
        std::fs::remove_dir_all(&root).ok();
    }

    /// The repo has to live on a mount that an immutable root keeps writable,
    /// keeps across a rollback, and rbinds into a snapshot set's chroot.
    /// `/var` is the only one of those; `/tmp` is boot-wiped and `/usr` is
    /// read-only.
    #[test]
    fn the_shared_repo_lives_on_a_mount_immutable_hosts_share() {
        assert!(crate::immutable::SHARED_LIVE_MOUNTS
            .iter()
            .any(|m| SHARED_REPO_DIR.starts_with(&format!("{m}/"))));
        assert!(!crate::immutable::INITRAMFS_OWNED_MOUNTPOINTS
            .iter()
            .any(|m| *m != "/" && SHARED_REPO_DIR.starts_with(&format!("{m}/"))));
    }

    #[test]
    fn embedded_pkgbuilds_cover_the_submodule_packages() {
        // These are the two packages a user cannot otherwise get without
        // cloning the repo and initialising its submodules.
        assert!(embedded_pkgbuild_for("gamescope-git").is_some());
        assert!(embedded_pkgbuild_for("tkg-gui-git").is_some());
    }

    #[test]
    fn deploytix_itself_has_no_embedded_pkgbuild() {
        // pkg/PKGBUILD builds from `$startdir/..` with an empty source=(), so
        // it cannot be materialised standalone. Embedding it would produce a
        // PKGBUILD that fails at build time instead of a clear error.
        assert!(embedded_pkgbuild_for("deploytix-git").is_none());
        assert!(embedded_pkgbuild_for("deploytix-gui-git").is_none());
        assert!(embedded_pkgbuild_for("some-other-pkg").is_none());
    }

    /// The invariant the whole on-demand build rests on: an embedded PKGBUILD
    /// must fetch its own sources, because the installer hands makepkg nothing
    /// but this one file in an otherwise empty directory.
    #[test]
    fn embedded_pkgbuilds_are_self_contained() {
        for (repo, contents) in EMBEDDED_PKGBUILDS {
            let source_line = contents
                .lines()
                .map(str::trim)
                .find(|l| l.starts_with("source="))
                .unwrap_or_else(|| panic!("{repo}: embedded PKGBUILD has no source= line"));
            assert!(
                source_line.contains("git+http"),
                "{repo}: source= must be a remote git URL so makepkg can fetch it \
                 with no repo checkout present, got: {source_line}"
            );
            assert!(
                !source_line.contains("file://"),
                "{repo}: source= points at a local path, which is not reachable \
                 when building from the embedded copy: {source_line}"
            );
        }
    }

    /// Each embedded PKGBUILD must build a package whose filename matches the
    /// prefix the installer scans for, or the build would succeed and then be
    /// reported as still missing.
    #[test]
    fn embedded_pkgbuilds_produce_recognised_package_names() {
        for (repo, contents) in EMBEDDED_PKGBUILDS {
            let pkgname = contents
                .lines()
                .map(str::trim)
                .find_map(|l| l.strip_prefix("pkgname="))
                .unwrap_or_else(|| panic!("{repo}: embedded PKGBUILD has no pkgname="));
            assert!(
                CUSTOM_PACKAGE_NAMES.contains(&pkgname),
                "{repo}: pkgname '{pkgname}' is not in CUSTOM_PACKAGE_NAMES"
            );
            assert!(
                CUSTOM_PKG_PREFIXES.contains(&format!("{pkgname}-").as_str()),
                "{repo}: no CUSTOM_PKG_PREFIXES entry for pkgname '{pkgname}'"
            );
        }
    }

    #[test]
    fn every_embedded_repo_key_maps_back_from_a_package_name() {
        for (repo, _) in EMBEDDED_PKGBUILDS {
            assert!(
                CUSTOM_PACKAGE_NAMES
                    .iter()
                    .any(|p| repo_dir_for_package(p) == *repo),
                "embedded key '{repo}' is unreachable from repo_dir_for_package"
            );
        }
    }

    /// The updater drives `deploytix update`, which cannot work on a mutable
    /// root, so it must never reach one — not as a stale binary, not as a
    /// desktop entry. Gating the package is what enforces that.
    #[test]
    fn the_update_gui_is_installed_only_on_immutable_roots() {
        let mut config = crate::config::DeploymentConfig::sample();

        config.packages.immutable_root = false;
        assert!(
            !build_package_list(&config).contains(&"deploytix-update-gui-git".to_string()),
            "a mutable install must not receive the updater"
        );

        // Both immutable backends must get it: btrfs snapshot sets ...
        config.packages.immutable_root = true;
        config.packages.install_grub_btrfs = true;
        config.disk.use_lvm_thin = false;
        assert!(build_package_list(&config).contains(&"deploytix-update-gui-git".to_string()));

        // ... and LVM A/B dm-verity slots.
        config.packages.install_grub_btrfs = false;
        config.disk.use_lvm_thin = true;
        assert!(build_package_list(&config).contains(&"deploytix-update-gui-git".to_string()));
    }

    /// The installer's own GUI ships on every deployment, so the updater must
    /// be a separate package rather than a file inside it.
    #[test]
    fn the_installer_gui_is_installed_unconditionally() {
        let mut config = crate::config::DeploymentConfig::sample();
        config.packages.immutable_root = false;
        let packages = build_package_list(&config);
        assert!(packages.contains(&"deploytix-gui-git".to_string()));
        assert!(packages.contains(&"deploytix-git".to_string()));
    }

    #[test]
    fn needed_custom_packages_selects_only_what_is_installed() {
        let list: Vec<String> = ["base", "tkg-gui-git", "nano", "gamescope-git"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let needed = needed_custom_packages(&list);
        assert_eq!(needed, vec!["gamescope-git", "tkg-gui-git"]);
        assert!(needed_custom_packages(&["base".to_string()]).is_empty());
    }

    #[test]
    fn find_missing_packages_matches_on_filename_prefix() {
        let found = vec![
            PathBuf::from("/tmp/repo/gamescope-git-r2513.754b539-1-x86_64.pkg.tar.zst"),
            PathBuf::from("/tmp/repo/deploytix-git-1.4.0-1-x86_64.pkg.tar.zst"),
        ];
        let needed = ["gamescope-git", "tkg-gui-git", "deploytix-git"];
        assert_eq!(find_missing_packages(&needed, &found), vec!["tkg-gui-git"]);
    }

    /// `deploytix-gui-git` must not be considered satisfied by a
    /// `deploytix-git-*` file: prefix matching is on `<name>-`, and
    /// "deploytix-git-" is itself a prefix of nothing else.
    #[test]
    fn find_missing_packages_does_not_confuse_sibling_package_names() {
        let found = vec![PathBuf::from(
            "/tmp/repo/deploytix-git-1.4.0-1-x86_64.pkg.tar.zst",
        )];
        let needed = ["deploytix-git", "deploytix-gui-git"];
        assert_eq!(
            find_missing_packages(&needed, &found),
            vec!["deploytix-gui-git"]
        );
    }
}
