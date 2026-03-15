//! Optional package collection installers
//!
//! Provides installation functions for:
//! - GPU drivers (NVIDIA, AMD, Intel)
//! - Wine compatibility layer
//! - Gaming packages (Steam, gamescope)
//! - yay AUR helper (built from source)
//! - Btrfs snapshot tools (snapper, btrfs-assistant) via yay
//! - User autostart entries (audio-startup, nm-applet)

use crate::config::{DeploymentConfig, GpuDriverVendor};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::info;

// ======================== GPU Driver Packages ========================

const NVIDIA_PACKAGES: &[&str] = &["nvidia", "nvidia-utils", "linux-firmware-nvidia"];

const AMD_PACKAGES: &[&str] = &[
    "linux-firmware-amdgpu",
    "amdgpu",
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
        println!("  [dry-run] Would install GPU driver packages: {:?}", packages);
        return Ok(());
    }

    let pkg_list = packages.join(" ");
    let install_cmd = format!("pacman -S --noconfirm --needed {}", pkg_list);
    cmd.run_in_chroot(install_root, &install_cmd)?;

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
    cmd.run_in_chroot(
        install_root,
        "pacman -S --noconfirm --needed artix-archlinux-support",
    )?;

    // Trust the Arch Linux package signing keys.
    info!("Populating Arch Linux keyring in chroot");
    cmd.run_in_chroot(install_root, "pacman-key --populate archlinux")?;

    // Append [extra] to the chroot's pacman.conf if not already present.
    let chroot_pacman_conf = format!("{}/etc/pacman.conf", install_root);
    let conf_content =
        std::fs::read_to_string(&chroot_pacman_conf).map_err(crate::utils::error::DeploytixError::Io)?;

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
    let install_cmd = format!("pacman -S --noconfirm --needed {}", pkg_list);
    cmd.run_in_chroot(install_root, &install_cmd)?;

    info!("Wine installation complete");
    Ok(())
}

// ======================== Gaming Packages ========================

const GAMING_PACKAGES: &[&str] = &["steam", "gamescope"];

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
/// 3. Installs Steam and gamescope.
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
        println!("  [dry-run] Would install lib32 Vulkan drivers: {:?}", lib32_vulkan);
        println!("  [dry-run] Would install gaming packages: {:?}", GAMING_PACKAGES);
        return Ok(());
    }

    // Step 1: Enable [lib32] repo so 32-bit packages are available
    enable_lib32_repo(cmd, install_root)?;

    // Step 2: Install lib32 Vulkan driver(s) for selected GPU vendor(s)
    if !lib32_vulkan.is_empty() {
        let vulkan_list = lib32_vulkan.join(" ");
        info!("Installing lib32 Vulkan drivers: {}", vulkan_list);
        let vulkan_cmd = format!("pacman -S --noconfirm --needed {}", vulkan_list);
        cmd.run_in_chroot(install_root, &vulkan_cmd)?;
    }

    // Step 3: Install Steam and gamescope
    let pkg_list = GAMING_PACKAGES.join(" ");
    let install_cmd = format!("pacman -S --noconfirm --needed {}", pkg_list);
    cmd.run_in_chroot(install_root, &install_cmd)?;

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
    info!("Installing yay AUR helper (building from source as {})", username);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would install go and build yay from source as {}", username);
        return Ok(());
    }

    // Ensure build dependencies are present
    cmd.run_in_chroot(
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
        println!("  [dry-run] Would install audio-startup to /home/{}/.local/bin/", username);
        println!("  [dry-run] Would install autostart .desktop entries to /home/{}/.config/autostart/", username);
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

    // Deploy nm-applet.desktop
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

    // Fix ownership: all deployed files should belong to the user, not root
    let chown_cmd = format!(
        "chown -R {0}:{0} /home/{0}/.local /home/{0}/.config",
        username
    );
    cmd.run_in_chroot(install_root, &chown_cmd)?;

    info!("Autostart entries installed successfully");
    Ok(())
}
