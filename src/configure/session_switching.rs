//! Session switching scripts deployment (gamescope ↔ desktop mode via greetd)

use crate::config::{DeploymentConfig, DesktopEnvironment};
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
const DESKTOP_SESSION_TEMPLATE: &str =
    include_str!("../resources/session_switching/desktop-session.sh");
const GAMESCOPE_SESSION_DESKTOP: &str =
    include_str!("../resources/session_switching/gamescope-session.desktop");
const STEAMOS_SELECT_BRANCH: &str =
    include_str!("../resources/session_switching/steamos-select-branch.sh");
const STEAMOS_UPDATE: &str = include_str!("../resources/session_switching/steamos-update.sh");
const JUPITER_BIOSUPDATE: &str =
    include_str!("../resources/session_switching/jupiter-biosupdate.sh");
const NETWORKMANAGER_POLKIT_RULES: &str =
    include_str!("../resources/session_switching/50-deploytix-networkmanager.rules");
const GREETD_IPC: &str = include_str!("../resources/session_switching/greetd-ipc.py");
const RESTART_GREETD: &str =
    include_str!("../resources/session_switching/deploytix-restart-greetd.sh");
const STEAM_LOGIN_CHECK: &str = include_str!("../resources/session_switching/steam-login-check.sh");
const STEAM_FIRST_LOGIN: &str = include_str!("../resources/session_switching/steam-first-login.sh");
const STEAM_FIRST_LOGIN_DESKTOP: &str =
    include_str!("../resources/session_switching/deploytix-steam-first-login.desktop");
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
    // SteamOS tooling stubs probed by Steam when launched with -steamdeck
    // (required for the first-boot Deck OOBE / login screen in gamescope).
    //
    // They follow the real tools' interfaces rather than returning a
    // constant: steamos-update distinguishes `check` from `now` and uses
    // exit 7 ("already up to date") because a deploytix system is never
    // updated through the SteamOS image path, and steamos-select-branch
    // refuses a branch that does not exist here instead of reporting
    // success for it. Each logs its invocation to
    // ~/.local/state/deploytix-steamos-tooling.log, so what Steam actually
    // asks for is observable rather than guessed at.
    DeployFile {
        dest: "usr/bin/steamos-update",
        content: STEAMOS_UPDATE,
        mode: 0o755,
    },
    DeployFile {
        dest: "usr/bin/jupiter-biosupdate",
        content: JUPITER_BIOSUPDATE,
        mode: 0o755,
    },
    DeployFile {
        dest: "usr/bin/greetd-ipc",
        content: GREETD_IPC,
        mode: 0o755,
    },
    // Init-agnostic greetd restart. session-select and return-to-gamemode
    // bounce greetd (via `sudo setsid`) to switch sessions; this helper
    // detects the running init system (runit, OpenRC, s6, dinit) and
    // issues the matching service command, so non-runit handhelds are
    // fully supported.
    DeployFile {
        dest: "usr/bin/deploytix-restart-greetd",
        content: RESTART_GREETD,
        mode: 0o755,
    },
    // First-boot Steam sign-in flow.
    //
    // `steam-login-check` is the shared predicate: does loginusers.vdf
    // contain a remembered account? `steam-gamescope-session` uses it to
    // route to the desktop when Steam exits still logged out, and the
    // XDG autostart entry runs `steam-first-login` in desktop sessions
    // to offer a windowed sign-in that auto-returns to gamemode.
    DeployFile {
        dest: "usr/bin/steam-login-check",
        content: STEAM_LOGIN_CHECK,
        mode: 0o755,
    },
    DeployFile {
        dest: "usr/bin/steam-first-login",
        content: STEAM_FIRST_LOGIN,
        mode: 0o755,
    },
    DeployFile {
        dest: "etc/xdg/autostart/deploytix-steam-first-login.desktop",
        content: STEAM_FIRST_LOGIN_DESKTOP,
        mode: 0o644,
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

/// Per-desktop-environment facts needed to render `desktop-session`.
struct DesktopSpec {
    /// Primary session command installed for this desktop environment.
    command: &'static str,
    /// `XDG_CURRENT_DESKTOP` / `XDG_SESSION_DESKTOP` value.
    name: &'static str,
    /// `XDG_SESSION_TYPE`; empty for X11 desktops that set it themselves.
    session_type: &'static str,
    /// Alternates tried, in order, when `command` is not on PATH — a system
    /// whose desktop was swapped after install still gets a usable session.
    fallbacks: &'static [&'static str],
    /// Teardown targets as `<x|f>:<pattern>` (`x` = exact process name,
    /// `f` = full command line), matching the template's `_de_kill`.
    procs: &'static [&'static str],
}

/// Processes torn down regardless of desktop environment.
const COMMON_TEARDOWN: &[&str] = &[
    "f:Xwayland :",
    "x:pipewire",
    "x:pipewire-pulse",
    "x:wireplumber",
];

/// Resolve the desktop-session facts for a desktop environment.
///
/// `DesktopEnvironment::None` has no session to wrap and returns `None`;
/// session switching already requires a desktop environment (enforced in
/// `DeploymentConfig::validate`), so that case simply deploys nothing.
fn desktop_spec(de: &DesktopEnvironment) -> Option<DesktopSpec> {
    match de {
        DesktopEnvironment::Kde => Some(DesktopSpec {
            command: "startplasma-wayland",
            name: "KDE",
            session_type: "wayland",
            fallbacks: &["gnome-session", "startxfce4"],
            procs: &[
                "x:startplasma-wayland",
                "x:plasma_session",
                "x:kwin_wayland",
                "x:kwin_wayland_wrapper",
                "x:kded6",
                "f:kactivitymanagerd",
                "f:xdg-desktop-portal-kde",
            ],
        }),
        DesktopEnvironment::Gnome => Some(DesktopSpec {
            command: "gnome-session",
            name: "GNOME",
            session_type: "wayland",
            fallbacks: &["startplasma-wayland", "startxfce4"],
            procs: &[
                "x:gnome-session",
                "x:gnome-session-binary",
                "x:gnome-shell",
                "x:gsd-media-keys",
                "f:xdg-desktop-portal-gnome",
            ],
        }),
        DesktopEnvironment::Xfce => Some(DesktopSpec {
            command: "startxfce4",
            name: "XFCE",
            // startxfce4 brings up its own X server and exports
            // XDG_SESSION_TYPE itself; forcing wayland here would break it.
            session_type: "",
            fallbacks: &["startplasma-wayland", "gnome-session"],
            procs: &[
                "x:startxfce4",
                "x:xfce4-session",
                "x:xfwm4",
                "x:xfdesktop",
                "x:xfce4-panel",
                "f:xdg-desktop-portal-xfce",
            ],
        }),
        DesktopEnvironment::None => None,
    }
}

/// Render `/usr/local/bin/desktop-session` for the configured desktop.
///
/// Pure and deterministic: the same [`DesktopEnvironment`] always yields the
/// same bytes, so re-running the installer over an existing system rewrites
/// an identical file instead of accumulating drift.
fn render_desktop_session(de: &DesktopEnvironment) -> Option<String> {
    let spec = desktop_spec(de)?;

    // Unquoted in the template's `for` list, so emit shell-quoted words.
    let fallbacks = spec
        .fallbacks
        .iter()
        .map(|f| format!("\"{}\"", f))
        .collect::<Vec<_>>()
        .join(" ");

    // Newline-separated inside a double-quoted assignment; the template
    // splits it back apart with `while IFS= read -r`.
    let procs = spec
        .procs
        .iter()
        .chain(COMMON_TEARDOWN.iter())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");

    Some(
        DESKTOP_SESSION_TEMPLATE
            .replace("@DEPLOYTIX_DESKTOP_CMD@", spec.command)
            .replace("@DEPLOYTIX_DESKTOP_NAME@", spec.name)
            .replace("@DEPLOYTIX_SESSION_TYPE@", spec.session_type)
            .replace("@DEPLOYTIX_DESKTOP_FALLBACKS@", &fallbacks)
            .replace("@DEPLOYTIX_DE_PROCS@", &procs),
    )
}

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

    // `/usr/local/bin/desktop-session` is rendered from the chosen desktop
    // environment rather than shipped verbatim, so its launch command,
    // session-type exports and teardown process list match the desktop that
    // was actually installed. It is the command deploytix-session-manager
    // hands to greetd for the "desktop" sentinel: with no such file on disk,
    // greetd's start_session exec fails instantly, the greeter is respawned,
    // and the manager flip-flops between a dead desktop launch and a fresh
    // gamescope launch — the loop in which Steam never stays on screen.
    if let Some(content) = render_desktop_session(&config.desktop.environment) {
        let path = format!("{}/usr/local/bin/desktop-session", install_root);
        if let Some(parent) = Path::new(&path).parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, content)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        info!(
            "  Installed usr/local/bin/desktop-session for {:?} (mode 755)",
            config.desktop.environment
        );
    } else {
        info!("  Skipping desktop-session (no desktop environment selected)");
    }

    // Polkit rule granting the gamescope session user passwordless control of
    // NetworkManager, so Wi-Fi can be configured from Steam's Deck OOBE and
    // Settings > Internet (both drive NetworkManager over D-Bus). The rule is
    // templated on the username, so it can't live in DEPLOY_FILES.
    let polkit_dir = format!("{}/etc/polkit-1/rules.d", install_root);
    fs::create_dir_all(&polkit_dir)?;
    let polkit_path = format!("{}/50-deploytix-networkmanager.rules", polkit_dir);
    let polkit_rules = NETWORKMANAGER_POLKIT_RULES.replace("@DEPLOYTIX_USER@", &config.user.name);
    fs::write(&polkit_path, polkit_rules)?;
    fs::set_permissions(&polkit_path, fs::Permissions::from_mode(0o644))?;
    info!(
        "  Installed etc/polkit-1/rules.d/50-deploytix-networkmanager.rules (user '{}')",
        config.user.name
    );

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every script deploytix installs, keyed by the const that carries it.
    const EMBEDDED_SCRIPTS: &[&str] = &[
        SESSION_MANAGER,
        SESSION_SELECT,
        RETURN_TO_GAMEMODE,
        STEAM_GAMESCOPE_SESSION,
        STEAMOS_SELECT_BRANCH,
        STEAMOS_UPDATE,
        JUPITER_BIOSUPDATE,
        RESTART_GREETD,
        STEAM_LOGIN_CHECK,
        STEAM_FIRST_LOGIN,
        STEAM_FIRST_LOGIN_DESKTOP,
    ];

    /// Paths that come from packages rather than from deploytix.
    const EXTERNALLY_PROVIDED: &[&str] = &[
        "/usr/bin/env",       // shebang interpreter
        "/usr/bin/bash",      // shebang interpreter
        "/usr/bin/gamescope", // gamescope-git package
    ];

    /// Pull every `/usr/bin/...` and `/usr/local/bin/...` literal out of a script.
    fn referenced_paths(script: &str) -> Vec<String> {
        let is_path_char =
            |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/');
        let mut found = Vec::new();
        let bytes: Vec<char> = script.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == '/' && (i == 0 || !is_path_char(bytes[i - 1])) {
                let mut j = i;
                while j < bytes.len() && is_path_char(bytes[j]) {
                    j += 1;
                }
                let candidate: String = bytes[i..j].iter().collect();
                let candidate = candidate.trim_end_matches('.').to_string();
                if candidate.starts_with("/usr/bin/") || candidate.starts_with("/usr/local/bin/") {
                    found.push(candidate);
                }
                i = j;
            } else {
                i += 1;
            }
        }
        found
    }

    /// Regression guard for the first-boot session loop: `desktop-session`
    /// was referenced by deploytix-session-manager but never installed, so
    /// greetd's start_session exec failed instantly and the greeter
    /// respawned forever. Any executable a deployed script invokes by
    /// absolute path must itself be deployed.
    #[test]
    fn every_referenced_helper_is_deployed() {
        let mut deployed: Vec<String> = DEPLOY_FILES
            .iter()
            .map(|f| format!("/{}", f.dest))
            .collect();
        // Rendered separately from the DE template, not via DEPLOY_FILES.
        deployed.push("/usr/local/bin/desktop-session".to_string());
        // Symlink created by setup_session_switching().
        deployed.push("/usr/bin/steamos-session-select".to_string());

        for script in EMBEDDED_SCRIPTS {
            for path in referenced_paths(script) {
                if EXTERNALLY_PROVIDED.contains(&path.as_str()) {
                    continue;
                }
                assert!(
                    deployed.contains(&path),
                    "{} is invoked by a deployed script but is never installed",
                    path
                );
            }
        }
    }

    #[test]
    fn desktop_session_rendered_per_desktop_environment() {
        for (de, cmd, name) in [
            (DesktopEnvironment::Kde, "startplasma-wayland", "KDE"),
            (DesktopEnvironment::Gnome, "gnome-session", "GNOME"),
            (DesktopEnvironment::Xfce, "startxfce4", "XFCE"),
        ] {
            let rendered = render_desktop_session(&de).expect("desktop environment renders");
            assert!(
                rendered.contains(&format!("for _candidate in \"{}\"", cmd)),
                "{:?} should launch {}",
                de,
                cmd
            );
            assert!(rendered.contains(&format!("XDG_CURRENT_DESKTOP=\"{}\"", name)));
            // Common teardown targets are appended to every DE's list.
            assert!(rendered.contains("x:wireplumber"));
        }
    }

    #[test]
    fn rendered_desktop_session_leaves_no_placeholders() {
        for de in [
            DesktopEnvironment::Kde,
            DesktopEnvironment::Gnome,
            DesktopEnvironment::Xfce,
        ] {
            let rendered = render_desktop_session(&de).unwrap();
            assert!(
                !rendered.contains("@DEPLOYTIX_"),
                "{:?} left an unsubstituted placeholder",
                de
            );
        }
    }

    #[test]
    fn desktop_session_teardown_is_desktop_specific() {
        let kde = render_desktop_session(&DesktopEnvironment::Kde).unwrap();
        let gnome = render_desktop_session(&DesktopEnvironment::Gnome).unwrap();
        assert!(kde.contains("x:kwin_wayland") && !kde.contains("x:gnome-shell"));
        assert!(gnome.contains("x:gnome-shell") && !gnome.contains("x:kwin_wayland"));
    }

    /// Rendering is pure, so re-running the installer converges on an
    /// identical file rather than drifting.
    #[test]
    fn rendering_is_idempotent() {
        let once = render_desktop_session(&DesktopEnvironment::Kde).unwrap();
        let twice = render_desktop_session(&DesktopEnvironment::Kde).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn headless_config_renders_no_desktop_session() {
        assert!(render_desktop_session(&DesktopEnvironment::None).is_none());
    }
}
