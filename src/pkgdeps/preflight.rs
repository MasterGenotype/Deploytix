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
use std::path::{Path, PathBuf};
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

/// Default location of the host's pacman sync database. Used as the
/// source we mirror into the scratch dbpath so the preflight can see
/// every package basestrap will see.
const HOST_SYNC_DIR: &str = "/var/lib/pacman/sync";

/// Build a [`PacmanSource`] for the host basestrap preflight.
///
/// `custom_conf` is the optional pacman.conf override basestrap will be
/// invoked with (the temporary one with the [deploytix] / [extra]
/// repos appended, when applicable).
///
/// `install_root` is the target directory basestrap will populate. We
/// resolve against this empty target rather than the live host so that
/// packages already installed on the live ISO don't mask dependency
/// problems that will still fail during basestrap in the fresh root.
///
/// `scratch_dbpath` (when `Some`) is a temporary dbpath that contains
/// the host's sync DBs but an empty `local/` — pointing pacman at it
/// gives the resolver full repo metadata while pretending nothing is
/// installed. When `None`, we fall back to leaving `--dbpath` unset
/// (i.e. legacy behaviour) so the call still produces something rather
/// than failing outright in environments where the scratch dir could
/// not be prepared.
fn host_source(
    custom_conf: Option<&str>,
    install_root: Option<&str>,
    scratch_dbpath: Option<&Path>,
) -> PacmanSource<super::pacman::SystemExec> {
    PacmanSource::system(PacmanConfig {
        config: custom_conf.map(|s| s.to_string()),
        dbpath: scratch_dbpath.map(|p| p.to_string_lossy().to_string()),
        root: install_root.map(|s| s.to_string()),
    })
}

/// Self-cleaning scratch directory. The directory is created in
/// `std::env::temp_dir()` and is recursively removed when this struct
/// is dropped. We don't pull in the `tempfile` crate just for this —
/// the requirements are minimal (one process, one path, no rename
/// semantics) and rolling it inline keeps the dependency footprint
/// small.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(prefix: &str) -> std::io::Result<Self> {
        // Combine PID + nanosecond clock for a per-process unique name.
        // This is good enough for a scratch dir — it does not need to
        // be cryptographically secure.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let name = format!("{}{}-{}", prefix, pid, nanos);
        let path = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        // Best-effort cleanup; nothing useful to do if it fails (e.g.
        // the dir was already removed).
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Build a temporary pacman dbpath that mirrors the host's sync DB
/// (so packages can be resolved against the same repos basestrap will
/// use) but has an empty `local/` directory (so already-installed host
/// packages don't mask missing deps in the fresh basestrap target).
///
/// On success returns the scratch directory; the caller holds it for
/// the lifetime of the resolver call and the directory is cleaned up
/// when it drops. On failure (no host sync DB, no permission to
/// symlink, etc.) we return `None` and the caller falls back to a
/// less-isolated query.
fn prepare_host_scratch_dbpath() -> Option<ScratchDir> {
    let host_sync = Path::new(HOST_SYNC_DIR);
    if !host_sync.is_dir() {
        debug!(
            "Preflight: host sync DB not found at {}; cannot prepare scratch dbpath",
            HOST_SYNC_DIR
        );
        return None;
    }

    let dir = match ScratchDir::new("deploytix-preflight-db-") {
        Ok(d) => d,
        Err(e) => {
            debug!("Preflight: failed to create scratch dbpath: {}", e);
            return None;
        }
    };

    // Empty local/ — this is what makes pacman treat the target as a
    // fresh root regardless of what the live host has installed.
    let local_dir: PathBuf = dir.path().join("local");
    if let Err(e) = std::fs::create_dir_all(&local_dir) {
        debug!("Preflight: failed to create scratch local/: {}", e);
        return None;
    }
    // libalpm checks for `ALPM_DB_VERSION` when reading the local DB.
    // Match the format pacman writes: a single number plus newline.
    if let Err(e) = std::fs::write(local_dir.join("ALPM_DB_VERSION"), "9\n") {
        debug!("Preflight: failed to write ALPM_DB_VERSION: {}", e);
        return None;
    }

    // Re-use the host's sync DBs: symlink rather than copy so we don't
    // duplicate hundreds of MB. Linux-only (the target platform).
    let scratch_sync = dir.path().join("sync");
    if let Err(e) = std::os::unix::fs::symlink(host_sync, &scratch_sync) {
        debug!(
            "Preflight: failed to symlink host sync DB into scratch: {}",
            e
        );
        return None;
    }

    Some(dir)
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
/// `install_root` is the directory basestrap is about to populate. We
/// point pacman's `--root` at this fresh target and `--dbpath` at a
/// scratch directory that mirrors the host's sync DBs but starts with
/// an empty `local/`. The result is that the resolver sees the same
/// repo metadata basestrap will see while pretending the target has
/// nothing installed yet — so dependency problems that would only
/// manifest in the empty basestrap root are caught up front instead
/// of being masked by packages that happen to be on the live ISO host.
///
/// In dry-run mode we skip the resolver entirely — basestrap won't run
/// either, and the scratch dir mutation would be wasted.
pub fn preflight_host(
    custom_conf: Option<&str>,
    install_root: &str,
    packages: &[String],
    dry_run: bool,
) -> Result<PreflightReport> {
    if dry_run {
        info!("[dry-run] Skipping host basestrap dependency preflight");
        return Ok(PreflightReport::skipped_reason("dry-run"));
    }

    // Build a scratch dbpath so the resolver sees an empty installed-DB
    // even though the host live ISO has plenty of packages. If we can't
    // build one (no /var/lib/pacman/sync, no /tmp write access, etc.),
    // log and continue with whatever pacman would resolve normally —
    // basestrap is still the source of truth.
    let scratch = prepare_host_scratch_dbpath();
    let scratch_path = scratch.as_ref().map(|s| s.path());
    if scratch_path.is_none() {
        warn!(
            "Preflight (host basestrap): could not prepare an isolated scratch dbpath; \
             resolution will use the live host's installed-DB and may mask missing deps. \
             basestrap remains the source of truth."
        );
    } else {
        debug!(
            "Preflight (host basestrap): isolated scratch dbpath at {}",
            scratch_path.unwrap().display()
        );
    }

    let source = host_source(custom_conf, Some(install_root), scratch_path);
    // basestrap installs into a fresh root and our scratch dbpath has
    // an empty local/, so already-installed host packages won't be
    // skipped. Use clean_root=true so the plan also disregards any
    // installed-DB residue and reflects everything pacman will
    // actually need to download into the target.
    let result = resolve(&source, packages, true, "host basestrap");
    // Keep `scratch` alive until after `resolve` returns, then drop —
    // the explicit drop documents the lifetime tie even though it is
    // implicit otherwise.
    drop(scratch);
    result
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
        let r = preflight_host(None, "/mnt", &["base".to_string()], true).unwrap();
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

    /// Bug fix #2: the host preflight must point pacman at the target
    /// install root and a scratch dbpath whose `local/` is empty, so
    /// already-installed host packages don't mask missing deps that
    /// will still fail in the fresh basestrap target. We exercise the
    /// scratch-prep helper directly: a successful prep yields an empty
    /// `local/` (with the libalpm version marker) and a `sync` symlink
    /// pointing at the host's sync DB.
    #[test]
    fn scratch_dbpath_has_empty_local_and_sync_symlink_when_host_sync_exists() {
        // Skip in environments without /var/lib/pacman/sync (most CI).
        if !Path::new(HOST_SYNC_DIR).is_dir() {
            return;
        }
        let scratch =
            prepare_host_scratch_dbpath().expect("scratch dbpath should be prepared");
        let local = scratch.path().join("local");
        let sync = scratch.path().join("sync");
        // Local exists, has only the version marker, no per-package
        // descriptors → installed-DB is empty from libalpm's POV.
        assert!(local.is_dir());
        let entries: Vec<_> = std::fs::read_dir(&local)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["ALPM_DB_VERSION".to_string()]);
        // sync is a symlink to the host's sync DB.
        let meta = std::fs::symlink_metadata(&sync).unwrap();
        assert!(meta.file_type().is_symlink());
        let target = std::fs::read_link(&sync).unwrap();
        assert_eq!(target, Path::new(HOST_SYNC_DIR));
    }

    /// Bug fix #2: when we cannot prepare a scratch dbpath (e.g. the
    /// host has no sync DB at all), `prepare_host_scratch_dbpath`
    /// returns `None` so the preflight can fall back gracefully. We
    /// can't easily fake this on a real host, but we CAN at least
    /// observe that the function returns a valid result without
    /// panicking and that, when it returns Some, the path is a
    /// directory.
    #[test]
    fn scratch_dbpath_is_dir_when_returned() {
        if let Some(s) = prepare_host_scratch_dbpath() {
            assert!(s.path().is_dir());
        }
    }

    /// Bug fix #2: ScratchDir cleans up on drop. Sanity-check the
    /// invariant — important because we rely on this rather than the
    /// `tempfile` crate.
    #[test]
    fn scratch_dir_is_removed_on_drop() {
        let path: PathBuf;
        {
            let s = ScratchDir::new("deploytix-preflight-test-").unwrap();
            path = s.path().to_path_buf();
            std::fs::write(path.join("marker"), b"hi").unwrap();
            assert!(path.is_dir());
        }
        assert!(!path.exists(), "scratch dir was not cleaned up: {:?}", path);
    }
}
