//! Dependency-resolution preflight for `basestrap` and chroot `pacman`
//! operations.
//!
//! Both basestrap (run on the host against the live system's pacman
//! configuration) and `pacman -S` invocations inside the chroot ultimately
//! ask libalpm to resolve a transaction. When that resolution fails —
//! missing virtual provider, target not in any sync DB, conflict the user
//! must resolve interactively — we want to know *before* the transaction
//! starts, while we still have a clean failure surface.
//!
//! This module wraps [`MetadataSource::install_plan`] (i.e. `pacman -S
//! --print`) with logging that surfaces unresolvable targets and
//! conflict-driven removals. It is best-effort: if pacman/expac aren't
//! available (e.g. running outside the live ISO during development) or the
//! sync DB is empty, we log a warning and let the real `basestrap` /
//! `pacman` invocation be the source of truth. We don't want to block an
//! install because the metadata layer disagrees with what pacman would
//! actually do.
//!
//! Two entry points:
//! * [`preflight_host`] — used before host `basestrap` runs. Reads the
//!   host's pacman.conf (optionally a temporary one with the [deploytix]
//!   / [extra] repos appended) and resolves against the host's sync DB.
//! * [`preflight_chroot`] — used before any chroot `pacman -S`. Points
//!   pacman at `<install_root>` via `--root` / `--config` so that the
//!   resolution sees the chroot's pacman.conf and pacman state DB.

use super::pacman::{PacmanConfig, PacmanSource};
use super::source::MetadataSource;
use crate::utils::error::Result;
use tracing::{debug, info, warn};

/// Outcome of a preflight resolution. Fields mirror the parts of
/// [`super::model::InstallPlan`] callers actually act on so the call
/// sites don't need to know about the full model crate.
#[derive(Debug, Clone, Default)]
pub struct PreflightReport {
    /// Number of packages pacman would install (after resolving deps,
    /// virtual providers, replacements). 0 means resolution failed or the
    /// sync DB hasn't seen these targets yet.
    pub planned_install_count: usize,
    /// Targets the resolver could not resolve at all. Empty means every
    /// requested package is reachable in the sync database. Non-empty is
    /// a strong signal that the upcoming pacman/basestrap call will fail
    /// with `target not found`.
    pub unresolved: Vec<String>,
    /// Conflict-driven removals reported by pacman. We surface these so
    /// an unattended install doesn't silently delete an installed package
    /// the user didn't expect to lose.
    pub to_remove: Vec<String>,
    /// Free-form warnings from the metadata layer (stale sync DB,
    /// missing repos, etc.).
    pub warnings: Vec<String>,
    /// True when the preflight could not run at all (pacman/expac not
    /// available, dry-run mode, etc.). The caller should treat this as
    /// "didn't preflight" rather than "preflight passed".
    pub skipped: bool,
}

impl PreflightReport {
    /// `true` when no unresolvable targets were observed. Conflicts are
    /// not considered failures here — pacman handles them, we only
    /// report.
    pub fn is_resolvable(&self) -> bool {
        self.unresolved.is_empty()
    }

    fn skipped_reason(reason: &str) -> Self {
        Self {
            warnings: vec![reason.to_string()],
            skipped: true,
            ..Default::default()
        }
    }
}

/// Build a [`PacmanSource`] for preflight against the host's sync DB,
/// optionally with a custom pacman.conf (the same `-C <conf>` path that
/// will be passed to basestrap).
fn host_source(custom_conf: Option<&str>) -> PacmanSource<super::pacman::SystemExec> {
    PacmanSource::system(PacmanConfig {
        config: custom_conf.map(|s| s.to_string()),
        dbpath: None,
        root: None,
    })
}

/// Build a [`PacmanSource`] that resolves against the chroot's pacman
/// state. `--root` makes pacman treat `install_root` as `/`; `--config`
/// (and the implied `--dbpath`) make it read the chroot's
/// `/etc/pacman.conf` and `/var/lib/pacman` instead of the host's.
fn chroot_source(install_root: &str) -> PacmanSource<super::pacman::SystemExec> {
    let conf = format!("{}/etc/pacman.conf", install_root);
    let dbpath = format!("{}/var/lib/pacman", install_root);
    PacmanSource::system(PacmanConfig {
        config: Some(conf),
        dbpath: Some(dbpath),
        root: Some(install_root.to_string()),
    })
}

/// Resolve `packages` against `source` and emit a [`PreflightReport`].
///
/// `clean_root=true` tells pacman to ignore the local installed-DB when
/// computing the plan — appropriate for basestrap (target system has no
/// installed packages yet) and for chroot operations that want to see
/// every transitive dep regardless of whether the host already has it.
///
/// This function NEVER returns `Err` for ordinary pacman failures. Any
/// command failure is folded into `PreflightReport::skipped = true` with
/// a warning, so the caller can log and continue. The only `Err` return
/// is signal interruption / I/O the caller cares about — callers can
/// safely use `?` here.
pub fn resolve(
    source: &dyn MetadataSource,
    packages: &[String],
    clean_root: bool,
    label: &str,
) -> Result<PreflightReport> {
    if packages.is_empty() {
        return Ok(PreflightReport::default());
    }

    let targets: Vec<&str> = packages.iter().map(|s| s.as_str()).collect();
    debug!(
        "Preflight ({}): resolving {} target(s) clean_root={}",
        label,
        targets.len(),
        clean_root
    );

    // `pacman -S --print` — a real transaction simulation. If pacman is
    // missing or the sync DB is empty this returns Err; treat it as
    // "skipped" rather than fatal so basestrap remains the source of
    // truth.
    let plan = match source.install_plan(&targets, clean_root) {
        Ok(p) => p,
        Err(e) => {
            warn!(
                "Preflight ({}): unable to compute install plan ({}). Continuing without preflight; \
                 the upcoming pacman/basestrap call will be the source of truth.",
                label, e
            );
            return Ok(PreflightReport::skipped_reason(&format!(
                "preflight skipped: {}",
                e
            )));
        }
    };

    // Targets that were requested but never appeared in the install
    // list are unresolvable — pacman will error with "target not
    // found". We compute this from the resolver output rather than
    // re-grepping pacman stderr because the `MockSource` path doesn't
    // produce real pacman errors.
    let resolved_names: std::collections::BTreeSet<&str> =
        plan.to_install.iter().map(|p| p.name.as_str()).collect();
    let unresolved: Vec<String> = targets
        .iter()
        .filter(|t| !resolved_names.contains(*t))
        .map(|s| (*s).to_string())
        .collect();

    let report = PreflightReport {
        planned_install_count: plan.to_install.len(),
        unresolved: unresolved.clone(),
        to_remove: plan.to_remove.clone(),
        warnings: plan.warnings.clone(),
        skipped: false,
    };

    info!(
        "Preflight ({}): {} target(s) → {} package(s) to install, {} to remove",
        label,
        targets.len(),
        report.planned_install_count,
        report.to_remove.len()
    );
    if !report.unresolved.is_empty() {
        warn!(
            "Preflight ({}): {} unresolvable target(s): {}. \
             The upcoming pacman/basestrap call is likely to fail with `target not found`.",
            label,
            report.unresolved.len(),
            report.unresolved.join(", ")
        );
    }
    if !report.to_remove.is_empty() {
        warn!(
            "Preflight ({}): pacman would remove {} existing package(s) due to conflicts/replacements: {}",
            label,
            report.to_remove.len(),
            report.to_remove.join(", ")
        );
    }
    for w in &report.warnings {
        warn!("Preflight ({}): {}", label, w);
    }

    Ok(report)
}

/// Preflight the host basestrap transaction.
///
/// `custom_conf` is the optional `-C <path>` value basestrap will be
/// invoked with (e.g. the temporary pacman.conf with the [deploytix]
/// repo appended). Pass `None` when basestrap will use the system's
/// `/etc/pacman.conf` directly.
///
/// In dry-run mode we skip the resolver entirely — basestrap won't run
/// either, and pacman -Sy on the host would mutate state.
pub fn preflight_host(
    custom_conf: Option<&str>,
    packages: &[String],
    dry_run: bool,
) -> Result<PreflightReport> {
    if dry_run {
        info!("[dry-run] Skipping host basestrap dependency preflight");
        return Ok(PreflightReport::skipped_reason("dry-run"));
    }
    let source = host_source(custom_conf);
    // basestrap installs into a fresh root → use clean_root semantics so
    // the plan reflects everything pacman would download (not just
    // what's missing on the live ISO host).
    resolve(&source, packages, true, "host basestrap")
}

/// Preflight a `pacman -S <packages>` call that will run inside the
/// chroot. Resolves against the chroot's pacman.conf and dbpath so
/// already-installed packages (i.e. those basestrap put down) are
/// correctly skipped.
///
/// In dry-run mode we skip the resolver — the chroot won't be populated.
pub fn preflight_chroot(
    install_root: &str,
    packages: &[String],
    dry_run: bool,
) -> Result<PreflightReport> {
    if dry_run {
        info!("[dry-run] Skipping chroot pacman dependency preflight");
        return Ok(PreflightReport::skipped_reason("dry-run"));
    }
    // The chroot's pacman state needs to exist before we can query it.
    // basestrap creates it, so this is normally fine; if it doesn't
    // (e.g. preflight called too early), `--root` will make pacman
    // bail and we'll fold that into "skipped".
    let conf_path = format!("{}/etc/pacman.conf", install_root);
    if !std::path::Path::new(&conf_path).exists() {
        let msg = format!(
            "chroot pacman.conf not found at {}; preflight skipped",
            conf_path
        );
        debug!("{}", msg);
        return Ok(PreflightReport::skipped_reason(&msg));
    }

    let source = chroot_source(install_root);
    // For chroot operations we want pacman to consult the *chroot's*
    // installed DB, so already-present packages (base, deps from prior
    // pacman -S in this same install) are correctly excluded. Hence
    // clean_root=false.
    resolve(&source, packages, false, "chroot pacman")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkgdeps::model::{Dep, Package};
    use crate::pkgdeps::source::MockSource;

    fn pkg(name: &str, version: &str, deps: &[&str]) -> Package {
        let mut p = Package::new(name, version, "world");
        p.depends = deps.iter().map(|d| Dep::parse(d)).collect();
        p
    }

    fn universe() -> MockSource {
        let mut s = MockSource::default();
        s.set_databases(vec!["world".into()]);
        s.insert(pkg("base", "1.0", &["glibc"]));
        s.insert(pkg("glibc", "2.39", &[]));
        s.insert(pkg("linux-zen", "6.10", &[]));
        // virtual provider
        let mut bash = pkg("bash", "5.2", &[]);
        bash.provides = vec![Dep::unversioned("sh")];
        s.insert(bash);
        // package with a conflict
        let mut foo = pkg("foo", "1.0", &[]);
        foo.conflicts = vec![Dep::unversioned("oldfoo")];
        s.insert(foo);
        s
    }

    #[test]
    fn empty_targets_short_circuits_to_default_report() {
        let s = universe();
        let r = resolve(&s, &[], true, "test").unwrap();
        assert!(!r.skipped);
        assert!(r.is_resolvable());
        assert_eq!(r.planned_install_count, 0);
        assert!(r.unresolved.is_empty());
    }

    #[test]
    fn resolves_known_targets() {
        let s = universe();
        let r = resolve(
            &s,
            &["base".to_string(), "linux-zen".to_string()],
            true,
            "test",
        )
        .unwrap();
        assert!(!r.skipped);
        assert!(r.is_resolvable(), "unresolved: {:?}", r.unresolved);
        // base + glibc + linux-zen (all clean_root)
        assert!(r.planned_install_count >= 3);
    }

    #[test]
    fn unresolvable_target_is_reported() {
        let s = universe();
        let r = resolve(
            &s,
            &["base".to_string(), "does-not-exist".to_string()],
            true,
            "test",
        )
        .unwrap();
        assert!(!r.is_resolvable());
        assert!(r.unresolved.iter().any(|u| u == "does-not-exist"));
    }

    #[test]
    fn virtual_provider_not_marked_unresolved() {
        // Build a universe where a package depends on `sh` (virtual). The
        // resolver must include the real provider (bash) and the
        // requested target itself must not be in `unresolved`.
        let mut s = MockSource::default();
        s.set_databases(vec!["world".into()]);
        let mut needs_sh = Package::new("needs-sh", "1.0", "world");
        needs_sh.depends = vec![Dep::unversioned("sh")];
        s.insert(needs_sh);
        let mut bash = Package::new("bash", "5.2", "world");
        bash.provides = vec![Dep::unversioned("sh")];
        s.insert(bash);

        let r = resolve(&s, &["needs-sh".to_string()], true, "test").unwrap();
        assert!(r.is_resolvable());
        assert!(r.unresolved.is_empty());
        // bash should be in the plan as the resolved provider.
        assert!(r.planned_install_count >= 2);
    }

    #[test]
    fn conflict_driven_removal_is_surfaced() {
        let mut s = universe();
        // Mark `oldfoo` as installed; planning `foo` (which conflicts
        // with oldfoo) should produce a removal entry.
        s.insert(pkg("oldfoo", "0.9", &[]));
        s.mark_installed("oldfoo");
        let r = resolve(&s, &["foo".to_string()], false, "test").unwrap();
        assert!(r.is_resolvable());
        assert!(
            r.to_remove.iter().any(|n| n == "oldfoo"),
            "expected oldfoo in to_remove, got {:?}",
            r.to_remove
        );
    }

    #[test]
    fn report_skipped_when_dry_run_host() {
        let r = preflight_host(None, &["base".to_string()], true).unwrap();
        assert!(r.skipped);
        assert_eq!(r.planned_install_count, 0);
    }

    #[test]
    fn report_skipped_when_dry_run_chroot() {
        let r = preflight_chroot("/nonexistent", &["base".to_string()], true).unwrap();
        assert!(r.skipped);
    }

    #[test]
    fn chroot_preflight_skips_when_install_root_missing() {
        // Live mode (dry_run=false) but install_root has no pacman.conf
        // → preflight reports skipped instead of erroring.
        let r = preflight_chroot(
            "/var/empty/definitely-not-an-install-root",
            &["base".to_string()],
            false,
        )
        .unwrap();
        assert!(r.skipped);
        assert!(r
            .warnings
            .iter()
            .any(|w| w.contains("pacman.conf not found")));
    }
}
