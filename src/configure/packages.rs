//! Optional package collection installers
//!
//! Provides installation functions for:
//! - GPU drivers (NVIDIA, AMD, Intel)
//! - Wine compatibility layer
//! - Gaming packages (Steam, gamescope)
//! - yay AUR helper (built from source)

use crate::config::{DeploymentConfig, GpuDriverVendor};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
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

const WINE_PACKAGES: &[&str] = &[
    "wine",
    "vkd3d",
    "winetricks",
    "wine-mono",
    "wine-gecko",
];

/// Install Wine compatibility packages via pacman in chroot.
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
        println!("  [dry-run] Would install Wine packages: {:?}", WINE_PACKAGES);
        return Ok(());
    }

    let pkg_list = WINE_PACKAGES.join(" ");
    let install_cmd = format!("pacman -S --noconfirm --needed {}", pkg_list);
    cmd.run_in_chroot(install_root, &install_cmd)?;

    info!("Wine installation complete");
    Ok(())
}

// ======================== Gaming Packages ========================

const GAMING_PACKAGES: &[&str] = &["steam", "gamescope"];

/// Install gaming packages via pacman in chroot.
pub fn install_gaming_packages(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_gaming {
        return Ok(());
    }

    info!("Installing gaming packages");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would install gaming packages: {:?}", GAMING_PACKAGES);
        return Ok(());
    }

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

    // Create a temporary build directory owned by the user
    cmd.run_in_chroot(
        install_root,
        &format!("mkdir -p /tmp/yay-build && chown {0}:{0} /tmp/yay-build", username),
    )?;

    // Clone and build yay as the non-root user
    let build_cmd = format!(
        "sudo -u {} bash -c 'cd /tmp/yay-build && \
         git clone https://aur.archlinux.org/yay.git && \
         cd yay && \
         makepkg -si --noconfirm'",
        username
    );
    cmd.run_in_chroot(install_root, &build_cmd)?;

    // Clean up build directory
    cmd.run_in_chroot(install_root, "rm -rf /tmp/yay-build")?;

    info!("yay AUR helper installed successfully");
    Ok(())
}
