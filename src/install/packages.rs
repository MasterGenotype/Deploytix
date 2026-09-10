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

use crate::aur::build as aur_build;
use crate::config::{DeploymentConfig, GpuDriverVendor, TkgScheduler};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use crate::utils::interactive::PacmanInvocation;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::{info, warn};

// ======================== Reviewed install helpers ========================

/// Run a chroot `pacman -S` install via `pacman_install_chroot`, after
/// passing the package list through the interactive policy.  Returns
/// `Ok(())` with no install when the policy skips this step.
pub(crate) fn pacman_install_chroot_reviewed(
    cmd: &CommandRunner,
    install_root: &str,
    label: &str,
    packages: Vec<String>,
) -> Result<()> {
    pacman_install_chroot_reviewed_status(cmd, install_root, label, packages).map(|_| ())
}

/// As [`pacman_install_chroot_reviewed`], but reports whether the transaction
/// actually ran: `Ok(false)` means the interactive policy skipped it.  Use
/// this when later steps depend on the packages being present.
pub(crate) fn pacman_install_chroot_reviewed_status(
    cmd: &CommandRunner,
    install_root: &str,
    label: &str,
    packages: Vec<String>,
) -> Result<bool> {
    let inv = PacmanInvocation::pacman_chroot(install_root, label, packages);
    let Some(inv) = cmd.review_pacman(inv)? else {
        return Ok(false);
    };
    let extras = if inv.extra_flags.is_empty() {
        String::new()
    } else {
        format!("{} ", inv.extra_flags.join(" "))
    };
    let install_cmd = format!(
        "pacman -S --noconfirm --needed {}{}",
        extras,
        inv.packages.join(" ")
    );
    pacman_install_chroot(cmd, install_root, &install_cmd)?;
    Ok(true)
}

/// Run `sudo -u <user> yay -S` in chroot, after passing the package list
/// through the interactive policy.
pub(crate) fn yay_install_chroot_reviewed(
    cmd: &CommandRunner,
    install_root: &str,
    run_as_user: &str,
    label: &str,
    packages: Vec<String>,
) -> Result<()> {
    let inv = PacmanInvocation::yay_chroot(install_root, run_as_user, label, packages);
    let Some(inv) = cmd.review_pacman(inv)? else {
        return Ok(());
    };
    let extras = if inv.extra_flags.is_empty() {
        String::new()
    } else {
        format!("{} ", inv.extra_flags.join(" "))
    };
    let cmd_str = format!(
        "sudo -u {} yay -S --noconfirm --needed {}{}",
        inv.run_as_user.as_deref().unwrap_or(run_as_user),
        extras,
        inv.packages.join(" ")
    );
    cmd.run_in_chroot(install_root, &cmd_str).map(|_| ())
}

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
    let _ = cmd.run_in_chroot(install_root, "pacman -Sy --noconfirm artix-keyring");

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

/// NVIDIA packages for a kernel the prebuilt `nvidia` module was not built
/// against.  `nvidia` ships a module tied to a stock kernel's ABI and simply
/// will not load on linux-tkg; `nvidia-dkms` builds against whatever kernel is
/// installed, which is why the headers package comes down alongside the kernel.
const NVIDIA_DKMS_PACKAGES: &[&str] = &[
    "nvidia-dkms",
    "nvidia-utils",
    "linux-firmware-nvidia",
    "dkms",
];

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
                if config.packages.install_tkg_kernel {
                    info!("Adding NVIDIA GPU driver packages (DKMS, for the linux-tkg kernel)");
                    packages.extend(NVIDIA_DKMS_PACKAGES);
                } else {
                    info!("Adding NVIDIA GPU driver packages");
                    packages.extend(NVIDIA_PACKAGES);
                }
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

    let pkg_strings: Vec<String> = packages.iter().map(|s| s.to_string()).collect();
    pacman_install_chroot_reviewed(cmd, install_root, "GPU drivers", pkg_strings)?;

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

        // Always use a direct Server URL rather than Include = mirrorlist-arch.
        // The chroot preflight runs host-side pacman with --config pointing at
        // the chroot's pacman.conf, but pacman resolves Include paths relative
        // to the host root — not --root.  If artix-archlinux-support is not
        // installed on the host ISO, /etc/pacman.d/mirrorlist-arch won't exist
        // and every chroot preflight will fail with "could not be read".
        // Inside artix-chroot the geo mirror works identically to the mirrorlist.
        let mirror_entry = "Server = https://geo.mirror.pkgbuild.com/$repo/os/$arch";

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

    let all_pkgs: Vec<String> = WINE_PACKAGES_ARTIX
        .iter()
        .chain(WINE_PACKAGES_ARCH_EXTRA.iter())
        .map(|s| s.to_string())
        .collect();
    pacman_install_chroot_reviewed(cmd, install_root, "Wine compatibility", all_pkgs)?;

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

/// Pulled in alongside Steam when `steam_prefetch_client` is set: Steam will
/// not run its client bootstrap without a display, and Xvfb is the smallest
/// thing that satisfies it inside a chroot.
/// `xorg-server-xvfb` ships `xvfb-run`, but `xvfb-run` shells out to `xauth`
/// to write its cookie file and does so under its own `set -e`, so without
/// `xorg-xauth` it exits before Steam ever starts. `xorg-server-xvfb` does not
/// depend on `xauth`, so both have to be asked for.
const STEAM_PREFETCH_PACKAGES: &[&str] = &["xorg-server-xvfb", "xorg-xauth"];

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
        if config.packages.steam_prefetch_client {
            println!(
                "  [dry-run] Would install {} and prefetch the Steam client",
                STEAM_PREFETCH_PACKAGES.join(" ")
            );
        }
        return Ok(());
    }

    // Step 1: Enable [lib32] repo so 32-bit packages are available
    enable_lib32_repo(cmd, install_root)?;

    // Step 2: Install lib32 Vulkan driver(s) for selected GPU vendor(s)
    if !lib32_vulkan.is_empty() {
        info!(
            "Installing lib32 Vulkan drivers: {}",
            lib32_vulkan.join(" ")
        );
        let pkgs: Vec<String> = lib32_vulkan.iter().map(|s| s.to_string()).collect();
        pacman_install_chroot_reviewed(cmd, install_root, "lib32 Vulkan drivers", pkgs)?;
    }

    // Step 3: Install Steam (plus Xvfb when the client is prefetched below —
    // Steam refuses to bootstrap without a display of some kind).
    let mut gaming_pkgs: Vec<String> = GAMING_PACKAGES.iter().map(|s| s.to_string()).collect();
    if config.packages.steam_prefetch_client {
        gaming_pkgs.extend(STEAM_PREFETCH_PACKAGES.iter().map(|p| p.to_string()));
    }
    pacman_install_chroot_reviewed(cmd, install_root, "Gaming (Steam, etc.)", gaming_pkgs)?;

    // Step 4: Seed the Steam client bootstrap into the user's home, so the
    // gamepad UI has its runtime before the machine has ever been online.
    seed_steam_bootstrap(cmd, config, install_root)?;

    // Step 5: Optionally download the client itself, so the target boots
    // straight into Game Mode instead of spending its first boot fetching it.
    prefetch_steam_client(cmd, config, install_root)?;

    info!("Gaming package installation complete");
    Ok(())
}

/// Extract the Steam client bootstrap into the user's Steam directory.
///
/// SteamOS images ship `bootstraplinux_ubuntu12_32.tar.xz` under
/// `/etc/first-boot` and extract it into `~/.local/share/Steam` before Steam
/// has ever run, so Game Mode comes up with its runtime already in place.
/// Artix's `steam` package ships the same tarball at
/// `/usr/lib/steam/bootstraplinux_ubuntu12_32.tar.xz`; doing the extraction
/// here rather than leaving it to Steam's first launch removes one network
/// round-trip from the very first boot, which is precisely the boot on which
/// no Wi-Fi credentials may exist yet.
///
/// Idempotent: a marker file inside the Steam directory means re-running the
/// installer over an existing system does not re-extract over live state.
fn seed_steam_bootstrap(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let username = &config.user.name;

    info!("Seeding Steam client bootstrap for user {}", username);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would extract /usr/lib/steam/bootstraplinux_ubuntu12_32.tar.xz \
             into /home/{}/.local/share/Steam",
            username
        );
        return Ok(());
    }

    cmd.run_in_chroot(install_root, &steam_bootstrap_script(username))?;

    Ok(())
}

/// Render the in-chroot script used by [`seed_steam_bootstrap`].
///
/// `set -e` is deliberately absent: a missing tarball or an unreadable
/// archive must not fail the install — Steam still bootstraps itself on
/// first launch, this only front-loads the work.
fn steam_bootstrap_script(username: &str) -> String {
    format!(
        "BOOTSTRAP=/usr/lib/steam/bootstraplinux_ubuntu12_32.tar.xz\n\
         STEAMDIR=/home/{user}/.local/share/Steam\n\
         MARKER=\"$STEAMDIR/.deploytix-bootstrap-seeded\"\n\
         if [ ! -f \"$BOOTSTRAP\" ]; then\n\
         echo 'steam bootstrap tarball not found; Steam will bootstrap itself on first launch'\n\
         exit 0\n\
         fi\n\
         if [ -f \"$MARKER\" ]; then\n\
         echo 'steam bootstrap already seeded; skipping'\n\
         exit 0\n\
         fi\n\
         mkdir -p \"$STEAMDIR\"\n\
         if tar xf \"$BOOTSTRAP\" -C \"$STEAMDIR\"; then\n\
         touch \"$MARKER\"\n\
         echo \"seeded steam bootstrap into $STEAMDIR\"\n\
         else\n\
         echo 'steam bootstrap extraction failed; Steam will retry on first launch'\n\
         fi\n\
         chown -R {user}:{user} /home/{user}/.local\n",
        user = username
    )
}

/// Download the Steam client into the user's home during installation.
///
/// [`seed_steam_bootstrap`] only unpacks the launcher. The client proper —
/// including `ubuntu12_64/steamwebhelper` and `steamui.so`, which *are* the
/// gamepad UI — is fetched by Steam's first real run. Leaving that to the
/// target's first boot is what makes Game Mode fall back to the desktop there:
/// `steam -gamepadui` has nothing to draw until the download completes. Doing
/// it here, where the installer already has a working network, means the
/// deployed system boots straight into Game Mode.
///
/// `steam +quit` is the standard way to drive that headlessly: Steam
/// bootstraps and updates on startup, then the `+quit` console command exits
/// the client it just installed. It still needs a display, hence `xvfb-run`.
///
/// Best-effort by design, like the seed above: no network, no Xvfb, a timeout
/// or a Steam that exits non-zero must not fail an otherwise complete install.
/// `steam-gamescope-session` bootstraps on first boot when this did not.
fn prefetch_steam_client(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.steam_prefetch_client {
        return Ok(());
    }
    let username = &config.user.name;

    info!("Prefetching the Steam client for user {} (this downloads a few hundred MB and can take several minutes)", username);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would run `xvfb-run steam +quit` as {} to download the Steam client",
            username
        );
        return Ok(());
    }

    // A non-zero exit is information, not a failure: log it and move on.
    if let Err(e) = cmd.run_in_chroot(install_root, &steam_prefetch_script(username)) {
        warn!(
            "Steam client prefetch did not complete ({}); the target will download it on first boot",
            e
        );
    }
    Ok(())
}

/// Render the in-chroot script used by [`prefetch_steam_client`].
///
/// `set -e` is deliberately absent, and every step is guarded: the install
/// must survive a missing `xvfb-run`, an offline mirror, or a Steam that hangs.
/// The `timeout` is the outer bound on all of it.
fn steam_prefetch_script(username: &str) -> String {
    format!(
        "STEAMDIR=/home/{user}/.local/share/Steam\n\
         if [ -s \"$STEAMDIR/ubuntu12_64/steamui.so\" ]; then\n\
         echo 'steam client already present; skipping prefetch'\n\
         exit 0\n\
         fi\n\
         if ! command -v xvfb-run >/dev/null 2>&1; then\n\
         echo 'xvfb-run not available; skipping prefetch (client downloads on first boot)'\n\
         exit 0\n\
         fi\n\
         if ! command -v xauth >/dev/null 2>&1; then\n\
         echo 'xauth not available; xvfb-run cannot start, skipping prefetch'\n\
         exit 0\n\
         fi\n\
         su -s /bin/sh {user} -c \
         'timeout {timeout} xvfb-run -a -s \"-screen 0 1024x768x24\" steam -silent +quit' \
         || echo 'steam exited non-zero during prefetch'\n\
         if [ -s \"$STEAMDIR/ubuntu12_64/steamui.so\" ]; then\n\
         echo \"prefetched steam client into $STEAMDIR\"\n\
         else\n\
         echo 'steam client still incomplete; it will finish downloading on first boot'\n\
         fi\n\
         chown -R {user}:{user} /home/{user}/.local 2>/dev/null\n\
         chown -R {user}:{user} /home/{user}/.steam 2>/dev/null\n\
         exit 0\n",
        user = username,
        timeout = STEAM_PREFETCH_TIMEOUT_SECS,
    )
}

/// Outer bound on the prefetch. Generous enough for a slow link, short enough
/// that a wedged Steam cannot hold an install open indefinitely.
const STEAM_PREFETCH_TIMEOUT_SECS: u32 = 1800;

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
    pacman_install_chroot_reviewed(
        cmd,
        install_root,
        "yay build deps (go, git, base-devel)",
        vec![
            "go".to_string(),
            "git".to_string(),
            "base-devel".to_string(),
        ],
    )?;

    // Build on the disk-backed scratch, not /tmp.  Both chroot paths give the
    // target a tmpfs /tmp with no size= (artix-chroot mounts one itself; the
    // plain-chroot fallback does it in chroot_api_setup_cmd), so the kernel
    // caps it at half of RAM and a large build tree hits ENOSPC while the root
    // volume still has tens of gigabytes free -- the failure mode
    // docs/TMP_DISK_BACKED.md describes for the booted system.
    //
    // Still one chroot invocation: a tmpfs /tmp also means a directory created
    // in one invocation would not survive to the next.
    aur_build::ensure_build_root(cmd, install_root, username)?;
    let clone_dir = format!("{}/yay", aur_build::BUILD_ROOT);
    let build_cmd = format!(
        "rm -rf {clone} && \
         sudo -u {user} env {env}bash -c '\
           git clone https://aur.archlinux.org/yay.git {clone} && \
           cd {clone} && \
           makepkg -si --noconfirm' && \
         rm -rf {clone}",
        user = username,
        env = aur_build::build_env_prefix(),
        clone = clone_dir,
    );
    cmd.run_in_chroot(install_root, &build_cmd)?;

    info!("yay AUR helper installed successfully");
    Ok(())
}

// ======================== Zen Browser (AUR) ========================

/// The AUR package providing Zen Browser.
const ZEN_BROWSER_AUR_PACKAGE: &str = "zen-browser-bin";

/// Install Zen Browser via yay, when asked for.
///
/// This used to run unconditionally whenever yay was installed, so every
/// install that wanted an AUR helper also got a browser it had not asked for.
/// It is now an option alongside Warp Terminal.
///
/// Requires `install_yay = true`; it is an AUR package and there is nothing to
/// build it with otherwise. The wizard and the GUI only offer it when yay is
/// selected, and this rechecks rather than trusting that.
pub fn install_zen_browser(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_zen_browser {
        return Ok(());
    }
    if !config.packages.install_yay {
        warn!(
            "install_zen_browser = true but install_yay = false; skipping {} \
             (it is an AUR package and needs a helper to build it)",
            ZEN_BROWSER_AUR_PACKAGE
        );
        return Ok(());
    }

    let username = &config.user.name;
    info!(
        "Installing Zen Browser via yay as {}: {}",
        username, ZEN_BROWSER_AUR_PACKAGE
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install {} via yay as {}",
            ZEN_BROWSER_AUR_PACKAGE, username
        );
        return Ok(());
    }

    yay_install_chroot_reviewed(
        cmd,
        install_root,
        username,
        "AUR: Zen Browser",
        vec![ZEN_BROWSER_AUR_PACKAGE.to_string()],
    )?;

    info!("Zen Browser installed");
    Ok(())
}

// ======================== Post-install extras (phase 5.95) ========================

/// Install repository (`pacman -S`) extras provided by the user via the
/// post-install extras step or persisted in `packages.extra_packages.pacman`.
pub fn install_extras_pacman(
    cmd: &CommandRunner,
    install_root: &str,
    packages: &[String],
) -> Result<()> {
    if packages.is_empty() {
        return Ok(());
    }
    info!("Installing pacman extras: {}", packages.join(", "));
    if cmd.is_dry_run() {
        println!("  [dry-run] Would install pacman extras: {:?}", packages);
        return Ok(());
    }
    pacman_install_chroot_reviewed(cmd, install_root, "Extras (pacman)", packages.to_vec())
}

/// Install AUR (`yay -S`) extras provided by the user via the
/// post-install extras step or persisted in `packages.extra_packages.aur`.
/// Requires `install_yay = true` (validated in `DeploymentConfig::validate`).
pub fn install_extras_aur(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
    packages: &[String],
) -> Result<()> {
    if packages.is_empty() {
        return Ok(());
    }
    if !config.packages.install_yay {
        warn!(
            "extra_packages.aur is non-empty but install_yay = false; skipping {} package(s)",
            packages.len()
        );
        return Ok(());
    }
    let username = &config.user.name;
    info!(
        "Installing AUR extras via yay as {}: {}",
        username,
        packages.join(", ")
    );
    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install AUR extras via yay as {}: {:?}",
            username, packages
        );
        return Ok(());
    }
    yay_install_chroot_reviewed(
        cmd,
        install_root,
        username,
        "Extras (AUR)",
        packages.to_vec(),
    )
}

// ======================== Warp Terminal (vendor package) ========================

/// Where the downloaded package is parked inside the chroot before install.
const WARP_PKG_PATH: &str = "/var/cache/deploytix/warp-terminal.pkg.tar.zst";

/// Magic bytes every `.pkg.tar.zst` starts with (the zstd frame header),
/// hex-encoded the way `od -An -N4 -tx1 | tr -d` prints them.
const ZSTD_MAGIC_HEX: &str = "28b52ffd";

/// The in-chroot script that fetches Warp and installs it.
///
/// `curl` is guaranteed present: `pacman` depends on it. The flags that matter:
///   - `-f` makes an HTTP error status a non-zero exit rather than a saved
///     error page that `pacman -U` would then reject as a corrupt package.
///   - `-L` follows redirects. The vendor URL redirects to the real file on
///     `releases.warp.dev`.
///   - `--retry` rides out a hiccup instead of failing the install.
///
/// `-f` is necessary and not sufficient: the wrong vendor URL answers `200`
/// with an HTML page, which curl saves happily under the package's name. So
/// the download is checked for the zstd frame header before `pacman -U` is
/// allowed near it -- a served web page then fails here, saying so, instead of
/// reaching pacman as "invalid or corrupted package" on a best-effort step.
///
/// `pacman -U` accepts the unsigned local file because pacman's
/// `LocalFileSigLevel` defaults to `Optional`. That is the same trust decision
/// as downloading Warp's package by hand, which is what this automates.
pub fn warp_terminal_script() -> String {
    format!(
        "set -e\n\
         mkdir -p \"$(dirname '{path}')\"\n\
         curl -fL --retry 3 --retry-delay 2 --connect-timeout 30 -o '{path}' '{url}'\n\
         magic=$(od -An -N4 -tx1 '{path}' | tr -d ' \\n')\n\
         if [ \"$magic\" != '{magic}' ]; then\n\
         rm -f '{path}'\n\
         echo \"{url} did not serve a pacman package (magic $magic)\" >&2\n\
         exit 1\n\
         fi\n\
         pacman -U --noconfirm '{path}'\n\
         rm -f '{path}'\n",
        path = WARP_PKG_PATH,
        url = crate::config::WARP_TERMINAL_URL,
        magic = ZSTD_MAGIC_HEX,
    )
}

/// Install Warp Terminal from the vendor's Arch package.
///
/// Best-effort, like the other optional extras: a download that fails is
/// reported and the install carries on rather than losing an otherwise
/// complete system to a terminal emulator.
///
/// Returns whether Warp was actually installed. It used to return `Ok(())`
/// whether or not the download worked, which made a silent failure
/// indistinguishable from a success -- the caller reported neither, and the
/// install finished claiming to have done something it had not.
pub fn install_warp_terminal(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<bool> {
    if !config.packages.install_warp_terminal {
        return Ok(false);
    }

    info!(
        "Installing Warp Terminal from {}",
        crate::config::WARP_TERMINAL_URL
    );
    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would download {} and install it with pacman -U",
            crate::config::WARP_TERMINAL_URL
        );
        return Ok(true);
    }

    match cmd.run_in_chroot(install_root, &warp_terminal_script()) {
        Ok(_) => Ok(true),
        Err(e) => Err(DeploytixError::ChrootError(format!(
            "Warp Terminal did not install: {e}"
        ))),
    }
}

// ======================== linux-tkg kernel (prebuilt) ========================

/// Where the downloaded kernel packages are parked inside the chroot.
const TKG_KERNEL_PATH: &str = "/var/cache/deploytix/linux-tkg.pkg.tar.zst";
const TKG_HEADERS_PATH: &str = "/var/cache/deploytix/linux-tkg-headers.pkg.tar.zst";

/// The `grep -o` pattern that finds one linux-tkg release asset URL.
///
/// `headers` is the empty string for the kernel package and `-headers` for its
/// counterpart, which is the whole trick: the two asset names share a prefix,
/// so a pattern written only for the kernel would match the headers asset too.
/// Requiring a digit immediately after `-llvm{headers}-` is what separates
/// them — `…-llvm-7.2.3-273-…` matches the kernel pattern and
/// `…-llvm-headers-7.2.3-273-…` does not, because `h` is not `[0-9.]`.
///
/// The kernel series is `[0-9]*` rather than a literal because it tracks the
/// kernel version (`linux72` today, `linux73` next release) and changes
/// without warning.
fn tkg_asset_pattern(sched: TkgScheduler, headers: bool) -> String {
    format!(
        "https://[^\"]*linux[0-9]*-tkg-{sched}-llvm{h}-[0-9.]*-[0-9]*-x86_64\\.pkg\\.tar\\.zst",
        sched = sched.as_str(),
        h = if headers { "-headers" } else { "" },
    )
}

/// The in-chroot script that fetches the linux-tkg kernel and installs it.
///
/// Structured like [`warp_terminal_script`] — `curl -fL`, a zstd frame-header
/// check before `pacman -U` sees the file — with one addition: the download
/// URL is *resolved* rather than fixed, because linux-tkg's asset names carry
/// the kernel series, version and build number, all of which move every
/// release.
///
/// The releases API is consulted first and the pinned build is the fallback,
/// so an unauthenticated rate limit (60 requests/hour) or an upstream rename
/// degrades to "slightly old kernel" rather than "no kernel". Both packages
/// install in a single `pacman -U` transaction so the headers can never end up
/// paired with a different build than the kernel.
pub fn tkg_kernel_script(sched: TkgScheduler) -> String {
    let (fallback_kernel, fallback_headers) = crate::config::tkg_fallback_urls(sched);
    format!(
        "set -e\n\
         mkdir -p \"$(dirname '{kpath}')\"\n\
         api=$(curl -fsSL --retry 3 --retry-delay 2 --connect-timeout 30 '{api}' || true)\n\
         kurl=$(printf '%s' \"$api\" | grep -o '{kpat}' | head -n1)\n\
         hurl=$(printf '%s' \"$api\" | grep -o '{hpat}' | head -n1)\n\
         if [ -z \"$kurl\" ] || [ -z \"$hurl\" ]; then\n\
         echo 'could not resolve the latest linux-tkg release; using the pinned {tag} build' >&2\n\
         kurl='{fk}'\n\
         hurl='{fh}'\n\
         fi\n\
         echo \"linux-tkg kernel:  $kurl\"\n\
         echo \"linux-tkg headers: $hurl\"\n\
         curl -fL --retry 3 --retry-delay 2 --connect-timeout 30 -o '{kpath}' \"$kurl\"\n\
         curl -fL --retry 3 --retry-delay 2 --connect-timeout 30 -o '{hpath}' \"$hurl\"\n\
         for f in '{kpath}' '{hpath}'; do\n\
         magic=$(od -An -N4 -tx1 \"$f\" | tr -d ' \\n')\n\
         if [ \"$magic\" != '{magic}' ]; then\n\
         rm -f '{kpath}' '{hpath}'\n\
         echo \"$f is not a pacman package (magic $magic)\" >&2\n\
         exit 1\n\
         fi\n\
         done\n\
         pacman -U --noconfirm '{kpath}' '{hpath}'\n\
         rm -f '{kpath}' '{hpath}'\n",
        kpath = TKG_KERNEL_PATH,
        hpath = TKG_HEADERS_PATH,
        api = crate::config::TKG_RELEASES_API,
        kpat = tkg_asset_pattern(sched, false),
        hpat = tkg_asset_pattern(sched, true),
        tag = crate::config::TKG_FALLBACK_TAG,
        fk = fallback_kernel,
        fh = fallback_headers,
        magic = ZSTD_MAGIC_HEX,
    )
}

/// Install the prebuilt linux-tkg kernel in place of `linux-zen`.
///
/// Unlike the other downloaded package ([`install_warp_terminal`]) this is not
/// best-effort. `build_package_list` omits `linux-zen` when this is enabled, so
/// a failure here leaves the target with no kernel at all — the error
/// propagates and the install stops rather than completing onto an unbootable
/// disk.
pub fn install_tkg_kernel(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_tkg_kernel {
        return Ok(());
    }

    let sched = config.packages.tkg_scheduler;
    info!(
        "Installing the prebuilt linux-tkg kernel ({sched}) resolved from {}",
        crate::config::TKG_RELEASES_API
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would resolve the latest linux-tkg {sched} package from {} \
             and install it (with its headers) via pacman -U",
            crate::config::TKG_RELEASES_API
        );
        return Ok(());
    }

    cmd.run_in_chroot(install_root, &tkg_kernel_script(sched))
        .map(|_| ())
        .map_err(|e| {
            DeploytixError::ChrootError(format!(
                "the linux-tkg kernel did not install: {e}. The target has no other kernel \
                 because linux-tkg replaces linux-zen, so the install cannot continue."
            ))
        })
}

// ======================== iwd GUI Frontend (AUR) ========================

/// Install the AUR-only iwd GUI frontend chosen by the user.
///
/// Only runs when the standalone iwd backend is selected and yay is
/// installed.  Validation forces these to come together — see
/// `DeploymentConfig::validate`.
pub fn install_iwd_frontend(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if config.network.backend != crate::config::NetworkBackend::Iwd {
        return Ok(());
    }
    if !config.packages.install_yay {
        return Ok(());
    }

    let username = &config.user.name;
    let pkg = config.network.iwd_frontend.aur_package();
    info!(
        "Installing iwd GUI frontend via yay as {}: {}",
        username, pkg
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install iwd frontend via yay as {}: {}",
            username, pkg
        );
        return Ok(());
    }

    yay_install_chroot_reviewed(
        cmd,
        install_root,
        username,
        &format!("AUR: iwd frontend ({})", pkg),
        vec![pkg.to_string()],
    )?;

    info!("iwd GUI frontend installed successfully");
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

    let pkgs: Vec<String> = BTRFS_TOOL_PACKAGES.iter().map(|s| s.to_string()).collect();
    yay_install_chroot_reviewed(
        cmd,
        install_root,
        username,
        "AUR: Btrfs snapshot tools",
        pkgs,
    )?;

    info!("Btrfs snapshot tools installed successfully");
    Ok(())
}

// ======================== Autostart Entries ========================

/// Embedded audio-startup script (compiled into binary).
const AUDIO_STARTUP_SCRIPT: &str = include_str!("../resources/autostart/audio-startup.sh");

/// Write a file into the user's home, leaving an existing one alone when
/// this run is preserving an existing /home.
///
/// A recovery install lands on a home directory the user has been living in.
/// Overwriting their autostart entries with the installer's defaults would
/// silently undo customisation that has nothing to do with the reinstall.
/// On an ordinary install there is nothing there yet, so this always writes.
fn write_user_file_preserving(
    config: &DeploymentConfig,
    path: &str,
    content: &str,
    mode: u32,
) -> Result<()> {
    let name = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);

    if config.disk.recovery.reuse_home && std::path::Path::new(path).exists() {
        info!("  Keeping the existing {} (preserved home)", name);
        return Ok(());
    }

    fs::write(path, content)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    info!("  Installed {}", name);
    Ok(())
}

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
    write_user_file_preserving(config, &audio_desktop_path, &audio_desktop, 0o644)?;

    // Deploy nm-applet.desktop for any NetworkManager-based backend
    if matches!(
        config.network.backend,
        crate::config::NetworkBackend::NetworkManager
            | crate::config::NetworkBackend::NetworkManagerWpa
    ) {
        let nm_desktop = "[Desktop Entry]\n\
             Type=Application\n\
             Name=Network Manager Applet\n\
             Exec=/bin/nm-applet\n\
             Hidden=false\n\
             NoDisplay=false\n\
             X-GNOME-Autostart-enabled=true\n\
             Comment=NetworkManager system tray applet\n";
        let nm_desktop_path = format!("{}/nm-applet.desktop", autostart_dir);
        write_user_file_preserving(config, &nm_desktop_path, nm_desktop, 0o644)?;
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
/// `hhd-git` is a split PKGBUILD; `hhd-license-git` comes in as a dependency.
/// We use it instead of the tagged `hhd` release or `adjustor` — `adjustor` is
/// now bundled into `hhd` itself (`replaces=(adjustor)` upstream).
///
/// `hhd-ui` **is** the Game Mode overlay, despite the AUR describing it as "a
/// (browser based) graphical user interface". hhd's overlay plugin finds it by
/// name — `find_overlay_exe` in `src/hhd/plugins/overlay/overlay.py` searches
/// `hhd-ui.AppImage`, `hhd-ui-dbg` and `hhd-ui` on PATH and in
/// `~/.local/bin` — and launches it with `STEAM_OVERLAY=1`. Without it
/// `find_overlay_exe` returns `None` and hhd logs "Failed to start hhd-ui":
/// the daemon runs, and nothing the user can reach ever appears in Game Mode.
/// The AUR `hhd-ui` installs `/usr/bin/hhd-ui`, which is exactly what that
/// lookup finds. It pulls in `electron` and builds with npm, so it is the
/// slowest part of an HHD install.
///
/// `hhd-systemd-git` is deliberately **not** installed. It exists to ship a
/// systemd unit, and this is an Artix system with no systemd and no plans for
/// it. Its one useful side effect was `83-hhd.rules`, which deploytix now
/// ships itself — see [`HHD_DATA_FILES`].
///
/// `hhd-ui` **is** the Game Mode overlay, despite the AUR describing it as "a
/// (browser based) graphical user interface". hhd's overlay plugin finds it by
/// name — `find_overlay_exe` in `src/hhd/plugins/overlay/overlay.py` searches
/// `hhd-ui.AppImage`, `hhd-ui-dbg` and `hhd-ui` on PATH and in
/// `~/.local/bin` — and launches it with `STEAM_OVERLAY=1`. Without it
/// `find_overlay_exe` returns `None` and hhd logs "Failed to start hhd-ui":
/// the daemon runs, and nothing the user can reach ever appears in Game Mode.
/// The AUR `hhd-ui` installs `/usr/bin/hhd-ui`, which is exactly what that
/// lookup finds. It pulls in `electron` and builds with npm, so it is the
/// slowest part of an HHD install.
const HHD_AUR_PACKAGES: &[&str] = &["hhd-git", "hhd-ui"];

/// Local patches applied to the installed Handheld Daemon, and the helper that
/// (re-)applies them.
///
/// `hhd-git` is an AUR package, so its files are pacman-owned and every
/// rebuild reverts anything we change. The helper is installed alongside the
/// patches so the user can re-run it after such a rebuild; deploytix runs it
/// once at install time. Both are self-disabling: a patch is dry-run first and
/// skipped if it no longer applies, so a fix landing upstream turns this into
/// a no-op rather than a conflict.
/// Files upstream ships in its `usr/` tree that no package deploytix installs
/// will put on disk.
///
/// `hhd-git` runs `python -m installer` on a wheel whose pyproject builds only
/// `where = ["src"]`, so the repository's entire `usr/` tree is absent from it.
/// `83-hhd.rules` is packaged only by `hhd-systemd-git`, which exists to ship a
/// systemd unit and has no place on an Artix system, so deploytix ships all
/// three itself:
///
/// - `83-hhd.rules` is what makes controllers work: `uaccess` tags on the
///   DualSense hidraw nodes, xpad binding for the MSI Claw, TECNO Pocket Go
///   and Legion Go S, the mask for the Ally HID devices that crash SDL and
///   Proton controller handlers, and the rule that stops iio buffer polling
///   interfering with the controllers.
/// - `83-hhd.hwdb` maps the extra buttons on Ayaneo, Mysten and similar
///   handhelds to F13-F18, which is how hhd sees them at all.
/// - `hhd-net.hadess.PowerProfiles.conf` is the D-Bus policy that lets root own
///   the `net.hadess.PowerProfiles` name on the system bus. hhd's `adjustor`
///   claims that name (`src/adjustor/drivers/gpu/ppd.py`); without the policy
///   dbus denies the request and TDP / power-profile switching does not work.
///
/// Copied from hhd-dev/hhd at a8bd8be (2026-09-02). They change only when new
/// hardware appears, but they are copies: check upstream when a new handheld's
/// buttons do not register.
const HHD_DATA_FILES: &[(&str, &str)] = &[
    (
        "etc/udev/rules.d/83-hhd.rules",
        include_str!("../resources/hhd/83-hhd.rules"),
    ),
    (
        "etc/udev/hwdb.d/83-hhd.hwdb",
        include_str!("../resources/hhd/83-hhd.hwdb"),
    ),
    (
        "usr/share/dbus-1/system.d/hhd-net.hadess.PowerProfiles.conf",
        include_str!("../resources/hhd/hhd-net.hadess.PowerProfiles.conf"),
    ),
];

const HHD_PATCH_HELPER: &str = include_str!("../resources/patches/deploytix-hhd-patch.sh");

/// Patches shipped into `HHD_PATCH_DIR`, as `(filename, contents)`.
///
/// `hhd-legion-go-2-touchpad.patch` — hhd-dev/hhd#340. On a Legion Go 2
/// (`17ef:61eb`) driven by the in-kernel `hid-lenovo-go` driver, hhd's
/// touchpad definition matched on `BTN_MOUSE`, but that driver presents the
/// touchpad as a real touchpad reporting `BTN_TOUCH` and puts `BTN_MOUSE` on a
/// separate "... Mouse" node the name pattern excludes. Nothing matched, and
/// `required=True` turned that into a `RuntimeError` and an endless
/// "Assuming controllers disconnected, restarting after 3s" loop that took the
/// whole controller — sticks, buttons, gyro — down with the touchpad. The
/// patch accepts either presentation, maps `BTN_TOUCH` to `touchpad_touch`
/// (the Go 2 node has no `BTN_TOOL_FINGER`, so matching alone would leave the
/// touchpad silent), and drops `required` so a miss degrades instead of
/// crash-looping.
///
/// Remove this entry once the fix is in the `hhd-git` build; until then the
/// helper's dry-run guard makes carrying it harmless either way.
const HHD_PATCHES: &[(&str, &str)] = &[(
    "hhd-legion-go-2-touchpad.patch",
    include_str!("../resources/patches/hhd-legion-go-2-touchpad.patch"),
)];

/// Where patches and the helper land in the target.
const HHD_PATCH_DIR: &str = "usr/share/deploytix/patches";
/// Installed path of the re-apply helper.
const HHD_PATCH_HELPER_PATH: &str = "usr/bin/deploytix-hhd-patch";

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
        for (name, _) in HHD_PATCHES {
            println!("  [dry-run] Would install /{HHD_PATCH_DIR}/{name}");
        }
        println!("  [dry-run] Would install /{HHD_PATCH_HELPER_PATH} and run it");
        println!("  [dry-run] Would write /etc/modules-load.d/hhd.conf (uhid)");
        for (dest, _) in HHD_DATA_FILES {
            println!("  [dry-run] Would write /{dest}");
        }
        println!("  [dry-run] Would run `udevadm hwdb --update`");
        println!(
            "  [dry-run] Would write HHD service file for init: {}",
            config.system.init
        );
        return Ok(());
    }

    // Step 1: Install AUR packages via yay
    let pkgs: Vec<String> = HHD_AUR_PACKAGES.iter().map(|s| s.to_string()).collect();
    yay_install_chroot_reviewed(
        cmd,
        install_root,
        username,
        "AUR: Handheld Daemon (hhd-git)",
        pkgs,
    )?;
    info!("  HHD AUR packages installed");

    // Step 1.5: Apply deploytix's local patches to the freshly installed hhd.
    apply_hhd_patches(cmd, install_root)?;

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

    // Step 2.5: Install the upstream data files no package ships.
    for (dest, contents) in HHD_DATA_FILES {
        let path = format!("{install_root}/{dest}");
        if let Some(parent) = std::path::Path::new(&path).parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, contents)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
        info!("  Written /{}", dest);
    }

    // A hwdb file does nothing until it is compiled into the binary database.
    // Best-effort: eudev and systemd both provide `udevadm hwdb --update`, but
    // a missing one must not fail an otherwise complete install.
    if let Err(e) = cmd.run_in_chroot(install_root, "udevadm hwdb --update") {
        warn!(
            "  Could not rebuild the udev hardware database ({}); run \
             `sudo udevadm hwdb --update` on the target if handheld buttons \
             do not register",
            e
        );
    }

    // Step 3: Write init-specific service file
    write_hhd_service(config, install_root, username)?;

    info!("HHD installation complete");
    Ok(())
}

/// Install [`HHD_PATCHES`] plus the re-apply helper into the target, then run
/// the helper once against the just-installed `hhd-git`.
///
/// Best-effort throughout: a patch that no longer applies is skipped by the
/// helper, and a failure to run it is logged rather than propagated. Shipping a
/// handheld with a working controller is the point, but an unpatchable hhd is a
/// reason to warn, not to fail an otherwise complete install.
///
/// The patches are written to the target as well as applied so the user can
/// re-run `deploytix-hhd-patch` after any rebuild of `hhd-git`, which — being
/// an AUR package whose files pacman owns — reverts them.
fn apply_hhd_patches(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    if HHD_PATCHES.is_empty() {
        return Ok(());
    }
    info!("  Installing deploytix's local hhd patches");

    let patch_dir = format!("{install_root}/{HHD_PATCH_DIR}");
    fs::create_dir_all(&patch_dir)?;
    for (name, contents) in HHD_PATCHES {
        let path = format!("{patch_dir}/{name}");
        fs::write(&path, contents)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    }

    let helper = format!("{install_root}/{HHD_PATCH_HELPER_PATH}");
    fs::write(&helper, HHD_PATCH_HELPER)?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;

    match cmd.run_in_chroot(install_root, &format!("/{HHD_PATCH_HELPER_PATH}")) {
        Ok(_) => info!("  hhd patches applied"),
        Err(e) => warn!(
            "  Could not apply hhd patches ({}); run `sudo deploytix-hhd-patch` \
             on the target to retry",
            e
        ),
    }
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
            // Admin-defined s6-rc services live in /etc/s6/adminsv
            // (package-provided ones ship in /etc/s6/sv).
            let sv_dir = format!("{}/etc/s6/adminsv/hhd", install_root);
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

            info!("  Written s6 service: /etc/s6/adminsv/hhd/");
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
/// `decky-loader-bin` AUR package, then bootstrap the user's homebrew
/// directory and write an init-specific service file.
///
/// Layout created on the target system (uses the canonical Decky /
/// SteamOS path `~/homebrew`, NOT the AUR package's
/// `~/.local/var/opt/decky-loader` default — the upstream PluginLoader
/// expects `HOMEBREW_FOLDER=~/homebrew` and every Decky plugin / tutorial
/// assumes that layout).  Because the AUR-shipped `decky-loader-helper`
/// hardcodes its destination to `~/.local/var/opt/decky-loader`, we
/// bypass it and copy `PluginLoader` into place ourselves.
/// ```text
/// /usr/lib/decky-loader/PluginLoader               (AUR package file)
/// /home/{user}/homebrew/
///   services/
///     PluginLoader          (copied from /usr/lib/decky-loader)
///     .loader.version       (version tag we write from pacman -Q)
///   plugins/
/// ~/.local/share/Steam/.cef-enable-remote-debugging
/// ~/.steam/steam -> ~/.local/share/Steam  (symlink)
/// ```
///
/// The init service runs `PluginLoader` **as root**, matching upstream's
/// `dist/plugin_loader-release.service` (`User=root`) and the AUR package.
/// Decky is built for that: its platform layer distinguishes the *effective*
/// user from an *unprivileged* user, shells out to `chown -R` (including
/// `chown root:root` for privileged paths), and `CHOWN_PLUGIN_PATH` is on by
/// default. `UNPRIVILEGED_USER` names the account whose `~/homebrew` this is,
/// so Decky chowns plugins to the right owner rather than inferring it from
/// the path — its fallback when it cannot is the literal string `deck`.
///
/// deploytix previously dropped to the session user here. That is not a
/// configuration upstream ships, and it left Decky unreachable in Game Mode.
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
    let decky_data = format!("/home/{}/homebrew", username);

    info!("Installing Decky Loader for user {}", username);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install decky-loader-bin via yay as {}",
            username
        );
        println!(
            "  [dry-run] Would bootstrap {} (services/PluginLoader, plugins/, .loader.version)",
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
    yay_install_chroot_reviewed(
        cmd,
        install_root,
        username,
        "AUR: Decky Loader (decky-loader-bin)",
        vec!["decky-loader-bin".to_string()],
    )?;
    info!("  decky-loader-bin installed");

    // Step 2: Enable Steam CEF remote debugging (required by Decky's frontend).
    //
    // Steam's first-run bootstrap creates ~/.steam/steam as a symlink
    // pointing to ~/.local/share/Steam.  We must NOT create it as a
    // real directory (fs::create_dir_all) or Steam can't initialise.
    // Instead: create the real data dir, set up the symlink, and write
    // the CEF flag into the real directory.
    let steam_data_dir = format!("{}/home/{}/.local/share/Steam", install_root, username);
    fs::create_dir_all(&steam_data_dir)?;
    fs::write(format!("{steam_data_dir}/.cef-enable-remote-debugging"), "")?;

    // Create ~/.steam/ and symlink ~/.steam/steam -> ~/.local/share/Steam
    let dot_steam_dir = format!("{}/home/{}/.steam", install_root, username);
    fs::create_dir_all(&dot_steam_dir)?;
    let steam_symlink = format!("{}/steam", dot_steam_dir);
    let symlink_path = std::path::Path::new(&steam_symlink);
    if !symlink_path.exists() && symlink_path.read_link().is_err() {
        std::os::unix::fs::symlink(
            format!("/home/{}/.local/share/Steam", username),
            symlink_path,
        )?;
    }

    let flatpak_steam = format!(
        "{}/home/{}/.var/app/com.valvesoftware.Steam/data/Steam",
        install_root, username
    );
    if std::path::Path::new(&flatpak_steam).exists() {
        fs::write(format!("{flatpak_steam}/.cef-enable-remote-debugging"), "")?;
    }
    info!("  Enabled Steam CEF remote debugging");

    // Step 3: Bootstrap the user's homebrew directory manually.
    //
    // We deliberately do NOT call /usr/bin/decky-loader-helper — the AUR
    // helper hardcodes ~/.local/var/opt/decky-loader as its destination,
    // which conflicts with the canonical ~/homebrew path that upstream
    // PluginLoader and every Decky plugin assume.  Instead we replicate
    // what the helper does (create services/ + plugins/ owned by the
    // user, copy PluginLoader, write the .loader.version tag) but at
    // ~/homebrew.
    let bootstrap_cmd = format!(
        "set -e; \
         DECKY_VER=$(pacman -Q decky-loader-bin | awk '{{print $2}}' | sed 's/-[0-9]*$//'); \
         install -dm 755 -o {user} -g {user} {data} {data}/services {data}/plugins; \
         install -m 755 -o {user} -g {user} \
           /usr/lib/decky-loader/PluginLoader {data}/services/PluginLoader; \
         printf 'v%s' \"${{DECKY_VER}}\" > {data}/services/.loader.version; \
         chown {user}:{user} {data}/services/.loader.version",
        user = username,
        data = decky_data,
    );
    cmd.run_in_chroot(install_root, &bootstrap_cmd)?;
    info!("  Bootstrapped Decky homebrew directory at {}", decky_data);

    // Step 4: Write init-specific service file
    write_decky_service(config, install_root, username, &decky_data)?;

    // Step 5: Ensure ownership under the user's home stays correct.
    // Bootstrap step above already chowns ~/homebrew; .local and .steam
    // were created by us as root, so chown them here.
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
/// Decky runs as root with `HOMEBREW_FOLDER` pointing at `~/homebrew`, the
/// canonical Decky / SteamOS layout. `UNPRIVILEGED_PATH` and `PRIVILEGED_PATH`
/// are what the loader actually reads (`localplatformlinux.py`); upstream sets
/// both to the same directory, and so do we. `UNPRIVILEGED_USER` is set so the
/// loader does not have to derive the owner from the path.
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

            // Runs as root, matching upstream's plugin_loader-release.service
            // (`User=root`) and the AUR package. UNPRIVILEGED_USER names the
            // account whose ~/homebrew this is, so Decky chowns plugins to the
            // right owner instead of guessing.
            let run_script = format!(
                "#!/bin/sh\n\
                 exec 2>&1\n\
                 export HOMEBREW_FOLDER={data}\n\
                 export UNPRIVILEGED_PATH={data}\n\
                 export PRIVILEGED_PATH={data}\n\
                 export LOG_LEVEL=INFO\n\
                 export UNPRIVILEGED_USER={user}\n\
                 export HOME=/home/{user}\n\
                 cd {wd}\n\
                 exec {pl}\n",
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

            // No command_user: PluginLoader runs as root, matching upstream.
            let script = format!(
                "#!/sbin/openrc-run\n\
                 description=\"SteamDeck Plugin Loader\"\n\
                 command=\"{pl}\"\n\
                 command_background=true\n\
                 directory=\"{wd}\"\n\
                 pidfile=\"/run/plugin_loader.pid\"\n\
                 \n\
                 export HOMEBREW_FOLDER={data}\n\
                 export UNPRIVILEGED_PATH={data}\n\
                 export PRIVILEGED_PATH={data}\n\
                 export LOG_LEVEL=INFO\n\
                 export UNPRIVILEGED_USER={user}\n\
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
            let sv_dir = format!("{}/etc/s6/adminsv/plugin_loader", install_root);
            fs::create_dir_all(&sv_dir)?;

            fs::write(format!("{}/type", sv_dir), "longrun\n")?;

            // Runs as root (no s6-setuidgid), matching upstream.
            let run_script = format!(
                "#!/bin/sh\n\
                 export HOMEBREW_FOLDER={data}\n\
                 export UNPRIVILEGED_PATH={data}\n\
                 export PRIVILEGED_PATH={data}\n\
                 export LOG_LEVEL=INFO\n\
                 export UNPRIVILEGED_USER={user}\n\
                 export HOME=/home/{user}\n\
                 cd {wd}\n\
                 exec {pl} 2>&1\n",
                data = decky_data,
                user = username,
                wd = working_dir,
                pl = plugin_loader
            );
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, &run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written s6 service: /etc/s6/adminsv/plugin_loader/");
        }

        InitSystem::Dinit => {
            let dinit_d = format!("{}/etc/dinit.d", install_root);
            fs::create_dir_all(&dinit_d)?;

            let env_content = format!(
                "HOMEBREW_FOLDER={data}\n\
                 UNPRIVILEGED_PATH={data}\n\
                 PRIVILEGED_PATH={data}\n\
                 LOG_LEVEL=INFO\n\
                 UNPRIVILEGED_USER={user}\n\
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
                 env-file = /etc/dinit.d/plugin_loader.env\n\
                 restart = true\n",
                pl = plugin_loader,
                wd = working_dir,
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
    let pkgs: Vec<String> = EVDEVHOOK2_AUR_PACKAGES
        .iter()
        .map(|s| s.to_string())
        .collect();
    yay_install_chroot_reviewed(cmd, install_root, username, "AUR: evdevhook2-git", pkgs)?;
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
            let sv_dir = format!("{}/etc/s6/adminsv/evdevhook2", install_root);
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

            info!("  Written s6 service: /etc/s6/adminsv/evdevhook2/");
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

#[cfg(test)]
mod hhd_patch_tests {
    use super::*;

    /// hhd-dev/hhd#340: on a Legion Go 2 the touchpad node reports BTN_TOUCH,
    /// not BTN_MOUSE, so hhd matched nothing and — with required=True — restarted
    /// the controller every 3s forever.
    #[test]
    fn legion_go_2_touchpad_patch_is_shipped() {
        let (name, body) = HHD_PATCHES
            .iter()
            .find(|(n, _)| n.contains("legion-go-2-touchpad"))
            .expect("the Legion Go 2 touchpad patch must be shipped");
        assert!(name.ends_with(".patch"));
        // All three halves of the fix: match either presentation, actually
        // report a touch, and degrade instead of crash-looping.
        assert!(body.contains(r#"EC("BTN_MOUSE"), EC("BTN_TOUCH")"#));
        assert!(body.contains(r#"B("BTN_TOOL_FINGER"), B("BTN_TOUCH")"#));
        assert!(body.contains("+        required=False,"));
        assert!(body.contains("-        required=True,"));
    }

    /// The helper globs `hhd-*.patch`, so a name that does not match would be
    /// silently ignored — installed, never applied.
    #[test]
    fn every_patch_matches_the_helper_glob() {
        for (name, _) in HHD_PATCHES {
            assert!(
                name.starts_with("hhd-") && name.ends_with(".patch"),
                "{name} would not be picked up by the helper's hhd-*.patch glob"
            );
        }
    }

    /// Patches are diffed against the upstream source tree (a/src/hhd/...) but
    /// applied to <site-packages>/hhd/..., so the strip level must be 2.
    #[test]
    fn patches_are_source_tree_relative_and_helper_strips_to_match() {
        for (name, body) in HHD_PATCHES {
            assert!(
                body.contains("--- a/src/hhd/"),
                "{name} is not relative to the upstream source tree"
            );
        }
        assert!(HHD_PATCH_HELPER.contains("patch -p2"));
    }

    /// A rebuild of the AUR package reverts the patches, and the helper is the
    /// only way back. It must be installed, executable, and self-disabling.
    #[test]
    fn helper_is_dry_run_guarded_and_never_fatal() {
        // Dry run before every apply: an already-applied or upstream-fixed
        // patch must be skipped rather than forced.
        assert!(HHD_PATCH_HELPER.contains("--forward --dry-run"));
        // Never a gate on the install.
        assert!(HHD_PATCH_HELPER.trim_end().ends_with("exit 0"));
        // Stale bytecode would shadow the patched sources.
        assert!(HHD_PATCH_HELPER.contains("__pycache__"));
    }

    #[test]
    fn helper_is_valid_shell() {
        if let Ok(status) = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(HHD_PATCH_HELPER)
            .status()
        {
            assert!(status.success(), "the hhd patch helper is not valid shell");
        }
    }

    /// The helper is invoked by absolute path inside the chroot, so the path it
    /// is written to and the path it is run from must not drift apart.
    #[test]
    fn helper_install_path_is_on_the_default_path() {
        assert_eq!(HHD_PATCH_HELPER_PATH, "usr/bin/deploytix-hhd-patch");
        assert!(HHD_PATCH_HELPER.contains(HHD_PATCH_DIR));
    }
}

#[cfg(test)]
mod steam_bootstrap_tests {
    use super::*;

    #[test]
    fn script_targets_the_users_steam_directory() {
        let script = steam_bootstrap_script("gamer");
        assert!(script.contains("STEAMDIR=/home/gamer/.local/share/Steam"));
        assert!(script.contains("/usr/lib/steam/bootstraplinux_ubuntu12_32.tar.xz"));
        assert!(script.contains("chown -R gamer:gamer /home/gamer/.local"));
    }

    /// Re-running the installer must not re-extract over live Steam state,
    /// and a missing tarball must not fail the install.
    #[test]
    fn script_is_idempotent_and_tolerates_a_missing_tarball() {
        let script = steam_bootstrap_script("gamer");
        assert!(script.contains(".deploytix-bootstrap-seeded"));
        assert!(script.contains(r#"if [ -f "$MARKER" ]"#));
        assert!(script.contains(r#"if [ ! -f "$BOOTSTRAP" ]"#));
        // No `set -e`: extraction failure logs and continues.
        assert!(!script.contains("set -e"));
    }

    fn assert_valid_shell(script: &str) {
        if let Ok(status) = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(script)
            .status()
        {
            assert!(status.success(), "not valid shell:\n{script}");
        }
    }

    #[test]
    fn bootstrap_script_is_valid_shell() {
        assert_valid_shell(&steam_bootstrap_script("gamer"));
    }

    /// The seed unpacks only the launcher. What Game Mode actually needs —
    /// steamwebhelper and steamui.so — arrives with the client download, which
    /// is what this prefetch front-loads; the probe must therefore test for the
    /// client, not for anything the tarball already provides.
    #[test]
    fn prefetch_probes_for_the_client_and_runs_as_the_user() {
        let script = steam_prefetch_script("gamer");
        assert!(script.contains("STEAMDIR=/home/gamer/.local/share/Steam"));
        assert!(script.contains("ubuntu12_64/steamui.so"));
        assert!(!script.contains("ubuntu12_32"));
        // makepkg-style: never as root, and never without a display.
        assert!(script.contains("su -s /bin/sh gamer -c"));
        assert!(script.contains("xvfb-run"));
        // `+quit` is what makes the run terminate once the update is done.
        assert!(script.contains("+quit"));
        assert!(script.contains("chown -R gamer:gamer /home/gamer/.local"));
        assert_valid_shell(&script);
    }

    use crate::config::InitSystem;

    fn test_root(tag: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "deploytix-decky-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn decky_config() -> DeploymentConfig {
        DeploymentConfig::sample()
    }

    /// Read back whichever file `write_decky_service` produced for `init`.
    fn decky_unit_text(root: &str, init: &InitSystem) -> String {
        let path = match init {
            InitSystem::Runit => format!("{root}/etc/runit/sv/plugin_loader/run"),
            InitSystem::OpenRC => format!("{root}/etc/init.d/plugin_loader"),
            InitSystem::S6 => format!("{root}/etc/s6/adminsv/plugin_loader/run"),
            InitSystem::Dinit => format!("{root}/etc/dinit.d/plugin_loader"),
        };
        let unit = std::fs::read_to_string(&path).unwrap_or_default();
        // dinit keeps its environment in a sidecar file.
        let env = std::fs::read_to_string(format!("{root}/etc/dinit.d/plugin_loader.env"))
            .unwrap_or_default();
        format!("{unit}\n{env}")
    }

    /// Upstream's plugin_loader-release.service is `User=root`, and the AUR
    /// package keeps it. Decky's platform layer is written for that: it
    /// separates the effective user from the unprivileged user and chowns
    /// between them. Dropping privileges here is not a configuration upstream
    /// ships, and it left Decky unreachable in Game Mode.
    #[test]
    fn decky_runs_as_root_on_every_init() {
        let config = decky_config();
        for init in [
            InitSystem::Runit,
            InitSystem::OpenRC,
            InitSystem::S6,
            InitSystem::Dinit,
        ] {
            let root = test_root(&format!("decky-{init:?}"));
            let mut cfg = config.clone();
            cfg.system.init = init.clone();
            write_decky_service(&cfg, &root, "gamer", "/home/gamer/homebrew").unwrap();

            let unit = decky_unit_text(&root, &init);
            for dropper in ["chpst -u", "s6-setuidgid", "run-as =", "command_user"] {
                assert!(
                    !unit.contains(dropper),
                    "{init:?} still drops privileges via `{dropper}`:\n{unit}"
                );
            }
            // And the loader is told whose homebrew this is, so it never has
            // to fall back to guessing (its default guess is "deck").
            assert!(
                unit.contains("UNPRIVILEGED_USER=gamer"),
                "{init:?} must set UNPRIVILEGED_USER:\n{unit}"
            );
            // Both paths upstream sets must still point at ~/homebrew.
            assert!(unit.contains("UNPRIVILEGED_PATH=/home/gamer/homebrew"));
            assert!(unit.contains("PRIVILEGED_PATH=/home/gamer/homebrew"));

            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// `hhd-git` runs `python -m installer` on a wheel whose pyproject builds
    /// only `where = ["src"]`, so the repository's `usr/` tree is not in it.
    /// `83-hhd.rules` is packaged only by `hhd-systemd-git`, which exists to
    /// ship a systemd unit — so on Artix deploytix has to ship the rules
    /// itself rather than pull in a systemd package for them.
    #[test]
    fn hhd_installs_no_systemd_package_and_ships_the_rules_itself() {
        assert!(HHD_AUR_PACKAGES.contains(&"hhd-git"));
        assert!(
            !HHD_AUR_PACKAGES.contains(&"hhd-systemd-git"),
            "this system has no systemd; the rules are shipped directly instead"
        );
        assert!(
            !HHD_AUR_PACKAGES.iter().any(|p| p.contains("systemd")),
            "no systemd package belongs in an Artix install"
        );
        assert!(
            HHD_DATA_FILES
                .iter()
                .any(|(d, _)| *d == "etc/udev/rules.d/83-hhd.rules"),
            "dropping hhd-systemd-git must not drop the udev rules with it"
        );
        // hhd-ui is the overlay itself, not an optional browser UI: hhd's
        // find_overlay_exe looks for it by name and logs "Failed to start
        // hhd-ui" without it, leaving a running daemon with no reachable UI.
        assert!(
            HHD_AUR_PACKAGES.contains(&"hhd-ui"),
            "hhd-ui renders the Game Mode overlay"
        );
    }

    /// The rules are the half that makes controllers work at all, so a copy
    /// that lost its substance would be worse than no copy.
    #[test]
    fn the_shipped_udev_rules_still_carry_the_device_quirks() {
        let (_, rules) = HHD_DATA_FILES
            .iter()
            .find(|(d, _)| d.ends_with("83-hhd.rules"))
            .expect("rules are shipped");
        // Steam reading the raw controllers.
        assert!(rules.contains("uaccess"));
        // xpad binding for the handhelds that need it.
        assert!(rules.contains("xpad"));
        // The Ally HID devices that crash SDL and Proton controller handlers.
        assert!(rules.contains("0b05"));
    }

    /// Two upstream files that no package installs at all. Without the hwdb the
    /// extra handheld buttons never reach hhd; without the D-Bus policy, root
    /// cannot own `net.hadess.PowerProfiles` and TDP switching fails.
    #[test]
    fn hhd_ships_the_data_files_no_package_provides() {
        let dests: Vec<&str> = HHD_DATA_FILES.iter().map(|(d, _)| *d).collect();
        assert!(dests.contains(&"etc/udev/hwdb.d/83-hhd.hwdb"));
        assert!(dests.contains(&"usr/share/dbus-1/system.d/hhd-net.hadess.PowerProfiles.conf"));

        for (dest, contents) in HHD_DATA_FILES {
            assert!(!contents.is_empty(), "{dest} is empty");
            assert!(
                !dest.starts_with('/'),
                "{dest} must be install-root relative"
            );
        }

        // The policy has to allow root to *own* the name, not merely talk to
        // it — hhd runs as root and is the one claiming it.
        let (_, policy) = HHD_DATA_FILES
            .iter()
            .find(|(d, _)| d.ends_with("PowerProfiles.conf"))
            .unwrap();
        assert!(policy.contains(r#"<allow own="net.hadess.PowerProfiles"/>"#));
        assert!(policy.contains(r#"<policy user="root">"#));
    }

    /// Zen Browser used to be installed unconditionally alongside yay, so every
    /// install that wanted an AUR helper also got a browser it had not asked
    /// for. It is an option now, and skipping it must be the default.
    #[test]
    fn zen_browser_is_opt_in_and_needs_yay() {
        let cmd = CommandRunner::new(true);
        let mut cfg = DeploymentConfig::sample();

        cfg.packages.install_zen_browser = false;
        cfg.packages.install_yay = true;
        assert!(install_zen_browser(&cmd, &cfg, "/mnt").is_ok());

        // Asked for without a helper to build it: skipped, not fatal.
        cfg.packages.install_zen_browser = true;
        cfg.packages.install_yay = false;
        assert!(install_zen_browser(&cmd, &cfg, "/mnt").is_ok());

        cfg.packages.install_yay = true;
        assert!(install_zen_browser(&cmd, &cfg, "/mnt").is_ok());
    }

    /// A fresh config must not opt anyone into either of the optional extras.
    #[test]
    fn the_optional_extras_default_to_off() {
        let cfg = DeploymentConfig::sample();
        assert!(!cfg.packages.install_zen_browser);
        assert!(!cfg.packages.install_warp_terminal);
    }

    /// The URL is a constant, and it is single-quoted in the script, so what
    /// keeps that safe is the constant never containing a quote. If someone
    /// ever edits it to one that does, this fails rather than the shell
    /// silently running the tail of it.
    #[test]
    fn warp_url_is_safe_to_shell_quote() {
        let url = crate::config::WARP_TERMINAL_URL;
        assert!(url.starts_with("https://"), "{url}");
        assert!(
            !url.chars()
                .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '\'' | '"' | '\\')),
            "{url} cannot be single-quoted safely"
        );
    }

    #[test]
    fn warp_script_downloads_then_installs() {
        let script = warp_terminal_script();
        // -f so an HTTP error is a failure rather than an error page saved as
        // a package; -L because the vendor URL is a redirect.
        assert!(script.contains("curl -fL"));
        assert!(script.contains(crate::config::WARP_TERMINAL_URL));
        assert!(script.contains("pacman -U --noconfirm"));
        let download = script.find("curl").expect("downloads");
        let install = script.find("pacman -U").expect("installs");
        assert!(download < install, "download must precede install");
        // The package is not left sitting in the image afterwards.
        assert!(script.contains("rm -f"));
        assert_valid_shell(&script);
    }

    /// `/get_warp` is the marketing page and answers `200 text/html`, so `-f`
    /// cannot tell it apart from a package. Pinning the endpoint is what keeps
    /// the download an actual package rather than a saved landing page.
    #[test]
    fn warp_url_is_the_download_endpoint_not_the_landing_page() {
        let url = crate::config::WARP_TERMINAL_URL;
        assert!(
            url.contains("/download"),
            "{url} must be the download endpoint"
        );
        assert!(
            !url.contains("get_warp"),
            "{url} is the landing page, which serves HTML with a 200"
        );
        assert!(url.contains("package=pacman"), "{url} must ask for pacman");
    }

    /// The guard that makes a wrong URL loud. Without it a served web page
    /// reaches `pacman -U` and comes back as "invalid or corrupted package" on
    /// a step that is best-effort and therefore shrugs it off.
    #[test]
    fn warp_script_rejects_a_download_that_is_not_a_package() {
        let script = warp_terminal_script();
        assert!(script.contains(ZSTD_MAGIC_HEX), "checks the zstd magic");
        let check = script.find(ZSTD_MAGIC_HEX).expect("checks");
        let install = script.find("pacman -U").expect("installs");
        assert!(check < install, "the check must precede the install");
        assert!(script.contains("exit 1"), "a bad download fails the step");
    }

    // ── linux-tkg kernel ───────────────────────────────────────────────

    /// Run one of the script's `grep -o` patterns against a line, the same way
    /// the script does, and return what it matched. Testing the pattern by
    /// eye is not enough: the kernel and headers asset names share a prefix,
    /// and the whole correctness of the resolve rests on telling them apart.
    fn grep_o(pattern: &str, input: &str) -> Vec<String> {
        use std::io::Write;
        let mut child = std::process::Command::new("grep")
            .arg("-o")
            .arg(pattern)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("grep");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(input.as_bytes())
            .expect("write");
        let out = child.wait_with_output().expect("wait");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A slice of the real releases API payload: both assets for one
    /// scheduler, plus a neighbouring scheduler's, all on one line the way the
    /// unformatted JSON arrives.
    const TKG_API_SAMPLE: &str = "\"browser_download_url\":\
        \"https://github.com/Frogging-Family/linux-tkg/releases/download/v7.2.3/\
        linux72-tkg-bore-llvm-7.2.3-273-x86_64.pkg.tar.zst\",\
        \"browser_download_url\":\
        \"https://github.com/Frogging-Family/linux-tkg/releases/download/v7.2.3/\
        linux72-tkg-bore-llvm-headers-7.2.3-273-x86_64.pkg.tar.zst\",\
        \"browser_download_url\":\
        \"https://github.com/Frogging-Family/linux-tkg/releases/download/v7.2.3/\
        linux72-tkg-pds-llvm-7.2.3-273-x86_64.pkg.tar.zst\"";

    /// The subtle one. `linux72-tkg-bore-llvm-headers-…` starts with every
    /// character of the kernel pattern's prefix, so a pattern that stops at
    /// `-llvm-` matches both assets and the resolve installs the headers
    /// package twice — with no kernel, on a config that has no linux-zen to
    /// fall back to.
    #[test]
    fn the_kernel_pattern_does_not_also_match_the_headers_asset() {
        let kernel = grep_o(
            &tkg_asset_pattern(TkgScheduler::Bore, false),
            TKG_API_SAMPLE,
        );
        assert_eq!(kernel.len(), 1, "matched {kernel:?}");
        assert!(kernel[0].ends_with("linux72-tkg-bore-llvm-7.2.3-273-x86_64.pkg.tar.zst"));
        assert!(!kernel[0].contains("headers"));

        let headers = grep_o(&tkg_asset_pattern(TkgScheduler::Bore, true), TKG_API_SAMPLE);
        assert_eq!(headers.len(), 1, "matched {headers:?}");
        assert!(headers[0].ends_with("linux72-tkg-bore-llvm-headers-7.2.3-273-x86_64.pkg.tar.zst"));
    }

    /// The scheduler token is the only thing separating five otherwise
    /// identical asset names, so picking one must not drag in another's.
    #[test]
    fn the_pattern_selects_only_the_chosen_scheduler() {
        let pds = grep_o(&tkg_asset_pattern(TkgScheduler::Pds, false), TKG_API_SAMPLE);
        assert_eq!(pds.len(), 1, "matched {pds:?}");
        assert!(pds[0].contains("-tkg-pds-llvm-"));

        // A scheduler with no asset in the payload resolves to nothing, which
        // is what sends the script to the pinned fallback.
        let muqss = grep_o(
            &tkg_asset_pattern(TkgScheduler::Muqss, false),
            TKG_API_SAMPLE,
        );
        assert!(muqss.is_empty(), "matched {muqss:?}");
    }

    /// The series prefix tracks the kernel version (`linux72` today), so
    /// pinning it as a literal would break on the next release.
    #[test]
    fn the_pattern_is_not_pinned_to_one_kernel_series() {
        let next = "\"https://github.com/Frogging-Family/linux-tkg/releases/download/v7.3.0/\
                    linux73-tkg-bore-llvm-7.3.0-1-x86_64.pkg.tar.zst\"";
        let found = grep_o(&tkg_asset_pattern(TkgScheduler::Bore, false), next);
        assert_eq!(found.len(), 1, "matched {found:?}");
        assert!(found[0].contains("linux73"));
    }

    #[test]
    fn tkg_script_resolves_then_downloads_then_installs() {
        let script = tkg_kernel_script(TkgScheduler::Bore);
        assert!(script.contains(crate::config::TKG_RELEASES_API), "resolves");
        assert!(script.contains("curl -fL"));
        // Both packages go in as one transaction, so the headers can never be
        // paired with a different build than the kernel they describe.
        assert!(script.contains("pacman -U --noconfirm '/var/cache/deploytix/linux-tkg.pkg.tar.zst' '/var/cache/deploytix/linux-tkg-headers.pkg.tar.zst'"));
        let resolve = script
            .find(crate::config::TKG_RELEASES_API)
            .expect("resolves");
        let download = script.find("curl -fL").expect("downloads");
        let install = script.find("pacman -U").expect("installs");
        assert!(resolve < download && download < install);
        assert!(
            script.contains("rm -f"),
            "packages are not left in the image"
        );
        assert_valid_shell(&script);
    }

    /// Losing the API — it is unauthenticated, and 60 requests/hour is easy to
    /// exhaust on a shared address — must cost a slightly older kernel, not
    /// the whole install, because there is no linux-zen behind it.
    #[test]
    fn tkg_script_falls_back_to_the_pinned_build() {
        let script = tkg_kernel_script(TkgScheduler::Bore);
        let (kernel, headers) = crate::config::tkg_fallback_urls(TkgScheduler::Bore);
        assert!(script.contains(&kernel), "pins a kernel fallback");
        assert!(script.contains(&headers), "pins a headers fallback");
    }

    #[test]
    fn tkg_script_rejects_a_download_that_is_not_a_package() {
        let script = tkg_kernel_script(TkgScheduler::Bore);
        assert!(script.contains(ZSTD_MAGIC_HEX), "checks the zstd magic");
        let check = script.find(ZSTD_MAGIC_HEX).expect("checks");
        let install = script.find("pacman -U").expect("installs");
        assert!(check < install, "the check must precede the install");
        assert!(script.contains("exit 1"), "a bad download fails the step");
    }

    /// Both URLs are built from constants and a closed enum, never from user
    /// input, which is what makes single-quoting them into the script safe.
    #[test]
    fn tkg_fallback_urls_are_safe_to_shell_quote() {
        for sched in TkgScheduler::all() {
            let (kernel, headers) = crate::config::tkg_fallback_urls(*sched);
            for url in [&kernel, &headers] {
                assert!(url.starts_with("https://"), "{url}");
                assert!(url.ends_with("-x86_64.pkg.tar.zst"), "{url}");
                assert!(
                    url.contains(&format!("-tkg-{}-llvm", sched.as_str())),
                    "{url}"
                );
                assert!(
                    !url.chars().any(|c| c.is_control()
                        || c.is_whitespace()
                        || matches!(c, '\'' | '"' | '\\')),
                    "{url} cannot be single-quoted safely"
                );
            }
            assert!(headers.contains("-llvm-headers-"), "{headers}");
            assert!(!kernel.contains("headers"), "{kernel}");
        }
    }

    /// The prebuilt `nvidia` module is built against a stock kernel's ABI and
    /// will not load on linux-tkg, so selecting the kernel has to switch the
    /// driver to DKMS or the machine boots without a GPU driver.
    #[test]
    fn nvidia_becomes_dkms_on_the_tkg_kernel() {
        assert!(NVIDIA_PACKAGES.contains(&"nvidia"));
        assert!(!NVIDIA_PACKAGES.contains(&"nvidia-dkms"));
        assert!(NVIDIA_DKMS_PACKAGES.contains(&"nvidia-dkms"));
        assert!(!NVIDIA_DKMS_PACKAGES.contains(&"nvidia"));
        // DKMS builds the module on the target, so the tool has to be there.
        assert!(NVIDIA_DKMS_PACKAGES.contains(&"dkms"));
        // Userspace is kernel-independent and must not be dropped.
        assert!(NVIDIA_DKMS_PACKAGES.contains(&"nvidia-utils"));
    }

    /// `xorg-server-xvfb` provides `xvfb-run` but does not depend on `xauth`,
    /// which `xvfb-run` needs in order to start at all. Asking for only the
    /// first package buys a feature that never runs.
    #[test]
    fn the_prefetch_asks_for_everything_xvfb_run_needs() {
        assert!(STEAM_PREFETCH_PACKAGES.contains(&"xorg-server-xvfb"));
        assert!(STEAM_PREFETCH_PACKAGES.contains(&"xorg-xauth"));
    }

    /// A prefetch is an optimisation, never a reason to fail an install: it is
    /// skipped when the client is already there or Xvfb is missing, bounded by
    /// a timeout, and every failure path still exits 0.
    #[test]
    fn prefetch_is_bounded_and_never_fails_the_install() {
        let script = steam_prefetch_script("gamer");
        assert!(script.contains("skipping prefetch"));
        assert!(script.contains("command -v xvfb-run"));
        // xvfb-run shells out to xauth under its own `set -e`, so a missing
        // xauth stops the prefetch before Steam starts. The guard has to check
        // for it too, or the skip message would be a lie and the run would
        // silently do nothing.
        assert!(script.contains("command -v xauth"));
        assert!(script.contains(&format!("timeout {STEAM_PREFETCH_TIMEOUT_SECS}")));
        assert!(!script.contains("set -e"));
        assert!(script.trim_end().ends_with("exit 0"));
    }
}

#[cfg(test)]
mod audio_startup_tests {
    use super::AUDIO_STARTUP_SCRIPT;

    /// audio-startup runs on the boot -> Steam path (backgrounded by
    /// steam-gamescope-session, and as an XDG autostart entry in desktop
    /// sessions). It used to serialise three fixed sleeps totalling four
    /// seconds; the only real ordering constraint is that pipewire's core
    /// socket exists before its clients connect, so wait for the socket.
    #[test]
    fn audio_startup_waits_on_the_socket_not_the_clock() {
        assert!(
            AUDIO_STARTUP_SCRIPT.contains("wait_for_socket"),
            "audio-startup must poll for pipewire's socket"
        );
        assert!(
            AUDIO_STARTUP_SCRIPT.contains("pipewire-0"),
            "the readiness check must name pipewire's core socket"
        );

        for (n, line) in AUDIO_STARTUP_SCRIPT.lines().enumerate() {
            let code = line.trim();
            let Some(secs) = code.strip_prefix("sleep ") else {
                continue;
            };
            // Only the sub-second poll interval inside wait_for_socket is
            // allowed; anything measured in whole seconds is a fixed delay.
            let secs: f64 = secs.parse().expect("sleep takes a literal duration");
            assert!(
                secs < 0.5,
                "line {} sleeps {}s on the audio startup path",
                n + 1,
                secs
            );
        }
    }

    /// Both pipewire clients connect to the same socket, so neither has to
    /// wait for the other — starting them back to back keeps the tail short.
    #[test]
    fn pipewire_clients_start_after_the_socket_check() {
        let wait = AUDIO_STARTUP_SCRIPT
            .find("if ! wait_for_socket")
            .expect("audio-startup blocks on the socket check");
        let pulse = AUDIO_STARTUP_SCRIPT
            .find("start_if_missing pipewire-pulse")
            .expect("audio-startup starts pipewire-pulse");
        let wireplumber = AUDIO_STARTUP_SCRIPT
            .find("start_if_missing wireplumber")
            .expect("audio-startup starts wireplumber");

        assert!(wait < pulse && wait < wireplumber);
        assert!(
            !AUDIO_STARTUP_SCRIPT[pulse..wireplumber].contains("sleep"),
            "the two clients must not be serialised behind a sleep"
        );
    }
}
