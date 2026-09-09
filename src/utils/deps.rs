//! Host system dependency checking and installation

use crate::config::{Bootloader, Filesystem};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::collections::HashMap;
use std::process::Command;
use tracing::info;

/// The Artix toolchain packages installed for `basestrap`.
///
/// `artools` was split into three: `artools-base` carries `basestrap` and
/// `artix-chroot`, the two binaries deploytix actually calls; `artools-pkg`
/// covers package building and `artools-iso` ISO building. All three are
/// installed together — an installer host is expected to carry the full
/// toolchain, and the old single `artools` name no longer resolves.
const ARTOOLS_PACKAGES: &[&str] = &["artools-base", "artools-pkg", "artools-iso"];

/// Binary to package mapping for Artix/Arch.
///
/// A binary maps to a *list* of packages: one binary can require several (the
/// artools split), and the list is what reaches `pacman -S` as separate
/// arguments.
fn binary_to_package() -> HashMap<&'static str, &'static [&'static str]> {
    let mut map: HashMap<&'static str, &'static [&'static str]> = HashMap::new();
    // Core partitioning
    map.insert("sfdisk", &["util-linux"]);
    map.insert("mkswap", &["util-linux"]);
    map.insert("blkid", &["util-linux"]);

    // Filesystems
    map.insert("mkfs.vfat", &["dosfstools"]);
    map.insert("mkfs.ext4", &["e2fsprogs"]);
    map.insert("mkfs.btrfs", &["btrfs-progs"]);
    map.insert("mkfs.xfs", &["xfsprogs"]);
    map.insert("mkfs.f2fs", &["f2fs-tools"]);
    map.insert("zpool", &["zfs-utils"]);

    // Encryption
    map.insert("cryptsetup", &["cryptsetup"]);
    // dm-verity (LVM immutable A/B) — provided by cryptsetup
    map.insert("veritysetup", &["cryptsetup"]);

    // LVM
    map.insert("pvcreate", &["lvm2"]);
    map.insert("vgcreate", &["lvm2"]);
    map.insert("lvcreate", &["lvm2"]);

    // Bootloaders
    map.insert("grub-install", &["grub"]);
    map.insert("grub-mkconfig", &["grub"]);

    // Artix tools
    map.insert("basestrap", ARTOOLS_PACKAGES);

    map
}

/// Check if a binary exists in PATH
fn binary_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Determine required binaries based on configuration
pub fn required_binaries(
    filesystem: &Filesystem,
    boot_filesystem: &Filesystem,
    encryption: bool,
    use_lvm_thin: bool,
    bootloader: &Bootloader,
    needs_verity: bool,
) -> Vec<&'static str> {
    let mut bins = vec![
        "sfdisk",
        "mkswap",
        "blkid",
        "mkfs.vfat", // EFI partition always FAT32
        "basestrap",
    ];

    // Data filesystem tool
    match filesystem {
        Filesystem::Ext4 => bins.push("mkfs.ext4"),
        Filesystem::Btrfs => bins.push("mkfs.btrfs"),
        Filesystem::Xfs => bins.push("mkfs.xfs"),
        Filesystem::Zfs => bins.push("zpool"),
        Filesystem::F2fs => bins.push("mkfs.f2fs"),
    }

    // Boot filesystem tool (only add if different from data filesystem)
    match boot_filesystem {
        Filesystem::Ext4 if filesystem != &Filesystem::Ext4 => bins.push("mkfs.ext4"),
        Filesystem::Btrfs if filesystem != &Filesystem::Btrfs => bins.push("mkfs.btrfs"),
        Filesystem::Xfs if filesystem != &Filesystem::Xfs => bins.push("mkfs.xfs"),
        Filesystem::Zfs if filesystem != &Filesystem::Zfs => bins.push("zpool"),
        Filesystem::F2fs if filesystem != &Filesystem::F2fs => bins.push("mkfs.f2fs"),
        _ => {} // already covered by data filesystem match
    }

    // Encryption
    if encryption {
        bins.push("cryptsetup");
    }

    // dm-verity sealing (LVM immutable A/B) needs veritysetup, which ships with
    // cryptsetup — required even on unencrypted A/B installs.
    if needs_verity {
        bins.push("veritysetup");
    }

    // LVM for LVM thin provisioning (feature-driven)
    if use_lvm_thin {
        bins.push("pvcreate");
        bins.push("vgcreate");
        bins.push("lvcreate");
    }

    // Bootloader
    match bootloader {
        Bootloader::Grub => {
            bins.push("grub-install");
            bins.push("grub-mkconfig");
        }
    }

    bins
}

/// Check for missing dependencies and return list of missing packages
pub fn check_dependencies(
    filesystem: &Filesystem,
    boot_filesystem: &Filesystem,
    encryption: bool,
    use_lvm_thin: bool,
    bootloader: &Bootloader,
    needs_verity: bool,
) -> Vec<String> {
    let required = required_binaries(
        filesystem,
        boot_filesystem,
        encryption,
        use_lvm_thin,
        bootloader,
        needs_verity,
    );
    let bin_to_pkg = binary_to_package();

    let mut missing_packages: Vec<String> = Vec::new();

    for bin in required {
        if !binary_exists(bin) {
            if let Some(&pkgs) = bin_to_pkg.get(bin) {
                for pkg in pkgs {
                    if !missing_packages.contains(&pkg.to_string()) {
                        missing_packages.push(pkg.to_string());
                    }
                }
            } else {
                // Unknown package, just report the binary
                missing_packages.push(format!("(provides {})", bin));
            }
        }
    }

    missing_packages
}

/// Check dependencies and optionally install missing packages
/// Returns Ok(()) if all dependencies are satisfied (or were installed)
/// Returns Err if dependencies are missing and user declined to install
pub fn ensure_dependencies(
    cmd: &CommandRunner,
    filesystem: &Filesystem,
    boot_filesystem: &Filesystem,
    encryption: bool,
    use_lvm_thin: bool,
    bootloader: &Bootloader,
    needs_verity: bool,
) -> Result<()> {
    let required = required_binaries(
        filesystem,
        boot_filesystem,
        encryption,
        use_lvm_thin,
        bootloader,
        needs_verity,
    );
    let bin_to_pkg = binary_to_package();

    // Collect missing binaries with their providing packages
    let mut missing_packages: Vec<String> = Vec::new();
    let mut missing_details: Vec<(String, String)> = Vec::new(); // (binary, package)

    for bin in required {
        if !binary_exists(bin) {
            let pkgs: &[&str] = bin_to_pkg.get(bin).copied().unwrap_or(&["unknown"]);
            missing_details.push((bin.to_string(), pkgs.join(" ")));
            for pkg in pkgs {
                if !missing_packages.contains(&pkg.to_string()) {
                    missing_packages.push(pkg.to_string());
                }
            }
        }
    }

    if missing_details.is_empty() {
        info!("All required host dependencies are installed");
        return Ok(());
    }

    println!("\n⚠ Missing host system dependencies:");
    for (bin, pkg) in &missing_details {
        println!("  - {} (package: {})", bin, pkg);
    }
    println!("\nPackages to install: {}", missing_packages.join(" "));
    println!();

    if cmd.is_dry_run() {
        println!(
            "[dry-run] Would install: pacman -S --noconfirm {}",
            missing_packages.join(" ")
        );
        return Ok(());
    }

    // Install missing packages automatically
    println!("Installing missing packages...");
    let status = Command::new("pacman")
        .args(["-S", "--noconfirm"])
        .args(&missing_packages)
        .status()?;

    if !status.success() {
        return Err(crate::utils::error::DeploytixError::CommandFailed {
            command: format!("pacman -S {}", missing_packages.join(" ")),
            stderr: format!("Exit code: {:?}", status.code()),
        });
    }

    // Verify installation
    let still_missing = check_dependencies(
        filesystem,
        boot_filesystem,
        encryption,
        use_lvm_thin,
        bootloader,
        needs_verity,
    );
    if !still_missing.is_empty() {
        return Err(crate::utils::error::DeploytixError::ConfigError(format!(
            "Failed to install some dependencies: {}",
            still_missing.join(", ")
        )));
    }

    info!("Successfully installed missing dependencies");
    Ok(())
}

// ======================== Kernel device-mapper targets ========================

/// A device-mapper target the install needs the **host** kernel to provide.
///
/// Binary checks are not enough. `lvcreate`, `cryptsetup` and `veritysetup` are
/// userspace front-ends: they exist, run, and then fail at the ioctl because the
/// running kernel has no such target. A kernel built without
/// `CONFIG_DM_THIN_PROVISIONING` gets all the way to
///
/// ```text
/// modprobe: FATAL: Module dm-thin-pool not found in directory /lib/modules/<ver>
///   thin-pool: Required device-mapper target(s) not detected in your kernel.
/// ```
///
/// which arrives in phase 2 — *after* the disk has been partitioned. Custom
/// kernels (linux-tkg and friends) trim these targets fairly often, and
/// deploying from a daily-driver machine to removable media is a supported flow,
/// so the host kernel is not something deploytix can assume anything about.
pub struct DmTarget {
    /// Target name as the kernel registers it (`dmsetup targets` column 1).
    pub target: &'static str,
    /// Module providing it, for the `modinfo` fallback and the error message.
    pub module: &'static str,
    /// Which deploytix feature needs it, named as the user selected it.
    pub feature: &'static str,
    /// Kernel config symbol, so the message says what to actually look for.
    pub kconfig: &'static str,
}

/// The device-mapper targets required by this configuration.
///
/// Feature-driven, matching the pipeline: each flag contributes its targets and
/// contributes nothing when off.
pub fn required_dm_targets(
    encryption: bool,
    integrity: bool,
    use_lvm_thin: bool,
    needs_verity: bool,
) -> Vec<DmTarget> {
    let mut targets = Vec::new();

    if use_lvm_thin {
        // `thin-pool` creates the pool; `thin` is what every thin LV carved out
        // of it needs. A kernel can technically carry one without the other, and
        // reporting both up front beats discovering the second one later.
        targets.push(DmTarget {
            target: "thin-pool",
            module: "dm-thin-pool",
            feature: "LVM thin provisioning (use_lvm_thin)",
            kconfig: "CONFIG_DM_THIN_PROVISIONING",
        });
        targets.push(DmTarget {
            target: "thin",
            module: "dm-thin-pool",
            feature: "LVM thin provisioning (use_lvm_thin)",
            kconfig: "CONFIG_DM_THIN_PROVISIONING",
        });
    }

    if encryption {
        targets.push(DmTarget {
            target: "crypt",
            module: "dm-crypt",
            feature: "LUKS2 encryption (disk.encryption)",
            kconfig: "CONFIG_DM_CRYPT",
        });
    }

    if integrity {
        targets.push(DmTarget {
            target: "integrity",
            module: "dm-integrity",
            feature: "per-sector integrity (disk.integrity)",
            kconfig: "CONFIG_DM_INTEGRITY",
        });
    }

    if needs_verity {
        targets.push(DmTarget {
            target: "verity",
            module: "dm-verity",
            feature: "immutable A/B dm-verity roots (immutable_root + use_lvm_thin)",
            kconfig: "CONFIG_DM_VERITY",
        });
    }

    targets
}

/// Targets the running kernel has already registered, per `dmsetup targets`.
///
/// `None` when `dmsetup` cannot be run at all, which is a different thing from
/// "no targets" and must not be read as one — the caller falls back to
/// [`module_is_installed`] rather than reporting everything missing.
fn registered_dm_targets() -> Option<Vec<String>> {
    let out = Command::new("dmsetup").arg("targets").output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_string)
            .collect(),
    )
}

/// Whether a module exists on disk for the running kernel (loaded or not).
///
/// `modinfo -n` resolves the module file without loading it, so this needs no
/// root and has no side effects — deliberately, because this runs in dry-run
/// too, and because a modprobe here would be a state change during what is
/// supposed to be a read-only preflight.
fn module_is_installed(module: &str) -> bool {
    Command::new("modinfo")
        .args(["-n", module])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Whether the host kernel can provide `target`.
///
/// Available if the kernel already registers it (built-in, or a module that is
/// already loaded), or if the module is installed and can therefore be loaded on
/// demand — which is what LVM and cryptsetup do themselves. Only when neither
/// holds is the target genuinely unavailable.
fn dm_target_available(target: &DmTarget, registered: Option<&Vec<String>>) -> bool {
    if let Some(registered) = registered {
        if registered.iter().any(|t| t == target.target) {
            return true;
        }
    }
    module_is_installed(target.module)
}

/// Device-mapper targets this configuration needs that the host kernel lacks.
pub fn check_dm_targets(
    encryption: bool,
    integrity: bool,
    use_lvm_thin: bool,
    needs_verity: bool,
) -> Vec<DmTarget> {
    let registered = registered_dm_targets();
    required_dm_targets(encryption, integrity, use_lvm_thin, needs_verity)
        .into_iter()
        .filter(|t| !dm_target_available(t, registered.as_ref()))
        .collect()
}

/// Fail the install *before* the disk is touched when the host kernel cannot
/// provide a device-mapper target the chosen layout depends on.
///
/// Unlike a missing binary, this is not installable — no package adds a target
/// to a running kernel — so the error explains the two things that do work:
/// boot a stock kernel, or pick a layout that does not need the target.
/// In dry-run this warns instead of failing, so a preview of an unsupported
/// layout still completes and still tells the user what would have stopped it.
pub fn ensure_kernel_dm_targets(
    cmd: &CommandRunner,
    encryption: bool,
    integrity: bool,
    use_lvm_thin: bool,
    needs_verity: bool,
) -> Result<()> {
    let missing = check_dm_targets(encryption, integrity, use_lvm_thin, needs_verity);
    if missing.is_empty() {
        info!("Host kernel provides all required device-mapper targets");
        return Ok(());
    }

    let kernel = Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let mut detail = String::new();
    for t in &missing {
        detail.push_str(&format!(
            "\n  - {} (module {}, {}) — needed by {}",
            t.target, t.module, t.kconfig, t.feature
        ));
    }

    let message = format!(
        "The running kernel ({kernel}) does not provide these device-mapper targets:{detail}\n\
         \n\
         This is a property of the kernel the installer is running on, not of the target \
         disk, and no package can add it — the module is absent from /lib/modules/{kernel}. \
         Custom kernels (linux-tkg and similar) often omit these targets.\n\
         \n\
         Options:\n\
         \x20 1. Boot the installer from a stock kernel (linux, linux-lts or linux-zen) \
         and run it again — the layout itself is fine.\n\
         \x20 2. Choose a layout that does not need the missing target (for thin-pool: \
         set use_lvm_thin = false and use the standard or encrypted layout).\n\
         \n\
         The disk has not been modified."
    );

    if cmd.is_dry_run() {
        println!("\n⚠ {message}\n");
        return Ok(());
    }

    Err(crate::utils::error::DeploytixError::ValidationError(
        message,
    ))
}

#[cfg(test)]
mod dm_target_tests {
    use super::*;

    fn names(targets: &[DmTarget]) -> Vec<&str> {
        targets.iter().map(|t| t.target).collect()
    }

    #[test]
    fn a_plain_layout_needs_no_device_mapper_targets() {
        assert!(required_dm_targets(false, false, false, false).is_empty());
    }

    #[test]
    fn lvm_thin_needs_both_thin_pool_and_thin() {
        let t = required_dm_targets(false, false, true, false);
        assert_eq!(names(&t), vec!["thin-pool", "thin"]);
        // The failure this whole check exists for.
        assert!(t
            .iter()
            .any(|t| t.module == "dm-thin-pool" && t.kconfig == "CONFIG_DM_THIN_PROVISIONING"));
    }

    #[test]
    fn encryption_and_integrity_are_independent() {
        assert_eq!(
            names(&required_dm_targets(true, false, false, false)),
            vec!["crypt"]
        );
        assert_eq!(
            names(&required_dm_targets(true, true, false, false)),
            vec!["crypt", "integrity"]
        );
        // integrity without encryption is not a layout deploytix builds, but the
        // selection must stay feature-driven rather than assume the pairing.
        assert_eq!(
            names(&required_dm_targets(false, true, false, false)),
            vec!["integrity"]
        );
    }

    #[test]
    fn immutable_ab_needs_verity_on_top_of_thin() {
        let t = required_dm_targets(true, false, true, true);
        assert_eq!(names(&t), vec!["thin-pool", "thin", "crypt", "verity"]);
    }

    #[test]
    fn a_target_the_kernel_registers_is_available_without_a_module() {
        // Built-in targets (CONFIG_DM_*=y) have no module file at all, so the
        // registered list must be enough on its own.
        let registered = vec!["linear".to_string(), "thin-pool".to_string()];
        let target = &required_dm_targets(false, false, true, false)[0];
        assert!(dm_target_available(target, Some(&registered)));
    }

    #[test]
    fn an_absent_target_with_no_module_is_missing() {
        // The reported case: dmsetup lists no thin-pool and /lib/modules has no
        // dm-thin-pool. `module_is_installed` is consulted for real here and is
        // expected to say no for this deliberately bogus module name.
        let registered = vec!["linear".to_string(), "striped".to_string()];
        let target = DmTarget {
            target: "deploytix-not-a-real-target",
            module: "deploytix-not-a-real-module",
            feature: "test",
            kconfig: "CONFIG_NOT_REAL",
        };
        assert!(!dm_target_available(&target, Some(&registered)));
    }

    #[test]
    fn an_unusable_dmsetup_does_not_report_everything_missing() {
        // registered = None means "could not ask", not "nothing registered".
        // Falling through to the module check is what keeps a non-root or
        // dmsetup-less host from failing a perfectly good install.
        let target = DmTarget {
            target: "deploytix-not-a-real-target",
            module: "deploytix-not-a-real-module",
            feature: "test",
            kconfig: "CONFIG_NOT_REAL",
        };
        assert!(!dm_target_available(&target, None));
    }

    #[test]
    fn dry_run_warns_rather_than_failing() {
        let cmd = CommandRunner::new(true);
        // Whatever this host's kernel offers, a dry run must not error.
        assert!(ensure_kernel_dm_targets(&cmd, true, true, true, true).is_ok());
    }
}
