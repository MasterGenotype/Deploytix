//! s6, managed through upstream's s6-frontend CLI.
//!
//! The only init here that keeps a compiled service database: enabling stages
//! a change to the default bundle, and it takes a separate commit for the
//! installed system to boot with it. Those two steps are the reason
//! [`super::InitModule`] has `sync_repository` and `commit_database` hooks at
//! all.

use super::InitModule;
use crate::config::InitSystem;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::path::Path;
use tracing::{info, warn};

pub const MODULE: InitModule = InitModule {
    id: InitSystem::S6,
    base_package: "s6-base",
    service_dir: "/etc/s6/sv",
    enabled_dir: "/etc/s6/adminsv/default/contents.d",
    // No greetd-s6 package exists in Artix repos; deploytix writes the service
    // directory itself in `configure::greetd`. Every other service (elogind-s6
    // included) has a proper Artix package.
    no_service_package: &["greetd"],
    enable,
    sync_repository: Some(sync_repository),
    commit_database: Some(commit_database),
};

/// Locate the s6 service definition for `service` on the target system.
///
/// Definitions from official `-s6` packages live in `/etc/s6/sv`; custom
/// services written by deploytix live in `/etc/s6/adminsv` (the directory
/// reserved for admin-defined s6-rc services).  Since the move to
/// s6-frontend, Artix packages ship service directories under the plain
/// service name; the legacy in-house `{name}-srv` layout is still checked
/// as a fallback for transition-era packages.
///
/// Returns the name to pass to `s6 set enable`, or `None` when no
/// definition exists.
fn resolve_service_name(service: &str, install_root: &str) -> Option<String> {
    let legacy = format!("{}-srv", service);
    for name in [service, legacy.as_str()] {
        for base in ["etc/s6/sv", "etc/s6/adminsv"] {
            if Path::new(&format!("{}/{}/{}", install_root, base, name)).exists() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Enable an s6 service via the s6-frontend CLI.
///
/// `s6 set enable <service>` adds the service to the default bundle, replacing
/// the old in-house scheme of touching empty files in
/// `/etc/s6/adminsv/default/contents.d/`. The staged change is made persistent
/// by a single [`commit_database`] in the finalize phase.
///
/// Service definitions come from official `-s6` packages (e.g. `seatd-s6`,
/// `iwd-s6`) or are written by deploytix into `/etc/s6/adminsv`.  If no
/// definition is found the corresponding package was not installed and we
/// skip with a warning.
fn enable(cmd: &CommandRunner, service: &str, install_root: &str) -> Result<()> {
    let Some(name) = resolve_service_name(service, install_root) else {
        warn!(
            "Service {} not found under /etc/s6/sv or /etc/s6/adminsv \
             (is the corresponding -s6 package installed?), skipping",
            service
        );
        return Ok(());
    };

    cmd.run_in_chroot(install_root, &format!("s6 set enable {}", name))?;
    info!("Enabled s6 service {}", service);

    Ok(())
}

/// Rebuild the s6-frontend reference database from the service stores.
///
/// `s6 repository sync` must run every time the service definition stores
/// change (services added, removed, or replaced) — otherwise a following
/// `s6 set enable` cannot see the new definition and fails or silently
/// leaves the service out of the set.  Deploytix changes the stores in two
/// ways: pacman installs `-s6` packages into `/etc/s6/sv`, and custom
/// definitions (greetd, zram, hhd, plugin_loader, evdevhook2) are written
/// by hand into `/etc/s6/adminsv`.
fn sync_repository(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Syncing s6 repository (s6 repository sync)");
    cmd.run_in_chroot(install_root, "s6 repository sync")?;
    Ok(())
}

/// Persist pending s6 service changes as the boot database.
///
/// `s6 set enable <service>` stages a change to the default bundle and `s6 set
/// commit` compiles the set — but the compiled database still has to be
/// installed as the boot database, or the installed system boots with whatever
/// the packages shipped instead of the services staged here.  `s6 live install
/// --init` copies the compiled database of the current set to the boot
/// location without touching live s6-rc state (the chroot has none); that is
/// exactly the first-installation case the `--init` flag exists for.
fn commit_database(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Committing s6 service database (s6 set commit)");
    cmd.run_in_chroot(install_root, "s6 set commit")?;

    info!("Installing committed set as the boot database (s6 live install --init)");
    cmd.run_in_chroot(install_root, "s6 live install --init")?;

    Ok(())
}
