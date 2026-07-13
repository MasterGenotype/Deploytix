//! Gamescope update utility deployment
//!
//! `gamescope-git` on deployed systems is the Bazzite-maintained fork built
//! with a specific set of meson options (installed during basestrap from the
//! custom \[deploytix\] repository).  Updating it through the AUR replaces it
//! with the upstream Valve build, compiled with different options, which
//! breaks the Steam gamescope session.
//!
//! This module deploys the canonical update path to the target:
//! - `deploytix-update-gamescope` — rebuilds gamescope from the same
//!   fork/branch with the exact same PKGBUILD (and thus the exact same meson
//!   options) every time, then installs it via `pacman -U`.
//! - The canonical PKGBUILD at `/usr/share/deploytix/gamescope/PKGBUILD`
//!   (kept in sync with `vendor/gamescope/pkg/PKGBUILD`; only the source URL
//!   differs — the public https remote instead of a local `git+file://`).
//! - A desktop entry so the update can be launched from the application menu.
//! - A pacman `PreTransaction` hook that aborts any gamescope
//!   install/upgrade not initiated by the update utility.
//! - `IgnorePkg = gamescope-git` in the target's `pacman.conf` so `pacman
//!   -Syu` / `yay -Syu` never try to replace the package on their own.

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::info;

// Embedded resources (compiled into the binary)
const UPDATE_SCRIPT: &str =
    include_str!("../resources/gamescope_update/deploytix-update-gamescope.sh");
const CANONICAL_PKGBUILD: &str = include_str!("../resources/gamescope_update/PKGBUILD");
const DESKTOP_ENTRY: &str =
    include_str!("../resources/gamescope_update/deploytix-update-gamescope.desktop");
const GUARD_HOOK: &str =
    include_str!("../resources/gamescope_update/deploytix-gamescope-guard.hook");
const GUARD_SCRIPT: &str = include_str!("../resources/gamescope_update/gamescope-guard.sh");

/// File to deploy with its destination path (relative to install root) and permissions
struct DeployFile {
    dest: &'static str,
    content: &'static str,
    mode: u32,
}

const DEPLOY_FILES: &[DeployFile] = &[
    DeployFile {
        dest: "usr/bin/deploytix-update-gamescope",
        content: UPDATE_SCRIPT,
        mode: 0o755,
    },
    DeployFile {
        dest: "usr/share/deploytix/gamescope/PKGBUILD",
        content: CANONICAL_PKGBUILD,
        mode: 0o644,
    },
    DeployFile {
        dest: "usr/share/deploytix/gamescope/gamescope-guard.sh",
        content: GUARD_SCRIPT,
        mode: 0o755,
    },
    DeployFile {
        dest: "usr/share/libalpm/hooks/deploytix-gamescope-guard.hook",
        content: GUARD_HOOK,
        mode: 0o644,
    },
    DeployFile {
        dest: "usr/share/applications/deploytix-update-gamescope.desktop",
        content: DESKTOP_ENTRY,
        mode: 0o644,
    },
];

/// Deploy the gamescope update utility, canonical PKGBUILD, desktop entry,
/// and update guard to the target system.
///
/// Only meaningful on gaming deployments — `gamescope-git` is installed
/// during basestrap when `install_gaming` is enabled.
pub fn setup_gamescope_update(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_gaming {
        return Ok(());
    }

    info!("Deploying gamescope update utility to {}", install_root);

    if cmd.is_dry_run() {
        for file in DEPLOY_FILES {
            println!(
                "  [dry-run] Would install /{} (mode {:o})",
                file.dest, file.mode
            );
        }
        println!("  [dry-run] Would add 'IgnorePkg = gamescope-git' to /etc/pacman.conf");
        return Ok(());
    }

    for file in DEPLOY_FILES {
        let full_path = format!("{}/{}", install_root, file.dest);

        if let Some(parent) = std::path::Path::new(&full_path).parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(&full_path, file.content)?;
        fs::set_permissions(&full_path, fs::Permissions::from_mode(file.mode))?;

        info!("  Installed {} (mode {:o})", file.dest, file.mode);
    }

    // Keep pacman/yay from ever replacing the custom build on -Syu.  The
    // PreTransaction hook already blocks explicit installs; IgnorePkg makes
    // routine system upgrades skip the package silently instead of failing.
    cmd.run_in_chroot(
        install_root,
        "grep -q '^IgnorePkg *= *gamescope-git' /etc/pacman.conf || \
         sed -i '/^\\[options\\]/a IgnorePkg = gamescope-git' /etc/pacman.conf",
    )?;
    info!("  Added IgnorePkg = gamescope-git to pacman.conf");

    info!("Gamescope update utility deployed successfully");
    Ok(())
}
