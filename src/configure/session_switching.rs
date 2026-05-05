//! Session switching scripts deployment (gamescope ↔ desktop mode via greetd)

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tracing::info;

// Embedded script resources (compiled into the binary)
const SESSION_MANAGER: &str =
    include_str!("../resources/session_switching/deploytix-session-manager.sh");
const SESSION_SELECT: &str = include_str!("../resources/session_switching/session-select.sh");
const RETURN_TO_GAMEMODE: &str =
    include_str!("../resources/session_switching/return-to-gamemode.sh");
const STEAM_GAMESCOPE_SESSION: &str =
    include_str!("../resources/session_switching/steam-gamescope-session.sh");
const GAMESCOPE_SESSION_DESKTOP: &str =
    include_str!("../resources/session_switching/gamescope-session.desktop");
const STEAMOS_SELECT_BRANCH: &str =
    include_str!("../resources/session_switching/steamos-select-branch.sh");
const GREETD_IPC: &str = include_str!("../resources/session_switching/greetd-ipc.py");
const GREETD_PAM: &str = include_str!("../resources/session_switching/greetd.pam");
const GREETD_GREETER_PAM: &str = include_str!("../resources/session_switching/greetd-greeter.pam");

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
        dest: "usr/local/bin/steam-gamescope-session",
        content: STEAM_GAMESCOPE_SESSION,
        mode: 0o755,
    },
    DeployFile {
        dest: "usr/share/wayland-sessions/gamescope-session.desktop",
        content: GAMESCOPE_SESSION_DESKTOP,
        mode: 0o644,
    },
    DeployFile {
        dest: "usr/bin/steamos-select-branch",
        content: STEAMOS_SELECT_BRANCH,
        mode: 0o755,
    },
    DeployFile {
        dest: "usr/bin/greetd-ipc",
        content: GREETD_IPC,
        mode: 0o755,
    },
    // PAM service files.
    //
    // `greetd` is used for Class=user sessions created via greetd IPC
    // (the path deploytix-session-manager takes after picking a session).
    //
    // `greetd-greeter` is used for greetd's own default_session (the
    // greeter itself). Without this file, greetd's pam_start("greetd-greeter")
    // falls through to /etc/pam.d/other (deny-all on Arch/Artix), which
    // contributed to the "greeter exited without creating a session"
    // respawn loop fixed alongside the removal of `steam -shutdown`
    // from cleanup_stale_sessions.
    DeployFile {
        dest: "etc/pam.d/greetd",
        content: GREETD_PAM,
        mode: 0o644,
    },
    DeployFile {
        dest: "etc/pam.d/greetd-greeter",
        content: GREETD_GREETER_PAM,
        mode: 0o644,
    },
];

/// Deploy session switching scripts and configuration to the target system.
///
/// Architecture: greetd runs `deploytix-session-manager` as its greeter.
/// The session manager uses `greetd-ipc` (Python) to create a proper
/// `Class=user` session via greetd's IPC protocol, then greetd starts
/// `steam-gamescope-session` (or a desktop session) in that user session.
/// This avoids the elogind seat-revocation issue with `Class=greeter`.
///
/// The gamescope compositor itself is built from the Bazzite-maintained
/// source in `configure::packages::install_gaming_packages`.
pub fn setup_session_switching(
    _cmd: &CommandRunner,
    _config: &DeploymentConfig,
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

    // Create steamos-session-select symlink so Steam's "Switch to Desktop" works.
    // Steam calls `steamos-session-select <session>` internally.
    let symlink_path = format!("{}/usr/bin/steamos-session-select", install_root);
    let symlink = Path::new(&symlink_path);
    if symlink.exists() || symlink.read_link().is_ok() {
        fs::remove_file(symlink)?;
    }
    std::os::unix::fs::symlink("session-select", symlink)?;
    info!("  Symlinked steamos-session-select -> session-select");

    info!("Session switching scripts deployed successfully");
    Ok(())
}
