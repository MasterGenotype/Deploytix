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
    include_str!("../../resources/session_switching/deploytix-session-manager.sh");
const SESSION_SELECT: &str = include_str!("../../resources/session_switching/session-select.sh");
const RETURN_TO_GAMEMODE: &str =
    include_str!("../../resources/session_switching/return-to-gamemode.sh");
const STEAM_GAMESCOPE_SESSION: &str =
    include_str!("../../resources/session_switching/steam-gamescope-session.sh");
const DESKTOP_SESSION_TEMPLATE: &str =
    include_str!("../../resources/session_switching/desktop-session.sh");
const GAMESCOPE_SESSION_DESKTOP: &str =
    include_str!("../../resources/session_switching/gamescope-session.desktop");
const STEAMOS_SELECT_BRANCH: &str =
    include_str!("../../resources/session_switching/steamos-select-branch.sh");
const STEAMOS_UPDATE: &str = include_str!("../../resources/session_switching/steamos-update.sh");
const JUPITER_BIOSUPDATE: &str =
    include_str!("../../resources/session_switching/jupiter-biosupdate.sh");
const NETWORKMANAGER_POLKIT_RULES: &str =
    include_str!("../../resources/session_switching/50-deploytix-networkmanager.rules");
const GREETD_IPC: &str = include_str!("../../resources/session_switching/greetd-ipc.py");
const RESTART_GREETD: &str =
    include_str!("../../resources/session_switching/deploytix-restart-greetd.sh");
const STEAM_LOGIN_CHECK: &str =
    include_str!("../../resources/session_switching/steam-login-check.sh");
const STEAM_FIRST_LOGIN: &str =
    include_str!("../../resources/session_switching/steam-first-login.sh");
const STEAM_FIRST_LOGIN_DESKTOP: &str =
    include_str!("../../resources/session_switching/deploytix-steam-first-login.desktop");
const GREETD_PAM: &str = include_str!("../../resources/session_switching/greetd.pam");
const GREETD_GREETER_PAM: &str =
    include_str!("../../resources/session_switching/greetd-greeter.pam");

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

/// Processes torn down regardless of desktop environment.
const COMMON_TEARDOWN: &[&str] = &[
    "f:Xwayland :",
    "x:pipewire",
    "x:pipewire-pulse",
    "x:wireplumber",
];

/// Render `/usr/local/bin/desktop-session` for the configured desktop.
///
/// Pure and deterministic: the same [`DesktopEnvironment`] always yields the
/// same bytes, so re-running the installer over an existing system rewrites
/// an identical file instead of accumulating drift.
fn render_desktop_session(de: &DesktopEnvironment) -> Option<String> {
    let spec = crate::desktop::module(de).session.as_ref()?;

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
            .replace("@DEPLOYTIX_DESKTOP_FALLBACKS@", &fallbacks)
            .replace("@DEPLOYTIX_DE_PROCS@", &procs),
    )
}

/// A file whose content is generated from the deployment rather than shipped
/// as-is.
///
/// This is a manifest next to [`DEPLOY_FILES`] rather than a one-off write
/// inside [`setup_session_switching`], so that one place lists everything this
/// module installs. `every_referenced_helper_is_deployed` builds its list of
/// deployed paths from both manifests. That way a generated file cannot drop
/// out of the check the way `desktop-session` did between 2026-05-04 and
/// 2026-08-29, when the greeter called it and nothing installed it.
struct GeneratedFile {
    dest: &'static str,
    mode: u32,
    /// `None` when the deployment has nothing to generate, e.g. a desktop
    /// session for a headless install. The file is then simply not written.
    render: fn(&DeploymentConfig) -> Option<String>,
}

const GENERATED_FILES: &[GeneratedFile] = &[
    // `/usr/local/bin/desktop-session` is generated from the chosen desktop
    // environment, so its launch command, session-type exports and teardown
    // process list match the desktop actually installed.
    //
    // deploytix-session-manager hands this path to greetd for the "desktop"
    // sentinel. If the file is missing, greetd's start_session exec fails at
    // once, the greeter restarts, and the manager flips between a dead desktop
    // launch and a fresh gamescope one.
    //
    // The hand-written script this replaced is kept at `ref/desktop-session.sh`
    // for reference. It is not deployed.
    GeneratedFile {
        dest: "usr/local/bin/desktop-session",
        mode: 0o755,
        render: |config| render_desktop_session(&config.desktop.environment),
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
/// source in `install::packages::install_gaming_packages`.
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

    for file in GENERATED_FILES {
        let Some(content) = (file.render)(config) else {
            info!("  Skipping {} (nothing to generate)", file.dest);
            continue;
        };
        debug_assert!(
            !content.contains("@DEPLOYTIX_"),
            "{} rendered with an unsubstituted placeholder",
            file.dest
        );

        let full_path = format!("{}/{}", install_root, file.dest);
        if let Some(parent) = Path::new(&full_path).parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&full_path, content)?;
        fs::set_permissions(&full_path, fs::Permissions::from_mode(file.mode))?;

        info!("  Generated {} (mode {:o})", file.dest, file.mode);
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

    fn test_root(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "deploytix-session-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    /// A script with its full-line comments removed, so assertions about
    /// behaviour read code rather than the prose that describes it.
    fn code_only(script: &str) -> String {
        script
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Pull every `/usr/bin/...` and `/usr/local/bin/...` literal out of a script.
    fn referenced_paths(script: &str) -> Vec<String> {
        let is_path_char =
            |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/');
        let chars: Vec<char> = script.chars().collect();
        let mut found = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '/' && (i == 0 || !is_path_char(chars[i - 1])) {
                let mut j = i;
                while j < chars.len() && is_path_char(chars[j]) {
                    j += 1;
                }
                let candidate: String = chars[i..j].iter().collect();
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

    /// The regression that hid here for four months.
    ///
    /// `deploytix-session-manager` has passed `/usr/local/bin/desktop-session`
    /// to greetd for the "desktop" sentinel since 2026-05-04 (`2b63d6e`), but
    /// nothing installed it until 2026-08-29 (`ea0be34`). It was added as a
    /// resource file and never wired into the deployment. With the file
    /// missing, greetd's `start_session` exec fails at once, the greeter
    /// restarts, and the manager flips between a dead desktop launch and a
    /// fresh gamescope one.
    ///
    /// So: any executable a deployed script calls by absolute path must itself
    /// be deployed.
    #[test]
    fn every_referenced_helper_is_deployed() {
        let mut deployed: Vec<String> = DEPLOY_FILES
            .iter()
            .map(|f| format!("/{}", f.dest))
            .collect();
        // Generated files count as deployed. They come from their own manifest
        // rather than a list maintained by hand here, which is the point of
        // having the manifest.
        deployed.extend(GENERATED_FILES.iter().map(|f| format!("/{}", f.dest)));
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

    /// No destination may appear in both manifests. If one did, the static copy
    /// and the generated copy would each try to be the last writer, and which
    /// one landed on disk would depend on loop order. That is how a
    /// hand-written `desktop-session` could quietly replace the generated one.
    #[test]
    fn no_destination_is_both_deployed_and_generated() {
        for generated in GENERATED_FILES {
            assert!(
                !DEPLOY_FILES.iter().any(|d| d.dest == generated.dest),
                "{} is written by both manifests; exactly one may own it",
                generated.dest
            );
        }
    }

    /// The installed file must be executable, and must be the generated script
    /// for the desktop actually installed rather than the raw template.
    /// Deploying the template would leave literal `@DEPLOYTIX_DESKTOP_CMD@`
    /// text where the launch command belongs.
    #[test]
    fn desktop_session_is_installed_rendered_for_the_chosen_desktop() {
        let root = test_root("desktop-session");
        let cmd = CommandRunner::new(false);
        let mut config = DeploymentConfig::sample();
        config.desktop.environment = DesktopEnvironment::Gnome;
        setup_session_switching(&cmd, &config, &root).unwrap();

        let path = format!("{}/usr/local/bin/desktop-session", root);
        let written = fs::read_to_string(&path).unwrap();

        assert_eq!(
            written,
            render_desktop_session(&DesktopEnvironment::Gnome).unwrap(),
            "the installed file must be the rendering for the configured desktop"
        );
        assert!(
            !written.contains("@DEPLOYTIX_"),
            "no placeholder may survive into the installed file"
        );
        assert!(
            written.contains("gnome-session"),
            "GNOME should launch GNOME"
        );
        assert_ne!(
            written, DESKTOP_SESSION_TEMPLATE,
            "the template itself must never be what gets deployed"
        );

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "greetd has to be able to exec it");

        let _ = fs::remove_dir_all(&root);
    }

    /// A headless deployment has no desktop to switch to, so nothing is written.
    #[test]
    fn headless_deployment_installs_no_desktop_session() {
        let root = test_root("headless");
        let cmd = CommandRunner::new(false);
        let mut config = DeploymentConfig::sample();
        config.desktop.environment = DesktopEnvironment::None;
        setup_session_switching(&cmd, &config, &root).unwrap();

        assert!(!Path::new(&format!("{}/usr/local/bin/desktop-session", root)).exists());
        let _ = fs::remove_dir_all(&root);
    }

    /// Steam invokes `steamos-session-select`; deploytix answers it with
    /// `session-select` through this symlink.
    #[test]
    fn steamos_session_select_is_symlinked() {
        let root = test_root("symlink");
        let cmd = CommandRunner::new(false);
        setup_session_switching(&cmd, &DeploymentConfig::sample(), &root).unwrap();

        let link = fs::read_link(format!("{}/usr/bin/steamos-session-select", root)).unwrap();
        assert_eq!(link.to_string_lossy(), "session-select");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn desktop_session_rendered_per_desktop_environment() {
        for (de, cmd) in [
            (DesktopEnvironment::Kde, "startplasma-wayland"),
            (DesktopEnvironment::Gnome, "gnome-session"),
            (DesktopEnvironment::Xfce, "startxfce4"),
        ] {
            let rendered = render_desktop_session(&de).expect("desktop environment renders");
            assert!(
                rendered.contains(&format!("for _candidate in \"{}\"", cmd)),
                "{:?} should launch {}",
                de,
                cmd
            );
            // Common teardown targets are appended to every DE's list.
            assert!(rendered.contains("x:wireplumber"));
        }
    }

    /// Substitution is a plain string replace over the whole file, so a
    /// placeholder named in a comment is expanded there too. The teardown list
    /// is multi-line, so documenting it in the shell header turned the header
    /// into nine bare words the shell tried to run as commands on every
    /// desktop start.
    #[test]
    fn no_placeholder_is_mentioned_inside_a_comment() {
        for (n, line) in DESKTOP_SESSION_TEMPLATE.lines().enumerate() {
            if line.trim_start().starts_with('#') && line.contains("@DEPLOYTIX_") {
                panic!(
                    "line {} documents a placeholder in a comment; it will be \
                     substituted there: {}",
                    n + 1,
                    line.trim()
                );
            }
        }
    }

    /// The rendered script must contain no stray words outside comments — the
    /// symptom the check above prevents, verified on the actual output.
    #[test]
    fn the_rendered_script_has_no_bare_teardown_entries() {
        for de in [
            DesktopEnvironment::Kde,
            DesktopEnvironment::Gnome,
            DesktopEnvironment::Xfce,
        ] {
            let rendered = render_desktop_session(&de).unwrap();
            let mut in_procs = false;
            for (n, line) in rendered.lines().enumerate() {
                let code = line.trim();
                // The teardown list is a legitimate multi-line assignment.
                if code.starts_with("_DE_PROCS=") {
                    in_procs = true;
                }
                if in_procs {
                    if code.ends_with('"') && !code.starts_with("_DE_PROCS=") {
                        in_procs = false;
                    }
                    continue;
                }
                if code.starts_with('#') || code.is_empty() {
                    continue;
                }
                assert!(
                    !(code.starts_with("x:") || code.starts_with("f:")),
                    "{de:?} line {} is a teardown entry loose in the script: {code}",
                    n + 1
                );
            }
        }
    }

    /// The wrapper must export no XDG session variables. startplasma-wayland is
    /// what creates the Wayland session; declaring XDG_SESSION_TYPE=wayland
    /// ahead of it tells Qt and KDE components a session already exists and
    /// they reach for a WAYLAND_DISPLAY kwin_wayland has not created yet.
    ///
    /// The version that ran on working hardware for four months set none of
    /// them. Setting them is what broke "Return to Desktop": Plasma died on
    /// startup, the greeter restarted with the sentinel already consumed, and
    /// the machine landed back in Game Mode.
    #[test]
    fn the_wrapper_exports_no_xdg_session_variables() {
        for de in [
            DesktopEnvironment::Kde,
            DesktopEnvironment::Gnome,
            DesktopEnvironment::Xfce,
        ] {
            let rendered = render_desktop_session(&de).unwrap();
            for line in rendered.lines() {
                let code = line.trim();
                if code.starts_with('#') {
                    continue;
                }
                for var in [
                    "XDG_SESSION_TYPE",
                    "XDG_CURRENT_DESKTOP",
                    "XDG_SESSION_DESKTOP",
                ] {
                    assert!(
                        !code.contains(&format!("{var}="))
                            && !code.contains(&format!("export {var}")),
                        "{de:?} exports {var}, which is what broke the desktop switch: {code}"
                    );
                }
            }
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

    /// Steam's first act is a client update. On a Wi-Fi-only machine
    /// NetworkManager is still associating when greetd starts, so a Steam
    /// launched at t=0 finds no route and gamemode never comes up -- while the
    /// network connects seconds later, which reads as "the network only
    /// connects once the desktop starts".
    ///
    /// The wait must come after gamescope is up (so the compositor is on
    /// screen, not a black display) and before Steam is launched.
    #[test]
    fn the_session_waits_for_a_route_before_launching_steam() {
        let s = code_only(STEAM_GAMESCOPE_SESSION);
        let ready = s
            .find("read -r response_x_display")
            .expect("session waits on gamescope's ready fd");
        let wait = s
            .find("ip route show default")
            .expect("session waits for a default route");
        let launch = s
            .find("steam -gamepadui -steamos3 -steampal -steamdeck &")
            .expect("session launches gamemode");

        assert!(ready < wait, "the wait must not delay gamescope coming up");
        assert!(wait < launch, "the wait must precede the Steam launch");
        // Bounded: an offline machine still has to reach Steam's own network
        // setup page rather than hanging here.
        assert!(s.contains("NETWORK_WAIT_SECONDS"));
    }

    /// The distro steam package ships only the bootstrap tarball. steamwebhelper
    /// and steamui.so -- which *are* the gamepad UI -- arrive with the client
    /// download, so -gamepadui before that has nothing to draw.
    #[test]
    fn a_missing_steam_client_is_bootstrapped_before_gamemode() {
        let s = code_only(STEAM_GAMESCOPE_SESSION);
        let probe = s
            .find("_steam_client_installed")
            .expect("session probes for the client");
        let launch = s
            .find("steam -gamepadui -steamos3 -steampal -steamdeck &")
            .expect("session launches gamemode");
        assert!(probe < launch);

        // The probe must test for client-only artifacts. steamwebhelper (the
        // CEF renderer) is 64-bit, but steamui.so is dlmopen'd by the 32-bit
        // legacy `steam` binary and lands in ubuntu12_32 -- confirmed against
        // a real client install, where ubuntu12_64/steamui.so never exists
        // even fully updated. The bootstrap tarball already provides the
        // ubuntu12_32 *launcher*, so this must check the .so file inside it,
        // not just the directory, which the bootstrap tarball also has.
        assert!(s.contains("ubuntu12_64/steamwebhelper"));
        assert!(s.contains("ubuntu12_32/steamui.so"));

        // The bootstrap run is the plain client; the Deck flags are exactly
        // what has no UI to draw yet.
        // End the slice at the gamemode banner: that echo names the Deck
        // flags in its message and would otherwise match below.
        let start = s.find("if ! _steam_client_installed; then").unwrap();
        let banner = s[start..]
            .find("echo \"[steam-session] Starting Steam")
            .expect("gamemode launch is announced")
            + start;
        let body = &s[start..banner];
        assert!(
            body.contains("\n    steam &\n"),
            "bootstrap runs plain steam"
        );
        for flag in ["-gamepadui", "-steamos3", "-steampal", "-steamdeck"] {
            assert!(!body.contains(flag), "bootstrap must not pass {flag}");
        }
        // Steam is single-instance via ~/.steam/steam.pipe, so the bootstrap
        // client must be gone before the gamemode launch.
        assert!(body.contains("steam.pipe"));
    }

    /// Steam runs session-select and throws away its exit code, so a rejected
    /// session name leaves no trace anywhere. The log has to be written before
    /// the `case` that can reject it, otherwise the one call worth seeing is
    /// the one that never gets recorded.
    #[test]
    fn session_select_logs_the_invocation_before_it_can_be_rejected() {
        let code = code_only(SESSION_SELECT);
        let log = code
            .find("deploytix-session-select.log")
            .expect("session-select records its invocations");
        let case = code
            .find("case \"$session\" in")
            .expect("session-select dispatches on the session name");
        assert!(log < case, "the log must precede the dispatch");
        assert!(
            code.contains("exit 1"),
            "an unknown name is still rejected, exactly as it was"
        );
    }

    /// Switching to the desktop kills gamescope directly rather than
    /// restarting greetd. Restarting the daemon races gamescope's own
    /// teardown -- gamescope holds the DRM master and two Xwayland servers,
    /// and a freshly (re)started greetd can spawn the next greeter before the
    /// old session has actually released them, leaving the desktop compositor
    /// it starts with nothing to acquire. Switching back to gamescope has no
    /// such race (a desktop compositor tears down without a second daemon in
    /// flight) and keeps using the greetd restart, same as return-to-gamemode.
    #[test]
    fn the_desktop_switch_kills_gamescope_without_restarting_greetd() {
        let code = code_only(SESSION_SELECT);
        let desktop_branch_start = code
            .find(r#"if [[ "$session" == "desktop" ]]; then"#)
            .expect("session-select branches on the target session");
        let else_start = code[desktop_branch_start..]
            .find("else")
            .map(|i| i + desktop_branch_start)
            .expect("the desktop branch has an else for the gamescope target");

        let desktop_branch = &code[desktop_branch_start..else_start];
        assert!(
            desktop_branch.contains("pidof gamescope"),
            "the desktop switch kills gamescope directly"
        );
        assert!(
            !desktop_branch.contains("deploytix-restart-greetd"),
            "the desktop switch must not race a greetd restart against gamescope's own teardown"
        );

        let gamescope_branch = &code[else_start..];
        assert!(
            gamescope_branch.contains("deploytix-restart-greetd"),
            "switching back to gamescope still restarts greetd"
        );

        // The reverse direction has always used the greetd restart alone.
        let back = code_only(RETURN_TO_GAMEMODE);
        assert!(back.contains("deploytix-restart-greetd"));
    }

    /// A session script that waits only on Steam wedges when gamescope goes
    /// first: it never exits, so greetd never restarts the greeter.
    #[test]
    fn the_gamescope_session_ends_when_the_compositor_does() {
        let code = code_only(STEAM_GAMESCOPE_SESSION);
        let launch = code
            .find("steam -gamepadui")
            .expect("the session launches gamemode Steam");
        let tail = &code[launch..];
        assert!(
            tail.contains(r#"wait -n "$steam_pid" "$gamescope_pid""#),
            "the session waits on whichever child exits first"
        );
        assert!(
            tail.contains(r#"kill -0 "$gamescope_pid""#),
            "and checks which one it was"
        );
    }
}
