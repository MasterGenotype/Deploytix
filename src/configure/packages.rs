//! Optional package collection installers
//!
//! Provides installation functions for:
//! - GPU drivers (NVIDIA, AMD, Intel)
//! - Wine compatibility layer
//! - Gaming packages (Steam, gamescope)
//! - yay AUR helper (built from source)
//! - Btrfs snapshot tools (snapper, btrfs-assistant) via yay
//! - User autostart entries (audio-startup, nm-applet)
//! - Gaming sysctl performance tweaks (/etc/sysctl.d/99-gaming.conf)
//! - Network performance sysctl tweaks (/etc/sysctl.d/99-network-performance.conf)
//! - Handheld Daemon (HHD) via AUR + init-specific service file
//! - Decky Loader (Steam plugin framework) + init-specific service file
//! - evdevhook2 (Cemuhook UDP motion server) via AUR + udev rule + service file

use crate::config::{DeploymentConfig, GpuDriverVendor};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::{info, warn};

// ======================== Signature-error recovery ========================

/// Check whether a pacman stderr message indicates a package signature
/// verification failure (as opposed to a network error, missing target,
/// or conflict).
fn is_signature_error(stderr: &str) -> bool {
    // "signature from … is invalid" — key rotation or stale keyring
    // "signature is unknown trust" — key not in the keyring at all
    // "invalid or corrupted package" — always accompanies sig failures
    // "key … could not be looked up remotely" — missing key
    // "required key missing" — key not imported
    (stderr.contains("is invalid") && stderr.contains("signature from"))
        || stderr.contains("signature is unknown trust")
        || stderr.contains("required key missing")
        || stderr.contains("could not be looked up remotely")
}

/// Chroot-relative path for the relaxed-SigLevel pacman.conf.
/// Must NOT be under /tmp — artix-chroot mounts a fresh tmpfs there
/// on each invocation, so files written from the host side are masked.
const SIG_BYPASS_CONF: &str = "/etc/deploytix-siglevel.conf";

/// Write a temporary pacman.conf inside the chroot that mirrors the
/// real one but sets `SigLevel = Optional TrustAll` so that packages
/// with broken/mismatched signatures can still be installed.
fn write_relaxed_pacman_conf(install_root: &str) -> Result<()> {
    let real_conf_path = format!("{}/etc/pacman.conf", install_root);
    let contents = std::fs::read_to_string(&real_conf_path).map_err(DeploytixError::Io)?;

    // Replace every SigLevel directive with a permissive one, and
    // inject a global override at the top of [options].
    let mut out = String::with_capacity(contents.len() + 128);
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("SigLevel") && !trimmed.starts_with('#') {
            out.push_str("SigLevel = Optional TrustAll\n");
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }

    let dest = format!("{}{}", install_root, SIG_BYPASS_CONF);
    std::fs::write(&dest, &out).map_err(DeploytixError::Io)?;
    Ok(())
}

/// Remove the temporary relaxed pacman.conf from the chroot.
fn remove_relaxed_pacman_conf(install_root: &str) {
    let dest = format!("{}{}", install_root, SIG_BYPASS_CONF);
    let _ = std::fs::remove_file(&dest);
}

/// Rewrite `pacman_cmd` to use `--config <SIG_BYPASS_CONF>`.
///
/// Handles both `pacman -S …` and `pacman -Sy …` forms.
fn inject_config_flag(pacman_cmd: &str) -> String {
    // Insert `--config <path>` right after `pacman`.
    if let Some(rest) = pacman_cmd.strip_prefix("pacman ") {
        format!("pacman --config {} {}", SIG_BYPASS_CONF, rest)
    } else {
        // Shouldn't happen, but be safe.
        pacman_cmd.to_string()
    }
}

/// Run a `pacman -S …` (or similar) command inside the chroot, retrying
/// after a keyring refresh if the first attempt fails with a package
/// signature error.  If the keyring refresh doesn't help (the mirror
/// genuinely serves a mis-signed package), falls back to a final retry
/// with `SigLevel = Optional TrustAll`.
///
/// Recovery sequence on signature failure:
///  1. Clear the pacman package cache so the corrupt / invalid download
///     is not reused on the retry.
///  2. Re-init the GPG keyring.
///  3. Update the `artix-keyring` package (the installed version may
///     predate a key rotation).
///  4. `pacman-key --populate` with the now-updated keyring.
///  5. Retry the original command.
///  6. If still a signature error: retry once more with relaxed
///     SigLevel (last resort for mirror-side signing issues).
///
/// This is the single call-site for every chroot pacman install in the
/// codebase.  Call sites that previously did
/// `cmd.run_in_chroot(root, &install_cmd)?` should use this instead.
pub(crate) fn pacman_install_chroot(
    cmd: &CommandRunner,
    install_root: &str,
    pacman_cmd: &str,
) -> Result<()> {
    match cmd.run_in_chroot(install_root, pacman_cmd) {
        Ok(_) => return Ok(()),
        Err(DeploytixError::CommandFailed { ref stderr, .. }) if is_signature_error(stderr) => {
            warn!(
                "pacman signature verification failed; refreshing keyring and retrying: {}",
                stderr.lines().next().unwrap_or("(no details)")
            );
        }
        Err(e) => return Err(e),
    }

    // --- Keyring refresh retry ---

    // 1. Wipe the package cache so the bad download is not reused.
    let _ = cmd.run_in_chroot(install_root, "pacman -Scc --noconfirm");

    // 2. Re-init the keyring.
    cmd.run_in_chroot(install_root, "pacman-key --init")?;

    // 3. Pull the latest keyring package (best-effort).
    let _ = cmd.run_in_chroot(
        install_root,
        "pacman -Sy --noconfirm artix-keyring",
    );

    // 4. Populate with updated keys.
    cmd.run_in_chroot(install_root, "pacman-key --populate artix")?;
    // If the Arch keyring is installed, refresh that too.
    let _ = cmd.run_in_chroot(install_root, "pacman-key --populate archlinux");

    // 5. Retry with refreshed keyring.
    match cmd.run_in_chroot(install_root, pacman_cmd) {
        Ok(_) => return Ok(()),
        Err(DeploytixError::CommandFailed { ref stderr, .. }) if is_signature_error(stderr) => {
            warn!(
                "Signature error persists after keyring refresh; \
                 retrying with relaxed SigLevel as last resort: {}",
                stderr.lines().next().unwrap_or("(no details)")
            );
        }
        Err(e) => return Err(e),
    }

    // --- Last-resort: relaxed SigLevel ---

    // 6. Clear cache again (the re-download above cached the same bad
    //    package), write a permissive pacman.conf, retry, clean up.
    let _ = cmd.run_in_chroot(install_root, "pacman -Scc --noconfirm");
    write_relaxed_pacman_conf(install_root)?;

    let relaxed_cmd = inject_config_flag(pacman_cmd);
    let result = cmd.run_in_chroot(install_root, &relaxed_cmd);

    remove_relaxed_pacman_conf(install_root);

    result?;
    Ok(())
}

// ======================== GPU Driver Packages ========================

const NVIDIA_PACKAGES: &[&str] = &["nvidia", "nvidia-utils", "linux-firmware-nvidia"];

const AMD_PACKAGES: &[&str] = &[
    "linux-firmware-amdgpu",
    "mesa",
    "vulkan-headers",
    "vulkan-icd-loader",
    "vulkan-mesa-implicit-layers",
    "vulkan-mesa-layers",
    "vulkan-radeon",
    "vulkan-tools",
    "vulkan-validation-layers",
    "vulkan-utility-libraries",
    "xf86-video-amdgpu",
];

const INTEL_PACKAGES: &[&str] = &[
    "linux-firmware-intel",
    "vulkan-intel",
    "mesa",
    "intel-media-driver",
    "xf86-video-intel",
];

/// Install selected GPU driver packages via pacman in chroot.
pub fn install_gpu_drivers(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if config.packages.gpu_drivers.is_empty() {
        return Ok(());
    }

    let mut packages: Vec<&str> = Vec::new();

    for vendor in &config.packages.gpu_drivers {
        match vendor {
            GpuDriverVendor::Nvidia => {
                info!("Adding NVIDIA GPU driver packages");
                packages.extend(NVIDIA_PACKAGES);
            }
            GpuDriverVendor::Amd => {
                info!("Adding AMD GPU driver packages");
                packages.extend(AMD_PACKAGES);
            }
            GpuDriverVendor::Intel => {
                info!("Adding Intel GPU driver packages");
                packages.extend(INTEL_PACKAGES);
            }
        }
    }

    // Deduplicate (e.g. mesa appears in both AMD and Intel)
    packages.sort();
    packages.dedup();

    if packages.is_empty() {
        return Ok(());
    }

    info!("Installing GPU driver packages: {}", packages.join(", "));

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install GPU driver packages: {:?}",
            packages
        );
        return Ok(());
    }

    let pkg_list = packages.join(" ");
    let pkg_strings: Vec<String> = packages.iter().map(|s| (*s).to_string()).collect();
    let _ =
        crate::pkgdeps::preflight::preflight_chroot(install_root, &pkg_strings, cmd.is_dry_run());
    let install_cmd = format!("pacman -S --noconfirm --needed {}", pkg_list);
    pacman_install_chroot(cmd, install_root, &install_cmd)?;

    info!("GPU driver installation complete");
    Ok(())
}

// ======================== Wine Packages ========================

/// Wine packages available in Artix repos.
const WINE_PACKAGES_ARTIX: &[&str] = &["wine", "vkd3d", "winetricks"];

/// Wine packages that live in the Arch Linux [extra] repository.
const WINE_PACKAGES_ARCH_EXTRA: &[&str] = &["wine-mono", "wine-gecko"];

/// Ensure the Arch Linux `[extra]` repository is configured inside the
/// chroot so that packages like `wine-mono` and `wine-gecko` (which are
/// not mirrored in Artix repos) can be installed.
///
/// Installs `artix-archlinux-support` (available from Artix repos),
/// populates the Arch keyring, appends `[extra]` to the chroot's
/// pacman.conf, and refreshes the package database.
fn ensure_arch_repos_in_chroot(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    // Install artix-archlinux-support which provides the Arch mirrorlist
    // and keyring.  This package is in Artix's own repos.
    info!("Installing artix-archlinux-support in chroot");
    let _ = crate::pkgdeps::preflight::preflight_chroot(
        install_root,
        &["artix-archlinux-support".to_string()],
        cmd.is_dry_run(),
    );
    pacman_install_chroot(
        cmd,
        install_root,
        "pacman -S --noconfirm --needed artix-archlinux-support",
    )?;

    // Trust the Arch Linux package signing keys.
    info!("Populating Arch Linux keyring in chroot");
    cmd.run_in_chroot(install_root, "pacman-key --populate archlinux")?;

    // Append [extra] to the chroot's pacman.conf if not already present.
    let chroot_pacman_conf = format!("{}/etc/pacman.conf", install_root);
    let conf_content = std::fs::read_to_string(&chroot_pacman_conf)
        .map_err(crate::utils::error::DeploytixError::Io)?;

    if !conf_content.lines().any(|line| line.trim() == "[extra]") {
        info!("Adding Arch [extra] repository to chroot pacman.conf");

        // artix-archlinux-support installs the mirrorlist at this path
        // inside the chroot.
        let mirrorlist = format!("{}/etc/pacman.d/mirrorlist-arch", install_root);
        let mirror_entry = if std::path::Path::new(&mirrorlist).exists() {
            "Include = /etc/pacman.d/mirrorlist-arch".to_string()
        } else {
            "Server = https://geo.mirror.pkgbuild.com/$repo/os/$arch".to_string()
        };

        let extra_section = format!(
            "\n\n# Arch Linux [extra] repository (auto-added by deploytix installer)\n\
             [extra]\n\
             SigLevel = PackageRequired\n\
             {}\n",
            mirror_entry,
        );

        let updated = format!("{}{}", conf_content.trim_end(), extra_section);
        std::fs::write(&chroot_pacman_conf, &updated)
            .map_err(crate::utils::error::DeploytixError::Io)?;
    }

    // Refresh package databases so the new repo is usable.
    cmd.run_in_chroot(install_root, "pacman -Sy --noconfirm")?;

    Ok(())
}

/// Install Wine compatibility packages via pacman in chroot.
///
/// `wine-mono` and `wine-gecko` live in the Arch Linux `[extra]`
/// repository, which is not enabled by default on Artix.  This function
/// ensures the repo is configured in the chroot (via
/// `artix-archlinux-support`) before installing the full package set.
pub fn install_wine_packages(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_wine {
        return Ok(());
    }

    info!("Installing Wine compatibility packages");

    if cmd.is_dry_run() {
        let all_pkgs: Vec<&str> = WINE_PACKAGES_ARTIX
            .iter()
            .chain(WINE_PACKAGES_ARCH_EXTRA.iter())
            .copied()
            .collect();
        println!("  [dry-run] Would install Wine packages: {:?}", all_pkgs);
        return Ok(());
    }

    // Enable the Arch [extra] repo in the chroot for wine-mono/wine-gecko.
    ensure_arch_repos_in_chroot(cmd, install_root)?;

    let all_pkgs: Vec<&str> = WINE_PACKAGES_ARTIX
        .iter()
        .chain(WINE_PACKAGES_ARCH_EXTRA.iter())
        .copied()
        .collect();
    let pkg_list = all_pkgs.join(" ");
    let pkg_strings: Vec<String> = all_pkgs.iter().map(|s| (*s).to_string()).collect();
    let _ =
        crate::pkgdeps::preflight::preflight_chroot(install_root, &pkg_strings, cmd.is_dry_run());
    let install_cmd = format!("pacman -S --noconfirm --needed {}", pkg_list);
    pacman_install_chroot(cmd, install_root, &install_cmd)?;

    info!("Wine installation complete");
    Ok(())
}

// ======================== Gaming Packages ========================

/// Packages installed via pacman for the gaming path.
///
/// `gamescope-git` (Bazzite fork) is installed during basestrap from the
/// custom [deploytix] repository — its runtime deps are declared in the
/// PKGBUILD and pulled in automatically by pacman, so they are not listed
/// here.  Steam is installed in the chroot phase because it requires the
/// [lib32] repo which is enabled here.
const GAMING_PACKAGES: &[&str] = &["steam"];

/// Enable the [lib32] repository in the chroot's pacman.conf.
///
/// Steam and its 32-bit Vulkan driver dependencies live in `lib32`,
/// which is commented-out by default.  This uncomments the section
/// header **and** its `Include` line, then refreshes the database.
fn enable_lib32_repo(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Enabling [lib32] repository in chroot pacman.conf");

    // Uncomment "#[lib32]" and the following "#Include = ..." line.
    // sed processes the file in-place; the two-line address form handles
    // both lines regardless of surrounding whitespace.
    cmd.run_in_chroot(
        install_root,
        "sed -i '/^#\\[lib32\\]/,/^#Include/ s/^#//' /etc/pacman.conf",
    )?;

    // Sync the newly-enabled repository
    cmd.run_in_chroot(install_root, "pacman -Sy --noconfirm")?;

    Ok(())
}

/// Return the lib32 Vulkan driver packages that match the selected GPU vendors.
///
/// Naming convention:
/// - NVIDIA  → `lib32-nvidia-utils`
/// - AMD     → `lib32-vulkan-radeon`
/// - Intel   → `lib32-vulkan-intel`
fn lib32_vulkan_packages(config: &DeploymentConfig) -> Vec<&'static str> {
    let mut pkgs = Vec::new();
    for vendor in &config.packages.gpu_drivers {
        match vendor {
            GpuDriverVendor::Nvidia => pkgs.push("lib32-nvidia-utils"),
            GpuDriverVendor::Amd => pkgs.push("lib32-vulkan-radeon"),
            GpuDriverVendor::Intel => pkgs.push("lib32-vulkan-intel"),
        }
    }
    pkgs.sort();
    pkgs.dedup();
    pkgs
}

/// Install gaming packages via pacman in chroot.
///
/// 1. Enables the `[lib32]` repository (required for Steam's 32-bit deps).
/// 2. Installs the appropriate `lib32-*` Vulkan driver for every selected GPU.
/// 3. Installs Steam (gamescope-git is already installed during basestrap
///    from the custom [deploytix] repository).
pub fn install_gaming_packages(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_gaming {
        return Ok(());
    }

    let lib32_vulkan = lib32_vulkan_packages(config);

    info!("Installing gaming packages");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would enable [lib32] repository");
        println!(
            "  [dry-run] Would install lib32 Vulkan drivers: {:?}",
            lib32_vulkan
        );
        println!(
            "  [dry-run] Would install gaming packages: {:?}",
            GAMING_PACKAGES
        );
        return Ok(());
    }

    // Step 1: Enable [lib32] repo so 32-bit packages are available
    enable_lib32_repo(cmd, install_root)?;

    // Step 2: Install lib32 Vulkan driver(s) for selected GPU vendor(s)
    if !lib32_vulkan.is_empty() {
        let vulkan_list = lib32_vulkan.join(" ");
        info!("Installing lib32 Vulkan drivers: {}", vulkan_list);
        let vulkan_strings: Vec<String> = lib32_vulkan.iter().map(|s| (*s).to_string()).collect();
        let _ = crate::pkgdeps::preflight::preflight_chroot(
            install_root,
            &vulkan_strings,
            cmd.is_dry_run(),
        );
        let vulkan_cmd = format!("pacman -S --noconfirm --needed {}", vulkan_list);
        pacman_install_chroot(cmd, install_root, &vulkan_cmd)?;
    }

    // Step 3: Install Steam
    let pkg_list = GAMING_PACKAGES.join(" ");
    let pkg_strings: Vec<String> = GAMING_PACKAGES.iter().map(|s| (*s).to_string()).collect();
    let _ =
        crate::pkgdeps::preflight::preflight_chroot(install_root, &pkg_strings, cmd.is_dry_run());
    let install_cmd = format!("pacman -S --noconfirm --needed {}", pkg_list);
    pacman_install_chroot(cmd, install_root, &install_cmd)?;

    info!("Gaming package installation complete");
    Ok(())
}

// ======================== yay AUR Helper ========================

/// Install yay AUR helper from source in chroot.
///
/// Requires `go`, `git`, and `base-devel` (go is added to basestrap when
/// `install_yay` is enabled).  Builds as the configured user (not root)
/// since `makepkg` refuses to run as root.
pub fn install_yay(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_yay {
        return Ok(());
    }

    let username = &config.user.name;
    info!(
        "Installing yay AUR helper (building from source as {})",
        username
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install go and build yay from source as {}",
            username
        );
        return Ok(());
    }

    // Ensure build dependencies are present
    let yay_build_deps = vec![
        "go".to_string(),
        "git".to_string(),
        "base-devel".to_string(),
    ];
    let _ = crate::pkgdeps::preflight::preflight_chroot(
        install_root,
        &yay_build_deps,
        cmd.is_dry_run(),
    );
    pacman_install_chroot(
        cmd,
        install_root,
        "pacman -S --noconfirm --needed go git base-devel",
    )?;

    // Create build dir, clone, build, and clean up in a single chroot
    // invocation.  artix-chroot may mount a tmpfs over /tmp, so a
    // directory created in one invocation would not survive to the next.
    let build_cmd = format!(
        "mkdir -p /tmp/yay-build && \
         chown {0}:{0} /tmp/yay-build && \
         sudo -u {0} bash -c '\
           cd /tmp/yay-build && \
           git clone https://aur.archlinux.org/yay.git && \
           cd yay && \
           makepkg -si --noconfirm' && \
         rm -rf /tmp/yay-build",
        username
    );
    cmd.run_in_chroot(install_root, &build_cmd)?;

    info!("yay AUR helper installed successfully");
    Ok(())
}

// ======================== AUR Packages (via yay) ========================

/// AUR packages to install via yay when the AUR helper is available.
const YAY_AUR_PACKAGES: &[&str] = &["zen-browser-bin"];

/// Install additional AUR packages via yay in chroot.
///
/// Runs unconditionally when yay is installed.  These are AUR packages
/// that are not available in the official Artix or Arch repositories.
pub fn install_aur_packages(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_yay {
        return Ok(());
    }

    let username = &config.user.name;
    info!(
        "Installing AUR packages via yay as {}: {}",
        username,
        YAY_AUR_PACKAGES.join(", ")
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install AUR packages via yay as {}: {:?}",
            username, YAY_AUR_PACKAGES
        );
        return Ok(());
    }

    let pkg_list = YAY_AUR_PACKAGES.join(" ");
    let install_cmd = format!(
        "sudo -u {} yay -S --noconfirm --needed {}",
        username, pkg_list
    );
    cmd.run_in_chroot(install_root, &install_cmd)?;

    info!("AUR packages installed successfully");
    Ok(())
}

// ======================== Btrfs Snapshot Tools ========================

/// Btrfs snapshot tool packages to install via yay.
const BTRFS_TOOL_PACKAGES: &[&str] = &["snapper", "btrfs-assistant"];

/// Install btrfs snapshot tools (snapper, btrfs-assistant) via yay in chroot.
///
/// Requires yay to already be installed and btrfs as the filesystem.
pub fn install_btrfs_tools(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_btrfs_tools {
        return Ok(());
    }

    let username = &config.user.name;
    info!(
        "Installing btrfs snapshot tools via yay as {}: {}",
        username,
        BTRFS_TOOL_PACKAGES.join(", ")
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install btrfs tools via yay as {}: {:?}",
            username, BTRFS_TOOL_PACKAGES
        );
        return Ok(());
    }

    let pkg_list = BTRFS_TOOL_PACKAGES.join(" ");
    let install_cmd = format!(
        "sudo -u {} yay -S --noconfirm --needed {}",
        username, pkg_list
    );
    cmd.run_in_chroot(install_root, &install_cmd)?;

    info!("Btrfs snapshot tools installed successfully");
    Ok(())
}

// ======================== Autostart Entries ========================

/// Embedded audio-startup script (compiled into binary).
const AUDIO_STARTUP_SCRIPT: &str = include_str!("../resources/autostart/audio-startup.sh");

/// Deploy user autostart entries to the target system.
///
/// Installs unconditionally:
/// - `~/.local/bin/audio-startup` — PipeWire audio startup script
/// - `~/.config/autostart/audio-startup.desktop` — autostart entry for the above
/// - `~/.config/autostart/nm-applet.desktop` — autostart entry for nm-applet
pub fn install_autostart_entries(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let username = &config.user.name;
    let home = format!("{}/home/{}", install_root, username);

    info!("Installing autostart entries for user {}", username);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install audio-startup to /home/{}/.local/bin/",
            username
        );
        println!(
            "  [dry-run] Would install autostart .desktop entries to /home/{}/.config/autostart/",
            username
        );
        return Ok(());
    }

    // Create directories
    let bin_dir = format!("{}/.local/bin", home);
    let autostart_dir = format!("{}/.config/autostart", home);
    fs::create_dir_all(&bin_dir)?;
    fs::create_dir_all(&autostart_dir)?;

    // Deploy audio-startup script
    let script_path = format!("{}/audio-startup", bin_dir);
    fs::write(&script_path, AUDIO_STARTUP_SCRIPT)?;
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))?;
    info!("  Installed ~/.local/bin/audio-startup");

    // Deploy audio-startup.desktop
    let audio_desktop = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Audio Startup\n\
         Exec=/home/{}/.local/bin/audio-startup\n\
         Hidden=false\n\
         NoDisplay=false\n\
         X-GNOME-Autostart-enabled=true\n\
         Comment=Start PipeWire audio services\n",
        username
    );
    let audio_desktop_path = format!("{}/audio-startup.desktop", autostart_dir);
    fs::write(&audio_desktop_path, &audio_desktop)?;
    fs::set_permissions(&audio_desktop_path, fs::Permissions::from_mode(0o644))?;
    info!("  Installed ~/.config/autostart/audio-startup.desktop");

    // Deploy nm-applet.desktop only when NetworkManager is the chosen backend
    if config.network.backend == crate::config::NetworkBackend::NetworkManager {
        let nm_desktop = "[Desktop Entry]\n\
             Type=Application\n\
             Name=Network Manager Applet\n\
             Exec=/bin/nm-applet\n\
             Hidden=false\n\
             NoDisplay=false\n\
             X-GNOME-Autostart-enabled=true\n\
             Comment=NetworkManager system tray applet\n";
        let nm_desktop_path = format!("{}/nm-applet.desktop", autostart_dir);
        fs::write(&nm_desktop_path, nm_desktop)?;
        fs::set_permissions(&nm_desktop_path, fs::Permissions::from_mode(0o644))?;
        info!("  Installed ~/.config/autostart/nm-applet.desktop");
    }

    // Fix ownership: all deployed files should belong to the user, not root
    let chown_cmd = format!(
        "chown -R {0}:{0} /home/{0}/.local /home/{0}/.config",
        username
    );
    cmd.run_in_chroot(install_root, &chown_cmd)?;

    info!("Autostart entries installed successfully");
    Ok(())
}

// ======================== Gaming sysctl Tweaks ========================

/// Sysctl configuration content for gaming/handheld performance.
const GAMING_SYSCTL_CONF: &str = "\
# Gaming performance tweaks — written by Deploytix
#
# vm.max_map_count: critical for Windows games via Proton/WINE.
# Matches the Steam Deck default (MAX_INT - 5).
vm.max_map_count = 2147483642

# Reduce kernel swap-out aggressiveness for interactive/gaming workloads.
vm.swappiness = 10

# Improve CPU scheduling responsiveness for desktop and gaming tasks.
kernel.sched_autogroup_enabled = 1

# Enable TCP Fast Open (client + server) for improved network latency.
net.ipv4.tcp_fastopen = 3

# Raise the maximum number of open file descriptors.
fs.file-max = 524288
";

/// Write `/etc/sysctl.d/99-gaming.conf` to the target system with
/// gaming/handheld performance tuning parameters.
pub fn install_sysctl_gaming(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.sysctl_gaming_tweaks {
        return Ok(());
    }

    info!("Writing gaming sysctl configuration");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would write /etc/sysctl.d/99-gaming.conf");
        println!("    vm.max_map_count = 2147483642");
        println!("    vm.swappiness    = 10");
        return Ok(());
    }

    let sysctl_dir = format!("{}/etc/sysctl.d", install_root);
    fs::create_dir_all(&sysctl_dir)?;

    let conf_path = format!("{}/99-gaming.conf", sysctl_dir);
    fs::write(&conf_path, GAMING_SYSCTL_CONF)?;
    fs::set_permissions(&conf_path, fs::Permissions::from_mode(0o644))?;

    info!("  Written /etc/sysctl.d/99-gaming.conf");
    Ok(())
}

// ======================== Network Performance sysctl Tweaks ========================

/// Sysctl configuration content for network performance.
///
/// Tuned for modern consumer hardware (Wi-Fi 6/6E or 1 GbE+ ethernet) on a
/// desktop/gaming workload.  Values intentionally do **not** overlap with
/// `GAMING_SYSCTL_CONF` so the two files coexist in `/etc/sysctl.d/`
/// without clobbering each other.  Ordering is determined by alphabetical
/// filename, so `99-network-performance.conf` loads after
/// `99-gaming.conf`.
const NETWORK_PERFORMANCE_SYSCTL_CONF: &str = "\
# Network performance tweaks \u{2014} written by Deploytix
# Complements /etc/sysctl.d/99-gaming.conf (no key overlap).

# --- Congestion control & queueing ---------------------------------------
# BBR + fq: pacing-aware qdisc recommended for BBR.  Improves throughput
# and latency under bufferbloat (typical of consumer Wi-Fi / ISPs).
net.core.default_qdisc = fq
net.ipv4.tcp_congestion_control = bbr

# --- Socket buffer ceilings ---------------------------------------------
# 16 MiB ceiling covers ~1.5 Gbps * 80 ms BDP, enough for Wi-Fi 6 +
# transcontinental links.
net.core.rmem_max = 16777216
net.core.wmem_max = 16777216
net.core.rmem_default = 1048576
net.core.wmem_default = 1048576
net.core.optmem_max = 65536

# TCP autotuning ranges: min / default / max bytes.
net.ipv4.tcp_rmem = 4096 1048576 16777216
net.ipv4.tcp_wmem = 4096 1048576 16777216

# UDP memory pressure thresholds (bytes per socket).
net.ipv4.udp_rmem_min = 16384
net.ipv4.udp_wmem_min = 16384

# --- Backlogs / queues ---------------------------------------------------
net.core.netdev_max_backlog = 5000
net.core.netdev_budget = 600
net.core.netdev_budget_usecs = 8000

net.core.somaxconn = 4096
net.ipv4.tcp_max_syn_backlog = 8192

# --- TCP behaviour -------------------------------------------------------
# Helps on links with broken PMTUD (Wi-Fi / VPN).
net.ipv4.tcp_mtu_probing = 1

# Cap unsent bytes in the socket buffer so BBR can pace tightly.
net.ipv4.tcp_notsent_lowat = 131072

# Recycle TIME_WAIT faster (safe on clients; fine on single-NAT hosts).
net.ipv4.tcp_fin_timeout = 15
net.ipv4.tcp_tw_reuse = 1

# Keepalive tuned for long-lived sessions on flaky Wi-Fi.
net.ipv4.tcp_keepalive_time = 300
net.ipv4.tcp_keepalive_intvl = 30
net.ipv4.tcp_keepalive_probes = 5

# ECN (negotiated, not forced).
net.ipv4.tcp_ecn = 1

# Don't restart congestion window after idle periods.
net.ipv4.tcp_slow_start_after_idle = 0

# SACK + F-RTO are on by default; pinned for clarity.
net.ipv4.tcp_sack = 1
net.ipv4.tcp_frto = 2

# --- Security / hygiene --------------------------------------------------
net.ipv4.tcp_syncookies = 1
net.ipv4.conf.all.rp_filter = 1
net.ipv4.conf.default.rp_filter = 1
net.ipv4.icmp_echo_ignore_broadcasts = 1
net.ipv4.conf.all.accept_redirects = 0
net.ipv4.conf.default.accept_redirects = 0
net.ipv6.conf.all.accept_redirects = 0
net.ipv6.conf.default.accept_redirects = 0
";

/// Write `/etc/sysctl.d/99-network-performance.conf` to the target system
/// with network performance tuning parameters.
///
/// Safe to enable alongside `install_sysctl_gaming`; the two files do not
/// share any keys.  The kernel must have `tcp_bbr` available (built-in on
/// modern stock Artix kernels).
pub fn install_sysctl_network_performance(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.sysctl_network_performance {
        return Ok(());
    }

    info!("Writing network performance sysctl configuration");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would write /etc/sysctl.d/99-network-performance.conf");
        println!("    net.ipv4.tcp_congestion_control = bbr");
        println!("    net.core.default_qdisc          = fq");
        println!("    net.core.rmem_max               = 16777216");
        return Ok(());
    }

    let sysctl_dir = format!("{}/etc/sysctl.d", install_root);
    fs::create_dir_all(&sysctl_dir)?;

    let conf_path = format!("{}/99-network-performance.conf", sysctl_dir);
    fs::write(&conf_path, NETWORK_PERFORMANCE_SYSCTL_CONF)?;
    fs::set_permissions(&conf_path, fs::Permissions::from_mode(0o644))?;

    info!("  Written /etc/sysctl.d/99-network-performance.conf");
    Ok(())
}

// ======================== Handheld Daemon (HHD) ========================

/// AUR packages installed for HHD.
///
/// We use `hhd-git` (a split PKGBUILD that depends on `hhd-license-git`)
/// instead of the tagged `hhd` release or `adjustor` — `adjustor` is now
/// bundled into `hhd` itself (`replaces=(adjustor)` in the upstream
/// PKGBUILD).  `hhd-systemd-git` is intentionally excluded (we generate
/// init-specific service files instead), and `hhd-ui` is excluded because
/// it spawns a browser overlay that races with the gamescope/Steam session
/// launch and causes a black screen on the reference handheld device.
const HHD_AUR_PACKAGES: &[&str] = &["hhd-git"];

/// Install Handheld Daemon (HHD) via yay and write an init-specific service
/// file so that HHD starts automatically on boot.
///
/// Upstream only ships a systemd service; we generate the appropriate file
/// for whichever init system the user has chosen.
///
/// Requires `install_yay = true` — the caller (`installer.rs`) checks this.
pub fn install_hhd(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_hhd {
        return Ok(());
    }

    let username = &config.user.name;

    info!("Installing Handheld Daemon (HHD) for user {}", username);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install AUR packages via yay as {}: {}",
            username,
            HHD_AUR_PACKAGES.join(" ")
        );
        println!("  [dry-run] Would write /etc/modules-load.d/hhd.conf (uhid)");
        println!(
            "  [dry-run] Would write HHD service file for init: {}",
            config.system.init
        );
        return Ok(());
    }

    // Step 1: Install AUR packages via yay
    let install_cmd = format!(
        "sudo -u {} yay -S --noconfirm --needed {}",
        username,
        HHD_AUR_PACKAGES.join(" ")
    );
    cmd.run_in_chroot(install_root, &install_cmd)?;
    info!("  HHD AUR packages installed");

    // Step 2: Ensure the uhid kernel module is loaded at boot.
    // uhid provides a user-space HID interface used by HHD to emulate
    // controllers; without it HHD gets permission errors on startup.
    let modules_dir = format!("{}/etc/modules-load.d", install_root);
    fs::create_dir_all(&modules_dir)?;
    let modules_conf = format!("{}/hhd.conf", modules_dir);
    fs::write(
        &modules_conf,
        "# Load uhid on startup — required by Handheld Daemon (HHD)\nuhid\n",
    )?;
    fs::set_permissions(&modules_conf, fs::Permissions::from_mode(0o644))?;
    info!("  Written /etc/modules-load.d/hhd.conf");

    // Step 3: Write init-specific service file
    write_hhd_service(config, install_root, username)?;

    info!("HHD installation complete");
    Ok(())
}

/// Write the HHD service file for the configured init system.
///
/// HHD needs to run **as root** — it writes to sysfs, /dev/uinput, ACPI
/// interfaces, and fan/TDP controls.  The `--user <name>` flag tells HHD
/// whose config directory to read; it does not drop privileges.  This
/// matches the upstream `hhd@.service` systemd unit, which has no `User=`
/// directive.
fn write_hhd_service(config: &DeploymentConfig, install_root: &str, username: &str) -> Result<()> {
    use crate::config::InitSystem;

    match config.system.init {
        InitSystem::Runit => {
            let sv_dir = format!("{}/etc/runit/sv/hhd", install_root);
            fs::create_dir_all(&sv_dir)?;

            let run_script = format!(
                "#!/bin/sh\n\
                 exec 2>&1\n\
                 exec /usr/bin/hhd --user {user}\n",
                user = username
            );
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, &run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            // log/run — pipe to svlogd
            let log_dir = format!("{}/log", sv_dir);
            fs::create_dir_all(&log_dir)?;
            let log_run = "#!/bin/sh\n\
                           [ -d /var/log/hhd ] || install -dm 755 /var/log/hhd\n\
                           exec svlogd -tt /var/log/hhd\n";
            let log_run_path = format!("{}/run", log_dir);
            fs::write(&log_run_path, log_run)?;
            fs::set_permissions(&log_run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written runit service: /etc/runit/sv/hhd/");
        }

        InitSystem::OpenRC => {
            let init_d = format!("{}/etc/init.d", install_root);
            fs::create_dir_all(&init_d)?;

            // Note: no `command_user` — HHD must run as root.
            let script = format!(
                "#!/sbin/openrc-run\n\
                 description=\"Handheld Daemon Service\"\n\
                 command=\"/usr/bin/hhd\"\n\
                 command_args=\"--user {user}\"\n\
                 command_background=true\n\
                 pidfile=\"/var/run/hhd.pid\"\n\
                 \n\
                 depend() {{\n\
                 \tneed udev\n\
                 \tafter seatd\n\
                 }}\n",
                user = username
            );
            let script_path = format!("{}/hhd", init_d);
            fs::write(&script_path, &script)?;
            fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written OpenRC service: /etc/init.d/hhd");
        }

        InitSystem::S6 => {
            let sv_dir = format!("{}/etc/s6/sv/hhd", install_root);
            fs::create_dir_all(&sv_dir)?;

            // type file declares this a long-running service
            fs::write(format!("{}/type", sv_dir), "longrun\n")?;

            // Run as root (no s6-setuidgid wrapping).
            let run_script = format!(
                "#!/bin/sh\n\
                 exec /usr/bin/hhd --user {user} 2>&1\n",
                user = username
            );
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, &run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written s6 service: /etc/s6/sv/hhd/");
        }

        InitSystem::Dinit => {
            let dinit_d = format!("{}/etc/dinit.d", install_root);
            fs::create_dir_all(&dinit_d)?;

            // No `run-as` — HHD must run as root.
            let service = format!(
                "type = process\n\
                 command = /usr/bin/hhd --user {user}\n\
                 restart = true\n",
                user = username
            );
            let service_path = format!("{}/hhd", dinit_d);
            fs::write(&service_path, &service)?;
            fs::set_permissions(&service_path, fs::Permissions::from_mode(0o644))?;

            info!("  Written dinit service: /etc/dinit.d/hhd");
        }
    }

    Ok(())
}

// ======================== Decky Loader ========================

/// Install Decky Loader — the Steam plugin framework — from the
/// `decky-loader-bin` AUR package, then bootstrap the user's data
/// directory with `decky-loader-helper` and write an init-specific
/// service file.
///
/// Layout created on the target system (mirrors the upstream systemd
/// unit shipped by `decky-loader-bin`):
/// ```
/// /usr/lib/decky-loader/PluginLoader               (AUR package file)
/// /usr/bin/decky-loader-helper                     (AUR package file)
/// /home/{user}/.local/var/opt/decky-loader/
///   services/
///     PluginLoader          (copy installed by decky-loader-helper)
///     .loader.version       (version tag written by the helper)
///   plugins/
/// ~/.steam/steam/.cef-enable-remote-debugging
/// ```
///
/// The init service runs `PluginLoader` **as the greetd session user**
/// (not root) per the reference handheld configuration — this means Decky
/// can read the user's Steam data but is sandboxed out of privileged
/// system operations.  Plugins that need root use `decky-loader` helper
/// binaries separately.
///
/// Requires `install_gaming = true` (Steam must be present) and
/// `install_yay = true` (we install via yay).  The caller
/// (`installer.rs`) checks both.
pub fn install_decky_loader(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_decky_loader {
        return Ok(());
    }

    let username = &config.user.name;
    let decky_data = format!("/home/{}/.local/var/opt/decky-loader", username);

    info!("Installing Decky Loader for user {}", username);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install decky-loader-bin via yay as {}",
            username
        );
        println!(
            "  [dry-run] Would bootstrap {} via /usr/bin/decky-loader-helper",
            decky_data
        );
        println!(
            "  [dry-run] Would write Decky Loader service file for init: {}",
            config.system.init
        );
        return Ok(());
    }

    // Step 1: Install decky-loader-bin via yay.  This drops the binary at
    // /usr/lib/decky-loader/PluginLoader and the helper at
    // /usr/bin/decky-loader-helper.
    let install_cmd = format!(
        "sudo -u {} yay -S --noconfirm --needed decky-loader-bin",
        username
    );
    cmd.run_in_chroot(install_root, &install_cmd)?;
    info!("  decky-loader-bin installed");

    // Step 2: Enable Steam CEF remote debugging (required by Decky's frontend).
    // The helper below also does this, but Flatpak Steam installs need it
    // at a separate path that the helper doesn't know about.
    let steam_dir = format!("{}/home/{}/.steam/steam", install_root, username);
    fs::create_dir_all(&steam_dir)?;
    fs::write(format!("{steam_dir}/.cef-enable-remote-debugging"), "")?;

    let flatpak_steam = format!(
        "{}/home/{}/.var/app/com.valvesoftware.Steam/data/Steam",
        install_root, username
    );
    if std::path::Path::new(&flatpak_steam).exists() {
        fs::write(format!("{flatpak_steam}/.cef-enable-remote-debugging"), "")?;
    }
    info!("  Enabled Steam CEF remote debugging");

    // Step 3: Bootstrap the user's Decky data directory with the helper
    // shipped by decky-loader-bin.  The helper reads the installed version
    // from pacman, creates {services,plugins}/ owned by the user, copies
    // /usr/lib/decky-loader/PluginLoader into services/, and records the
    // version tag in .loader.version.
    let helper_cmd = format!(
        "DECKY_VER=$(pacman -Q decky-loader-bin | awk '{{print $2}}' | sed 's/-[0-9]*$//'); \
         /usr/bin/decky-loader-helper \"v${{DECKY_VER}}\" {user}",
        user = username
    );
    cmd.run_in_chroot(install_root, &helper_cmd)?;
    info!("  Bootstrapped Decky data directory at {}", decky_data);

    // Step 4: Write init-specific service file
    write_decky_service(config, install_root, username, &decky_data)?;

    // Step 5: Ensure ownership under the user's home stays correct
    // (helper already sets ownership on the decky data dir, but .steam
    // was created by us as root above).
    let chown_cmd = format!(
        "chown -R {user}:{user} /home/{user}/.local /home/{user}/.steam",
        user = username
    );
    cmd.run_in_chroot(install_root, &chown_cmd)?;

    info!("Decky Loader installation complete");
    Ok(())
}

/// Write the `plugin_loader` service file for the configured init system.
///
/// Decky runs as the greetd session user with HOMEBREW_FOLDER pointing at
/// `~/.local/var/opt/decky-loader`.  UNPRIVILEGED_PATH / PRIVILEGED_PATH
/// are historical aliases consumed by older PluginLoader builds; we set
/// them to the same path for compatibility.
fn write_decky_service(
    config: &DeploymentConfig,
    install_root: &str,
    username: &str,
    decky_data: &str,
) -> Result<()> {
    use crate::config::InitSystem;

    let plugin_loader = format!("{}/services/PluginLoader", decky_data);
    let working_dir = format!("{}/services", decky_data);

    match config.system.init {
        InitSystem::Runit => {
            let sv_dir = format!("{}/etc/runit/sv/plugin_loader", install_root);
            fs::create_dir_all(&sv_dir)?;

            // chpst -u user:user drops to the session user and its primary
            // group before exec'ing PluginLoader.  Environment is exported
            // inline so it survives the chpst exec chain.
            let run_script = format!(
                "#!/bin/sh\n\
                 exec 2>&1\n\
                 export HOMEBREW_FOLDER={data}\n\
                 export UNPRIVILEGED_PATH={data}\n\
                 export PRIVILEGED_PATH={data}\n\
                 export LOG_LEVEL=INFO\n\
                 export HOME=/home/{user}\n\
                 cd {wd}\n\
                 exec chpst -u {user}:{user} {pl}\n",
                data = decky_data,
                user = username,
                wd = working_dir,
                pl = plugin_loader
            );
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, &run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            let log_dir = format!("{}/log", sv_dir);
            fs::create_dir_all(&log_dir)?;
            let log_run = "#!/bin/sh\n\
                           [ -d /var/log/plugin_loader ] || install -dm 755 /var/log/plugin_loader\n\
                           exec svlogd -tt /var/log/plugin_loader\n";
            let log_run_path = format!("{}/run", log_dir);
            fs::write(&log_run_path, log_run)?;
            fs::set_permissions(&log_run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written runit service: /etc/runit/sv/plugin_loader/");
        }

        InitSystem::OpenRC => {
            let init_d = format!("{}/etc/init.d", install_root);
            fs::create_dir_all(&init_d)?;

            // start_pre bootstraps env + working directory; command_user
            // drops privileges to the session user.
            let script = format!(
                "#!/sbin/openrc-run\n\
                 description=\"SteamDeck Plugin Loader\"\n\
                 command=\"{pl}\"\n\
                 command_user=\"{user}:{user}\"\n\
                 command_background=true\n\
                 directory=\"{wd}\"\n\
                 pidfile=\"/run/plugin_loader.pid\"\n\
                 \n\
                 export HOMEBREW_FOLDER={data}\n\
                 export UNPRIVILEGED_PATH={data}\n\
                 export PRIVILEGED_PATH={data}\n\
                 export LOG_LEVEL=INFO\n\
                 export HOME=/home/{user}\n\
                 \n\
                 depend() {{\n\
                 \tneed net\n\
                 }}\n",
                pl = plugin_loader,
                wd = working_dir,
                user = username,
                data = decky_data,
            );
            let script_path = format!("{}/plugin_loader", init_d);
            fs::write(&script_path, &script)?;
            fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written OpenRC service: /etc/init.d/plugin_loader");
        }

        InitSystem::S6 => {
            let sv_dir = format!("{}/etc/s6/sv/plugin_loader", install_root);
            fs::create_dir_all(&sv_dir)?;

            fs::write(format!("{}/type", sv_dir), "longrun\n")?;

            // s6-setuidgid drops to the session user.
            let run_script = format!(
                "#!/bin/sh\n\
                 export HOMEBREW_FOLDER={data}\n\
                 export UNPRIVILEGED_PATH={data}\n\
                 export PRIVILEGED_PATH={data}\n\
                 export LOG_LEVEL=INFO\n\
                 export HOME=/home/{user}\n\
                 cd {wd}\n\
                 exec s6-setuidgid {user} {pl} 2>&1\n",
                data = decky_data,
                user = username,
                wd = working_dir,
                pl = plugin_loader
            );
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, &run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written s6 service: /etc/s6/sv/plugin_loader/");
        }

        InitSystem::Dinit => {
            let dinit_d = format!("{}/etc/dinit.d", install_root);
            fs::create_dir_all(&dinit_d)?;

            let env_content = format!(
                "HOMEBREW_FOLDER={data}\n\
                 UNPRIVILEGED_PATH={data}\n\
                 PRIVILEGED_PATH={data}\n\
                 LOG_LEVEL=INFO\n\
                 HOME=/home/{user}\n",
                data = decky_data,
                user = username
            );
            let env_path = format!("{}/plugin_loader.env", dinit_d);
            fs::write(&env_path, &env_content)?;
            fs::set_permissions(&env_path, fs::Permissions::from_mode(0o644))?;

            let service = format!(
                "type = process\n\
                 command = {pl}\n\
                 working-dir = {wd}\n\
                 run-as = {user}\n\
                 env-file = /etc/dinit.d/plugin_loader.env\n\
                 restart = true\n",
                pl = plugin_loader,
                wd = working_dir,
                user = username,
            );
            let service_path = format!("{}/plugin_loader", dinit_d);
            fs::write(&service_path, &service)?;
            fs::set_permissions(&service_path, fs::Permissions::from_mode(0o644))?;

            info!("  Written dinit service: /etc/dinit.d/plugin_loader");
        }
    }

    Ok(())
}

// ======================== evdevhook2 ========================

/// AUR package installed for evdevhook2.  Built by upstream author (v1993)
/// from <https://github.com/v1993/evdevhook2> — a Cemuhook UDP motion server
/// supporting modern Linux drivers (`hid-playstation`, `hid-nintendo`,
/// `hid-sony`).
const EVDEVHOOK2_AUR_PACKAGES: &[&str] = &["evdevhook2-git"];

/// udev rule shipped with evdevhook2 — grants the `input` group read/write
/// access (MODE=0660) on motion sensor evdev nodes exposed by Sony
/// controllers, *and* tags them with `uaccess` so the active local-session
/// user also gets ACL access.
///
/// Covers VID 054c (Sony Interactive Entertainment):
///   - DualShock 3       (0x0268, no gyro)
///   - DualShock 4       (0x05c4)
///   - DualShock 4 v2    (0x09cc)
///   - DualSense         (0x0ce6)
///   - DualSense Edge    (0x0df2)
const EVDEVHOOK2_UDEV_RULES: &str = "\
# udev rules for evdevhook2 (installed by Deploytix)\n\
#\n\
# Grants the locally logged-in user (via uaccess/ACL) and members of the\n\
# 'input' group read-write access to motion sensor evdev nodes exposed by\n\
# Sony controllers so that evdevhook2 does not need to run as root.\n\
\n\
ACTION!=\"add|change\", GOTO=\"evdevhook2_end\"\n\
SUBSYSTEM!=\"input\", GOTO=\"evdevhook2_end\"\n\
\n\
# Sony Interactive Entertainment (VID 054c)\n\
# DualShock 3\n\
KERNEL==\"event*\", ATTRS{id/vendor}==\"054c\", ATTRS{id/product}==\"0268\", TAG+=\"uaccess\", MODE=\"0660\", GROUP=\"input\"\n\
# DualShock 4\n\
KERNEL==\"event*\", ATTRS{id/vendor}==\"054c\", ATTRS{id/product}==\"05c4\", TAG+=\"uaccess\", MODE=\"0660\", GROUP=\"input\"\n\
# DualShock 4 (2nd gen)\n\
KERNEL==\"event*\", ATTRS{id/vendor}==\"054c\", ATTRS{id/product}==\"09cc\", TAG+=\"uaccess\", MODE=\"0660\", GROUP=\"input\"\n\
# DualSense\n\
KERNEL==\"event*\", ATTRS{id/vendor}==\"054c\", ATTRS{id/product}==\"0ce6\", TAG+=\"uaccess\", MODE=\"0660\", GROUP=\"input\"\n\
# DualSense Edge\n\
KERNEL==\"event*\", ATTRS{id/vendor}==\"054c\", ATTRS{id/product}==\"0df2\", TAG+=\"uaccess\", MODE=\"0660\", GROUP=\"input\"\n\
\n\
LABEL=\"evdevhook2_end\"\n\
";

/// Install evdevhook2 via yay, add the user to the `input` group, write a
/// udev rule that grants that group access to motion sensor evdev nodes,
/// and write an init-specific service file so the Cemuhook UDP server
/// starts automatically on boot as the configured user.
///
/// Upstream only ships an AppImage; we generate the appropriate service
/// file for whichever init system the user has chosen and run the daemon
/// as the login user (not root) so it matches the `input`-group + uaccess
/// permission model.
///
/// Requires `install_yay = true` — the caller (`installer.rs`) checks this.
pub fn install_evdevhook2(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_evdevhook2 {
        return Ok(());
    }

    let username = &config.user.name;

    info!("Installing evdevhook2 for user {}", username);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install AUR packages via yay as {}: {}",
            username,
            EVDEVHOOK2_AUR_PACKAGES.join(" ")
        );
        println!("  [dry-run] Would write /etc/udev/rules.d/60-evdevhook2.rules");
        println!(
            "  [dry-run] Would add user '{}' to the 'input' group",
            username
        );
        println!(
            "  [dry-run] Would write evdevhook2 service file for init: {}",
            config.system.init
        );
        return Ok(());
    }

    // Step 1: Install AUR package via yay
    let install_cmd = format!(
        "sudo -u {} yay -S --noconfirm --needed {}",
        username,
        EVDEVHOOK2_AUR_PACKAGES.join(" ")
    );
    cmd.run_in_chroot(install_root, &install_cmd)?;
    info!("  evdevhook2 AUR package installed");

    // Step 2: Write the udev rule (GROUP=input, uaccess tag) that grants
    // the user access to /dev/input/event* motion sensor nodes without
    // being root.
    let rules_dir = format!("{}/etc/udev/rules.d", install_root);
    fs::create_dir_all(&rules_dir)?;
    let rules_path = format!("{}/60-evdevhook2.rules", rules_dir);
    fs::write(&rules_path, EVDEVHOOK2_UDEV_RULES)?;
    fs::set_permissions(&rules_path, fs::Permissions::from_mode(0o644))?;
    info!("  Written udev rule: /etc/udev/rules.d/60-evdevhook2.rules");

    // Step 3: Add the user to the `input` group so the service (running as
    // that user) can read the motion sensor evdev nodes before any local
    // login session has been established (i.e. at boot, before uaccess
    // ACLs are applied).  `gpasswd -a` is idempotent.
    cmd.run_in_chroot(install_root, &format!("gpasswd -a {} input", username))?;
    info!("  Added user '{}' to the 'input' group", username);

    // Step 4: Write init-specific service file
    write_evdevhook2_service(config, install_root, username)?;

    info!("evdevhook2 installation complete");
    Ok(())
}

/// Write the evdevhook2 service file for the configured init system.
///
/// evdevhook2 is run as the configured user (a member of the `input`
/// group, see `install_evdevhook2()`).  With the udev rule shipped above,
/// the user has read-write access to the motion sensor evdev nodes, so
/// the daemon does not require root privileges.
///
/// No command-line arguments are required: without a config file
/// evdevhook2 binds the default UDP port (26760) and exposes every
/// supported motion-capable controller automatically.
fn write_evdevhook2_service(
    config: &DeploymentConfig,
    install_root: &str,
    username: &str,
) -> Result<()> {
    use crate::config::InitSystem;

    match config.system.init {
        InitSystem::Runit => {
            let sv_dir = format!("{}/etc/runit/sv/evdevhook2", install_root);
            fs::create_dir_all(&sv_dir)?;

            // chpst -u <user> drops uid/gid (and supplementary groups,
            // including 'input') before exec'ing evdevhook2.  dbus is a
            // soft dependency for UPower battery reporting.
            let run_script = format!(
                "#!/bin/sh\n\
                 # evdevhook2 runit service - Cemuhook UDP motion server\n\
                 sv check dbus >/dev/null || exit 1\n\
                 exec 2>&1\n\
                 exec chpst -u {user} /usr/bin/evdevhook2\n",
                user = username
            );
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, &run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            // log/run — pipe to svlogd
            let log_dir = format!("{}/log", sv_dir);
            fs::create_dir_all(&log_dir)?;
            let log_run = "#!/bin/sh\n\
                           [ -d /var/log/evdevhook2 ] || install -dm 755 /var/log/evdevhook2\n\
                           exec svlogd -tt /var/log/evdevhook2\n";
            let log_run_path = format!("{}/run", log_dir);
            fs::write(&log_run_path, log_run)?;
            fs::set_permissions(&log_run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written runit service: /etc/runit/sv/evdevhook2/");
        }

        InitSystem::OpenRC => {
            let init_d = format!("{}/etc/init.d", install_root);
            fs::create_dir_all(&init_d)?;

            // command_user drops privileges to the configured user.
            let script = format!(
                "#!/sbin/openrc-run\n\
                 description=\"evdevhook2 Cemuhook UDP motion server\"\n\
                 command=\"/usr/bin/evdevhook2\"\n\
                 command_user=\"{user}:{user}\"\n\
                 command_background=true\n\
                 pidfile=\"/run/evdevhook2.pid\"\n\
                 \n\
                 depend() {{\n\
                 \tneed udev dbus\n\
                 }}\n",
                user = username
            );
            let script_path = format!("{}/evdevhook2", init_d);
            fs::write(&script_path, &script)?;
            fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written OpenRC service: /etc/init.d/evdevhook2");
        }

        InitSystem::S6 => {
            let sv_dir = format!("{}/etc/s6/sv/evdevhook2", install_root);
            fs::create_dir_all(&sv_dir)?;

            // type file declares this a long-running service
            fs::write(format!("{}/type", sv_dir), "longrun\n")?;

            // s6-setuidgid drops to the configured user.
            let run_script = format!(
                "#!/bin/sh\n\
                 exec s6-setuidgid {user} /usr/bin/evdevhook2 2>&1\n",
                user = username
            );
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, &run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written s6 service: /etc/s6/sv/evdevhook2/");
        }

        InitSystem::Dinit => {
            let dinit_d = format!("{}/etc/dinit.d", install_root);
            fs::create_dir_all(&dinit_d)?;

            let service = format!(
                "type = process\n\
                 command = /usr/bin/evdevhook2\n\
                 run-as = {user}\n\
                 restart = true\n",
                user = username
            );
            let service_path = format!("{}/evdevhook2", dinit_d);
            fs::write(&service_path, &service)?;
            fs::set_permissions(&service_path, fs::Permissions::from_mode(0o644))?;

            info!("  Written dinit service: /etc/dinit.d/evdevhook2");
        }
    }

    Ok(())
}
