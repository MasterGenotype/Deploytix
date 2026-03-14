//! Session switching scripts deployment (gamescope ↔ desktop mode via greetd)

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::{info, warn};

// Embedded script resources (compiled into the binary)
const SESSION_MANAGER: &str =
    include_str!("../resources/session_switching/deploytix-session-manager.sh");
const SESSION_SELECT: &str = include_str!("../resources/session_switching/session-select.sh");
const RETURN_TO_GAMEMODE: &str =
    include_str!("../resources/session_switching/return-to-gamemode.sh");
const GAMESCOPE_SESSION_DESKTOP: &str =
    include_str!("../resources/session_switching/gamescope-session.desktop");

/// File to deploy with its destination path (relative to install root) and permissions
struct DeployFile {
    dest: &'static str,
    content: &'static str,
    mode: u32,
}

const DEPLOY_FILES: &[DeployFile] = &[
    DeployFile {
        dest: "usr/bin/deploytix-session-manager",
        content: SESSION_MANAGER,
        mode: 0o755,
    },
    DeployFile {
        dest: "usr/bin/session-select",
        content: SESSION_SELECT,
        mode: 0o755,
    },
    DeployFile {
        dest: "usr/bin/return-to-gamemode",
        content: RETURN_TO_GAMEMODE,
        mode: 0o755,
    },
    DeployFile {
        dest: "usr/share/wayland-sessions/gamescope-session.desktop",
        content: GAMESCOPE_SESSION_DESKTOP,
        mode: 0o644,
    },
];

/// Deploy session switching scripts and configuration to the target system.
///
/// This writes `deploytix-session-manager`, `session-select`,
/// `return-to-gamemode`, and the gamescope wayland session `.desktop`
/// file into `install_root`, then builds `gamescope-session-git` from
/// the AUR.
pub fn setup_session_switching(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    info!("Deploying session switching scripts to {}", install_root);

    for file in DEPLOY_FILES {
        let full_path = format!("{}/{}", install_root, file.dest);

        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(&full_path).parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(&full_path, file.content)?;
        fs::set_permissions(&full_path, fs::Permissions::from_mode(file.mode))?;

        info!("  Installed {} (mode {:o})", file.dest, file.mode);
    }

    // Build gamescope-session-git from AUR (provides the gamescope-session
    // command that greetd's initial_session and the .desktop file reference).
    install_gamescope_session(cmd, config, install_root)?;

    info!("Session switching scripts deployed successfully");
    Ok(())
}

/// Build and install `gamescope-session-git` from the AUR.
///
/// Uses the same pattern as the yay AUR build: clone into a temp
/// directory, build as the configured user via `makepkg -si`, then
/// clean up.
fn install_gamescope_session(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let username = &config.user.name;

    info!("Building gamescope-session-git from AUR as {}", username);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would build gamescope-session-git from AUR as {}", username);
        return Ok(());
    }

    // Ensure base-devel and git are present (should already be from basestrap)
    cmd.run_in_chroot(
        install_root,
        "pacman -S --noconfirm --needed git base-devel",
    )?;

    let build_cmd = format!(
        "mkdir -p /tmp/aur-build && \
         chown {0}:{0} /tmp/aur-build && \
         sudo -u {0} bash -c '\
           cd /tmp/aur-build && \
           git clone https://aur.archlinux.org/gamescope-session-git.git && \
           cd gamescope-session-git && \
           makepkg -si --noconfirm' && \
         rm -rf /tmp/aur-build/gamescope-session-git",
        username
    );

    match cmd.run_in_chroot(install_root, &build_cmd) {
        Ok(_) => info!("gamescope-session-git installed successfully"),
        Err(e) => {
            warn!("Failed to build gamescope-session-git from AUR: {}", e);
            warn!("Session switching may not work until gamescope-session is installed manually");
        }
    }

    Ok(())
}
