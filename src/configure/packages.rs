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
//! - Handheld Daemon (HHD) via AUR + init-specific service file
//! - Decky Loader (Steam plugin framework) + init-specific service file

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
        println!(
            "  [dry-run] Would install GPU driver packages: {:?}",
            packages
        );
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

// ======================== Handheld Daemon (HHD) ========================

/// Install Handheld Daemon (HHD) via yay and write an init-specific service
/// file so that HHD starts automatically on boot.
///
/// HHD is available on the AUR as `hhd`, `adjustor`, and `hhd-ui`.
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
            "  [dry-run] Would install AUR packages via yay as {}: hhd adjustor hhd-ui",
            username
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
        "sudo -u {} yay -S --noconfirm --needed hhd adjustor hhd-ui",
        username
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
fn write_hhd_service(config: &DeploymentConfig, install_root: &str, username: &str) -> Result<()> {
    use crate::config::InitSystem;

    match config.system.init {
        InitSystem::Runit => {
            let sv_dir = format!("{}/etc/runit/sv/hhd", install_root);
            fs::create_dir_all(&sv_dir)?;

            let run_script = format!(
                "#!/bin/sh\n\
                 exec 2>&1\n\
                 exec chpst -u {user} /usr/bin/hhd --user {user}\n",
                user = username
            );
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, &run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            // log/run — pipe to logger
            let log_dir = format!("{}/log", sv_dir);
            fs::create_dir_all(&log_dir)?;
            let log_run = "#!/bin/sh\nexec svlogd -tt /var/log/hhd\n";
            let log_run_path = format!("{}/run", log_dir);
            fs::write(&log_run_path, log_run)?;
            fs::set_permissions(&log_run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written runit service: /etc/runit/sv/hhd/");
        }

        InitSystem::OpenRC => {
            let init_d = format!("{}/etc/init.d", install_root);
            fs::create_dir_all(&init_d)?;

            let script = format!(
                "#!/sbin/openrc-run\n\
                 description=\"Handheld Daemon Service\"\n\
                 command=\"/usr/bin/hhd\"\n\
                 command_args=\"--user {user}\"\n\
                 command_user=\"{user}\"\n\
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

            let run_script = format!(
                "#!/bin/sh\n\
                 exec s6-setuidgid {user} /usr/bin/hhd --user {user} 2>&1\n",
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

            let service = format!(
                "type = process\n\
                 command = /usr/bin/hhd --user {user}\n\
                 run-as = {user}\n\
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

/// Install Decky Loader — the Steam plugin framework — by downloading the
/// latest `PluginLoader` binary from the GitHub releases API and writing an
/// init-specific service file.
///
/// Layout created on the target system:
/// ```
/// /home/{user}/homebrew/
///   services/
///     PluginLoader          (executable binary, downloaded at install time)
///     .loader.version       (tag of the installed release)
///   plugins/                (empty; populated at runtime by Steam/Decky)
/// ~/.steam/steam/.cef-enable-remote-debugging   (flag file for Steam's CEF)
/// ```
///
/// Requires `install_gaming = true` (Steam must be present).
pub fn install_decky_loader(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_decky_loader {
        return Ok(());
    }

    let username = &config.user.name;
    let homebrew = format!("/home/{}/homebrew", username);
    let homebrew_host = format!("{}{}", install_root, homebrew);

    info!("Installing Decky Loader for user {}", username);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would create {}/services/ and {}/plugins/",
            homebrew, homebrew
        );
        println!("  [dry-run] Would download PluginLoader from GitHub releases");
        println!(
            "  [dry-run] Would write Decky Loader service file for init: {}",
            config.system.init
        );
        return Ok(());
    }

    // Step 1: Create directory structure
    fs::create_dir_all(format!("{}/services", homebrew_host))?;
    fs::create_dir_all(format!("{}/plugins", homebrew_host))?;
    info!("  Created homebrew directory structure");

    // Step 2: Enable Steam CEF remote debugging (required by Decky's frontend)
    let steam_dir = format!("{}/home/{}/.steam/steam", install_root, username);
    fs::create_dir_all(&steam_dir)?;
    fs::write(format!("{steam_dir}/.cef-enable-remote-debugging"), "")?;

    // Also handle Flatpak Steam if present (no-op when the dir doesn't exist)
    let flatpak_steam = format!(
        "{}/home/{}/.var/app/com.valvesoftware.Steam/data/Steam",
        install_root, username
    );
    if std::path::Path::new(&flatpak_steam).exists() {
        fs::write(format!("{flatpak_steam}/.cef-enable-remote-debugging"), "")?;
    }
    info!("  Enabled Steam CEF remote debugging");

    // Step 3: Download PluginLoader binary via the GitHub releases API.
    // We fetch the releases list with curl, parse it with jq to find the
    // latest non-prerelease asset URL, then pipe that into a second curl.
    let download_cmd = format!(
        r#"curl -sL "https://api.github.com/repos/SteamDeckHomebrew/decky-loader/releases" \
  | jq -r 'first(.[] | select(.prerelease == false)) | .assets[] | .browser_download_url | select(endswith("PluginLoader"))' \
  | xargs -I{{}} curl -Lo {homebrew}/services/PluginLoader {{}} \
  && chmod +x {homebrew}/services/PluginLoader"#,
        homebrew = homebrew
    );
    cmd.run_in_chroot(install_root, &download_cmd)?;
    info!("  Downloaded PluginLoader binary");

    // Step 4: Record the installed version tag
    let version_cmd = format!(
        r#"curl -sL "https://api.github.com/repos/SteamDeckHomebrew/decky-loader/releases" \
  | jq -r 'first(.[] | select(.prerelease == false)) | .tag_name' \
  > {homebrew}/services/.loader.version"#,
        homebrew = homebrew
    );
    cmd.run_in_chroot(install_root, &version_cmd)?;
    info!("  Recorded loader version");

    // Step 5: Write init-specific service file
    write_decky_service(config, install_root, username, &homebrew)?;

    // Step 6: Fix ownership of everything under ~/homebrew
    let chown_cmd = format!(
        "chown -R {user}:{user} /home/{user}/homebrew /home/{user}/.steam",
        user = username
    );
    cmd.run_in_chroot(install_root, &chown_cmd)?;

    info!("Decky Loader installation complete");
    Ok(())
}

/// Write the plugin_loader service file for the configured init system.
fn write_decky_service(
    config: &DeploymentConfig,
    install_root: &str,
    _username: &str,
    homebrew: &str,
) -> Result<()> {
    use crate::config::InitSystem;

    // Decky Loader must run as root so it can inject into Steam's process.
    // The UNPRIVILEGED_PATH/PRIVILEGED_PATH env vars tell it where the
    // homebrew directory lives.

    match config.system.init {
        InitSystem::Runit => {
            let sv_dir = format!("{}/etc/runit/sv/plugin_loader", install_root);
            fs::create_dir_all(&sv_dir)?;

            let run_script = format!(
                "#!/bin/sh\n\
                 export UNPRIVILEGED_PATH={hb}\n\
                 export PRIVILEGED_PATH={hb}\n\
                 export LOG_LEVEL=INFO\n\
                 exec {hb}/services/PluginLoader 2>&1\n",
                hb = homebrew
            );
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, &run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            let log_dir = format!("{}/log", sv_dir);
            fs::create_dir_all(&log_dir)?;
            let log_run = "#!/bin/sh\nexec svlogd -tt /var/log/plugin_loader\n";
            let log_run_path = format!("{}/run", log_dir);
            fs::write(&log_run_path, log_run)?;
            fs::set_permissions(&log_run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written runit service: /etc/runit/sv/plugin_loader/");
        }

        InitSystem::OpenRC => {
            let init_d = format!("{}/etc/init.d", install_root);
            fs::create_dir_all(&init_d)?;

            let script = format!(
                "#!/sbin/openrc-run\n\
                 description=\"SteamDeck Plugin Loader\"\n\
                 command=\"{hb}/services/PluginLoader\"\n\
                 command_background=true\n\
                 pidfile=\"/var/run/plugin_loader.pid\"\n\
                 \n\
                 export UNPRIVILEGED_PATH={hb}\n\
                 export PRIVILEGED_PATH={hb}\n\
                 export LOG_LEVEL=INFO\n\
                 \n\
                 depend() {{\n\
                 \tneed net\n\
                 }}\n",
                hb = homebrew
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

            let run_script = format!(
                "#!/bin/sh\n\
                 export UNPRIVILEGED_PATH={hb}\n\
                 export PRIVILEGED_PATH={hb}\n\
                 export LOG_LEVEL=INFO\n\
                 exec {hb}/services/PluginLoader 2>&1\n",
                hb = homebrew
            );
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, &run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written s6 service: /etc/s6/sv/plugin_loader/");
        }

        InitSystem::Dinit => {
            let dinit_d = format!("{}/etc/dinit.d", install_root);
            fs::create_dir_all(&dinit_d)?;

            // Write env file referenced by the service descriptor
            let env_content = format!(
                "UNPRIVILEGED_PATH={hb}\nPRIVILEGED_PATH={hb}\nLOG_LEVEL=INFO\n",
                hb = homebrew
            );
            let env_path = format!("{}/plugin_loader.env", dinit_d);
            fs::write(&env_path, &env_content)?;
            fs::set_permissions(&env_path, fs::Permissions::from_mode(0o644))?;

            let service = format!(
                "type = process\n\
                 command = {hb}/services/PluginLoader\n\
                 working-dir = {hb}/services\n\
                 env-file = /etc/dinit.d/plugin_loader.env\n\
                 restart = true\n",
                hb = homebrew
            );
            let service_path = format!("{}/plugin_loader", dinit_d);
            fs::write(&service_path, &service)?;
            fs::set_permissions(&service_path, fs::Permissions::from_mode(0o644))?;

            info!("  Written dinit service: /etc/dinit.d/plugin_loader");
        }
    }

    Ok(())
}
