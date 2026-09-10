//! Desktop environments, as insertable modules.
//!
//! Adding a desktop environment used to mean finding every place that knew how
//! many there were: a match in this file, package lists in `install/basestrap`,
//! a theme line in the display-manager config, session facts in
//! `configure/gaming/session_switching`, a hardcoded dropdown in the GUI.
//!
//! Now each desktop is one [`DesktopModule`] value in its own file, and this
//! module is the registry: [`module`] maps a [`DesktopEnvironment`] to its
//! descriptor and [`ALL`] lists them for anything that needs to enumerate.
//! Adding one is a variant on the config enum, a file, and a line in [`module`].
//!
//! The enum stays as the identity. It is the TOML surface — `environment =
//! "kde"` — so replacing it with a string key would break every config file in
//! existence for no gain. What moved out from behind it is the *behaviour*.

use crate::config::{DeploymentConfig, DesktopEnvironment};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

pub mod gnome;
pub mod kde;
pub mod none;
pub mod xfce;

/// What a desktop session is, for the gaming stack's session switching.
///
/// Lives here rather than in `session_switching` because it is a fact about
/// the desktop, and a new desktop should be able to state it in its own file.
pub struct DesktopSession {
    /// The command that starts the session.
    pub command: &'static str,
    /// Commands to try if `command` is missing, in order.
    pub fallbacks: &'static [&'static str],
    /// Processes belonging to this session, to tear down when switching away.
    /// Prefixed `x:` (exact) or `f:` (full-match) for `pkill`.
    pub procs: &'static [&'static str],
}

/// Everything the rest of deploytix needs to know about one desktop
/// environment.
pub struct DesktopModule {
    /// The config value this module answers for.
    pub id: DesktopEnvironment,
    /// Name for logs.
    pub label: &'static str,
    /// Packages to install in the chroot.
    pub packages: &'static [&'static str],
    /// Packages that follow Artix's `{name}-{init}` service convention. The
    /// base package is expected to be in `packages`; this adds the init-
    /// specific service unit for whichever init the install uses.
    pub service_packages: &'static [&'static str],
    /// What `.xinitrc` should exec, for the startx fallback.
    pub xinitrc_command: Option<&'static str>,
    /// Session facts, or `None` for a desktop with no session to switch to.
    pub session: Option<DesktopSession>,
    /// An extra stanza for `sddm.conf`, such as a theme this desktop ships.
    pub sddm_conf: Option<&'static str>,
    /// The body of the deploytix launcher's `.desktop` file.
    pub desktop_file: fn(&str) -> String,
}

impl DesktopModule {
    /// Whether this is a real graphical environment.
    ///
    /// The headless module answers `false`, which is what the display server,
    /// display manager and session-switching steps key off.
    pub fn is_graphical(&self) -> bool {
        !self.packages.is_empty()
    }
}

/// Every desktop deploytix knows how to install.
pub const ALL: &[&DesktopModule] = &[&none::MODULE, &kde::MODULE, &gnome::MODULE, &xfce::MODULE];

/// The module for a desktop environment.
///
/// The single place that turns the config enum into behaviour. A new variant
/// makes this fail to compile until its module is registered, which is the
/// point.
pub fn module(de: &DesktopEnvironment) -> &'static DesktopModule {
    match de {
        DesktopEnvironment::None => &none::MODULE,
        DesktopEnvironment::Kde => &kde::MODULE,
        DesktopEnvironment::Gnome => &gnome::MODULE,
        DesktopEnvironment::Xfce => &xfce::MODULE,
    }
}

/// Generate desktop file content for the given desktop environment.
pub fn generate_desktop_file(de: &DesktopEnvironment, bindir: &str) -> String {
    (module(de).desktop_file)(bindir)
}

/// Install the configured desktop environment into the target.
///
/// One routine for every desktop: the differences between them are all in
/// their descriptors. The headless module installs nothing and falls out of
/// the same code path rather than needing a branch.
pub fn install(cmd: &CommandRunner, config: &DeploymentConfig, install_root: &str) -> Result<()> {
    let module = module(&config.desktop.environment);

    if !module.is_graphical() {
        info!("No desktop environment selected — headless/server mode");
        return Ok(());
    }

    info!("Installing {} desktop environment", module.label);

    // Artix names an init system's service package `{package}-{init}`, so the
    // init-specific half of the list is derived rather than enumerated.
    let mut packages: Vec<String> = module.packages.iter().map(|p| p.to_string()).collect();
    packages.extend(
        module
            .service_packages
            .iter()
            .map(|p| format!("{}-{}", p, config.system.init)),
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install {} packages: {:?}",
            module.label, packages
        );
        return Ok(());
    }

    let install_cmd = format!("pacman -S --noconfirm {}", packages.join(" "));
    crate::install::packages::pacman_install_chroot(cmd, install_root, &install_cmd)?;

    // .xinitrc, for the startx fallback.
    if let Some(session_command) = module.xinitrc_command {
        let xinitrc_path = format!("{}/home/{}/.xinitrc", install_root, config.user.name);
        fs::write(&xinitrc_path, format!("exec {}\n", session_command))?;
    }

    info!("{} installation complete", module.label);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_enum_variant_resolves_to_its_own_module() {
        for de in [
            DesktopEnvironment::None,
            DesktopEnvironment::Kde,
            DesktopEnvironment::Gnome,
            DesktopEnvironment::Xfce,
        ] {
            assert_eq!(module(&de).id, de);
        }
    }

    #[test]
    fn the_registry_lists_every_module_exactly_once() {
        assert_eq!(ALL.len(), 4);
        for m in ALL {
            assert_eq!(module(&m.id).label, m.label);
        }
    }

    #[test]
    fn only_the_headless_module_is_non_graphical() {
        let headless: Vec<&str> = ALL
            .iter()
            .filter(|m| !m.is_graphical())
            .map(|m| m.label)
            .collect();
        assert_eq!(headless, vec![none::MODULE.label]);
    }

    /// A desktop that can be switched to needs something to switch *to*.
    #[test]
    fn every_graphical_module_has_a_session() {
        for m in ALL.iter().filter(|m| m.is_graphical()) {
            assert!(m.session.is_some(), "{} has no session", m.label);
            assert!(m.xinitrc_command.is_some(), "{} has no xinitrc", m.label);
        }
    }
}
