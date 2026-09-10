//! Init systems, as insertable modules.
//!
//! Artix ships four, and deploytix supports all of them. Most of the variation
//! is mechanical — the `{package}-{init}` naming convention means a service's
//! package name is derived, not looked up — but the exceptions were scattered:
//! how a service is enabled, where definitions live, which services have no
//! packaged unit for a given init, and the two commands only s6 needs.
//!
//! Each init is now one [`InitModule`] in its own file, and this module is the
//! registry. As with [`crate::desktop`], the config enum stays as the identity
//! (it is the TOML surface, `init = "runit"`); what moved is the behaviour.

use crate::config::InitSystem;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;

pub mod dinit;
pub mod openrc;
pub mod runit;
pub mod s6;

/// Everything the rest of deploytix needs to know about one init system.
pub struct InitModule {
    /// The config value this module answers for.
    pub id: InitSystem,
    /// The init's base package.
    pub base_package: &'static str,
    /// Where service definitions live on the installed system.
    pub service_dir: &'static str,
    /// Where enabled services are recorded.
    ///
    /// For s6 this is the default bundle's contents directory. Do not write to
    /// it directly: services are enabled with `s6 set enable` and persisted
    /// with `s6 set commit` + `s6 live install --init`.
    pub enabled_dir: &'static str,
    /// Base packages that have no `{base}-{init}` service package for this
    /// init, so asking pacman for one would fail the whole transaction.
    pub no_service_package: &'static [&'static str],
    /// Enable a service on the target system.
    pub enable: fn(&CommandRunner, &str, &str) -> Result<()>,
    /// Rebuild the init's service database after definitions change on disk.
    /// `None` for inits whose definitions need no indexing.
    pub sync_repository: Option<fn(&CommandRunner, &str) -> Result<()>>,
    /// Persist staged service changes as the boot database. `None` for inits
    /// whose enable operations (symlinks, `rc-update`) are already permanent.
    pub commit_database: Option<fn(&CommandRunner, &str) -> Result<()>>,
}

/// Every init system deploytix can install.
pub const ALL: &[&InitModule] = &[&runit::MODULE, &openrc::MODULE, &s6::MODULE, &dinit::MODULE];

/// The module for an init system.
pub fn module(init: &InitSystem) -> &'static InitModule {
    match init {
        InitSystem::Runit => &runit::MODULE,
        InitSystem::OpenRC => &openrc::MODULE,
        InitSystem::S6 => &s6::MODULE,
        InitSystem::Dinit => &dinit::MODULE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_enum_variant_resolves_to_its_own_module() {
        for init in [
            InitSystem::Runit,
            InitSystem::OpenRC,
            InitSystem::S6,
            InitSystem::Dinit,
        ] {
            assert_eq!(module(&init).id, init);
        }
    }

    #[test]
    fn the_registry_lists_every_module_exactly_once() {
        assert_eq!(ALL.len(), 4);
        for m in ALL {
            assert_eq!(module(&m.id).base_package, m.base_package);
        }
    }

    /// The paths and package names each init is installed and wired up with.
    /// These moved off `InitSystem`'s own impl; the assertions came with them.
    #[test]
    fn each_module_carries_its_own_packages_and_paths() {
        let runit = module(&InitSystem::Runit);
        assert_eq!(runit.base_package, "runit");
        assert_eq!(runit.service_dir, "/etc/runit/sv");
        assert_eq!(runit.enabled_dir, "/run/runit/service");

        let openrc = module(&InitSystem::OpenRC);
        assert_eq!(openrc.base_package, "openrc");
        assert_eq!(openrc.service_dir, "/etc/init.d");
        assert_eq!(openrc.enabled_dir, "/etc/runlevels/default");

        let s6 = module(&InitSystem::S6);
        assert_eq!(s6.base_package, "s6-base");
        assert_eq!(s6.service_dir, "/etc/s6/sv");
        assert_eq!(s6.enabled_dir, "/etc/s6/adminsv/default/contents.d");

        let dinit = module(&InitSystem::Dinit);
        assert_eq!(dinit.base_package, "dinit");
        assert_eq!(dinit.service_dir, "/etc/dinit.d");
        assert_eq!(dinit.enabled_dir, "/etc/dinit.d/boot.d");
    }

    /// s6 is the only init that needs a database step; if a second one ever
    /// does, this test is the reminder that `commit_database` is a real hook
    /// and not an s6 special case in disguise.
    #[test]
    fn only_s6_needs_a_service_database() {
        let with_db: Vec<&str> = ALL
            .iter()
            .filter(|m| m.commit_database.is_some())
            .map(|m| m.base_package)
            .collect();
        assert_eq!(with_db, vec![s6::MODULE.base_package]);
    }
}
