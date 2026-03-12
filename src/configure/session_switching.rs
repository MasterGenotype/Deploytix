//! Session switching scripts deployment (gamescope ↔ desktop mode via greetd)

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::info;

// Embedded script resources (compiled into the binary)
const SESSION_SELECT: &str = include_str!("../resources/session_switching/session-select.sh");
const RETURN_TO_GAMEMODE: &str =
    include_str!("../resources/session_switching/return-to-gamemode.sh");
const DESKTOP_SESSION_ONESHOT: &str =
    include_str!("../resources/session_switching/desktop-session-oneshot.sh");
const DESKTOP_ONESHOT_DESKTOP: &str =
    include_str!("../resources/session_switching/desktop-oneshot.desktop");
const GAMESCOPE_SESSION_DESKTOP: &str =
    include_str!("../resources/session_switching/gamescope-session.desktop");
const POLKIT_RULES: &str = include_str!("../resources/session_switching/session-switching.rules");

/// File to deploy with its destination path (relative to install root) and permissions
struct DeployFile {
    dest: &'static str,
    content: &'static str,
    mode: u32,
}

const DEPLOY_FILES: &[DeployFile] = &[
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
        dest: "usr/bin/desktop-session-oneshot",
        content: DESKTOP_SESSION_ONESHOT,
        mode: 0o755,
    },
    DeployFile {
        dest: "usr/share/wayland-sessions/desktop-oneshot.desktop",
        content: DESKTOP_ONESHOT_DESKTOP,
        mode: 0o644,
    },
    DeployFile {
        dest: "usr/share/wayland-sessions/gamescope-session.desktop",
        content: GAMESCOPE_SESSION_DESKTOP,
        mode: 0o644,
    },
    DeployFile {
        dest: "usr/share/polkit-1/rules.d/session-switching.rules",
        content: POLKIT_RULES,
        mode: 0o644,
    },
];

/// Deploy session switching scripts and configuration to the target system.
///
/// This writes the helper scripts (`session-select`, `return-to-gamemode`,
/// `desktop-session-oneshot`), wayland session `.desktop` files, and polkit
/// rules into `install_root`.
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

    info!("Session switching scripts deployed successfully");
    Ok(())
}
