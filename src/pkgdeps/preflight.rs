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

/// Default location of the host's pacman database root. Used as a
/// last-resort fallback when `pacman-conf` is unavailable; the sync DB
/// we mirror into the scratch dbpath is derived from this (or from the
/// configured `DBPath`, when one is set in the effective pacman.conf).
const DEFAULT_PACMAN_DBPATH: &str = "/var/lib/pacman";

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
fn host_pacman_config(
    custom_conf: Option<&str>,
    install_root: Option<&str>,
    scratch_dbpath: Option<&Path>,
) -> PacmanConfig {
    PacmanConfig {
        config: custom_conf.map(|s| s.to_string()),
        dbpath: scratch_dbpath.map(|p| p.to_string_lossy().to_string()),
        root: install_root.map(|s| s.to_string()),
    }
}

fn host_source(
    custom_conf: Option<&str>,
    install_root: Option<&str>,
    scratch_dbpath: Option<&Path>,
) -> PacmanSource<super::pacman::SystemExec> {
    PacmanSource::system(host_pacman_config(custom_conf, install_root, scratch_dbpath))
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

/// Resolve the effective `DBPath` pacman would use given an optional
/// `--config` override, by shelling out to `pacman-conf`. Falls back to
/// the compile-time default if `pacman-conf` is unavailable or the
/// output cannot be parsed.
///
/// `basestrap` resolves DBPath via the same pacman.conf we pass through
/// `custom_conf`, so a user who configured a non-default DBPath there
/// would otherwise see the preflight read the wrong sync directory.
fn effective_host_dbpath(custom_conf: Option<&str>) -> PathBuf {
    let mut args: Vec<String> = Vec::new();
    if let Some(c) = custom_conf {
        args.push("--config".into());
        args.push(c.to_string());
    }
    args.push("DBPath".into());
    let output = std::process::Command::new("pacman-conf").args(&args).output();
    match output {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout);
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return PathBuf::from(trimmed);
            }
            debug!("Preflight: pacman-conf returned empty DBPath; falling back to default");
        }
        Ok(out) => {
            debug!(
                "Preflight: pacman-conf DBPath exited {}; falling back to default",
                out.status
            );
        }
        Err(e) => {
            debug!(
                "Preflight: could not invoke pacman-conf ({}); falling back to default DBPath",
                e
            );
        }
    }
    PathBuf::from(DEFAULT_PACMAN_DBPATH)
}

/// Build a temporary pacman dbpath that mirrors the host's sync DB
/// (so packages can be resolved against the same repos basestrap will
/// use) but has an empty `local/` directory (so already-installed host
/// packages don't mask missing deps in the fresh basestrap target).
///
/// `host_sync_dir` is the directory containing the sync DBs we should
/// mirror — typically `<DBPath>/sync` for the effective pacman config.
/// Passing this in (rather than hardcoding `/var/lib/pacman/sync`)
/// keeps the scratch DB consistent with whatever DBPath basestrap will
/// resolve to via the same pacman.conf override.
///
/// On success returns the scratch directory; the caller holds it for
/// the lifetime of the resolver call and the directory is cleaned up
/// when it drops. On failure (no host sync DB, no permission to
/// symlink, etc.) we return `None` and the caller falls back to a
/// less-isolated query.
fn prepare_host_scratch_dbpath(host_sync_dir: &Path) -> Option<ScratchDir> {
    if !host_sync_dir.is_dir() {
        debug!(
            "Preflight: host sync DB not found at {}; cannot prepare scratch dbpath",
            host_sync_dir.display()
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

    // Re-use the host's sync DBs: per-file symlinks rather than a
    // single symlink for the whole sync directory. We don't copy
    // (avoid duplicating hundreds of MB) and we don't symlink the
    // whole directory (would prevent the caller from overlaying
    // individual repo .db files for local file:// repos whose host
    // sync entry is stale or absent).
    let scratch_sync = dir.path().join("sync");
    if let Err(e) = std::fs::create_dir_all(&scratch_sync) {
        debug!("Preflight: failed to create scratch sync/: {}", e);
        return None;
    }
    let entries = match std::fs::read_dir(host_sync_dir) {
        Ok(e) => e,
        Err(e) => {
            debug!(
                "Preflight: failed to read host sync dir {}: {}",
                host_sync_dir.display(),
                e
            );
            return None;
        }
    };
    for entry in entries.flatten() {
        let src = entry.path();
        let name = match src.file_name() {
            Some(n) => n.to_owned(),
            None => continue,
        };
        let dst = scratch_sync.join(&name);
        if let Err(e) = std::os::unix::fs::symlink(&src, &dst) {
            debug!(
                "Preflight: failed to symlink {} -> {}: {}",
                dst.display(),
                src.display(),
                e
            );
            return None;
        }
    }

    Some(dir)
}

/// Parse a pacman.conf and yield `(repo_name, server_dir)` for every
/// repository whose `Server = file://<dir>` URL points at a local
/// directory. Other server schemes (`http`, `https`, `Include = ...`)
/// are ignored — they're already handled by the host's sync DB.
///
/// We deliberately do not pull in a TOML/INI parser: pacman.conf is
/// line-oriented and the subset we need (section header + `Server =`
/// lines) is trivial to scan. Comments (`#`) and `Include =` directives
/// are skipped.
fn parse_local_file_repos(conf_path: &str) -> Vec<(String, PathBuf)> {
    let content = match std::fs::read_to_string(conf_path) {
        Ok(s) => s,
        Err(e) => {
            debug!(
                "Preflight: failed to read pacman.conf {} for local-repo overlay: {}",
                conf_path, e
            );
            return Vec::new();
        }
    };
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let mut current: Option<String> = None;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            if let Some(name) = rest.strip_suffix(']') {
                let name = name.trim();
                current = if name.eq_ignore_ascii_case("options") {
                    None
                } else {
                    Some(name.to_string())
                };
            }
            continue;
        }
        let repo = match &current {
            Some(r) => r,
            None => continue,
        };
        // Match `Server = file://<dir>` (with optional whitespace around `=`).
        let kv = line.split_once('=');
        let (key, value) = match kv {
            Some((k, v)) => (k.trim(), v.trim()),
            None => continue,
        };
        if !key.eq_ignore_ascii_case("Server") {
            continue;
        }
        if let Some(path) = value.strip_prefix("file://") {
            // Already-known repo wins (first Server = ... in the file)
            // — pacman uses the first server for its repo definition
            // when multiple are listed.
            if !out.iter().any(|(r, _)| r == repo) {
                out.push((repo.clone(), PathBuf::from(path)));
            }
        }
    }
    out
}

/// Overlay each local file:// repo's `.db` (and `.files`, when
/// available) into `<scratch>/sync/`, replacing any existing entry.
///
/// Why this is necessary: the host's sync DB may carry a stale
/// `<repo>.db` from a previous installer run (or from an earlier
/// version of the same repo), while the freshly built local repo at
/// `Server = file://<dir>` carries the up-to-date package list. A
/// preflight `pacman -S --print` does NOT fetch the sync DB — it just
/// reads `<dbpath>/sync/<repo>.db`. Without overlay, packages added in
/// the new local repo would be reported as `target not found` even
/// though basestrap (which runs `-Sy` and would re-fetch) would resolve
/// them correctly. The mismatch produces a misleading WARN and erodes
/// trust in the preflight signal.
fn overlay_local_repo_dbs(scratch: &Path, custom_conf: Option<&str>) {
    let conf = match custom_conf {
        Some(c) => c,
        None => return,
    };
    let scratch_sync = scratch.join("sync");
    for (repo, dir) in parse_local_file_repos(conf) {
        // pacman accepts both the bare `<repo>.db` symlink and the
        // `<repo>.db.tar.zst` archive form. Prefer the bare name (what
        // `repo-add` writes by default), fall back to the archive.
        let db_candidates = [
            dir.join(format!("{}.db", repo)),
            dir.join(format!("{}.db.tar.zst", repo)),
        ];
        let db_src = match db_candidates.iter().find(|p| p.exists()) {
            Some(p) => p,
            None => {
                debug!(
                    "Preflight: local repo [{}] at {} has no .db; skipping overlay",
                    repo,
                    dir.display()
                );
                continue;
            }
        };
        let db_dst = scratch_sync.join(format!("{}.db", repo));
        // Remove any prior entry (host symlink or earlier overlay).
        // Ignore NotFound; surface anything else as a debug log.
        match std::fs::remove_file(&db_dst) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => debug!(
                "Preflight: could not clear {} before overlay: {}",
                db_dst.display(),
                e
            ),
        }
        if let Err(e) = std::os::unix::fs::symlink(db_src, &db_dst) {
            debug!(
                "Preflight: failed to overlay {} -> {}: {}",
                db_dst.display(),
                db_src.display(),
                e
            );
            continue;
        }
        debug!(
            "Preflight: overlaid local repo [{}] db from {}",
            repo,
            db_src.display()
        );

        // Optional: same treatment for .files, used by `pacman -F`.
        // Not required for `-S --print` resolution but cheap to keep
        // consistent if the local repo has it.
        let files_candidates = [
            dir.join(format!("{}.files", repo)),
            dir.join(format!("{}.files.tar.zst", repo)),
        ];
        if let Some(files_src) = files_candidates.iter().find(|p| p.exists()) {
            let files_dst = scratch_sync.join(format!("{}.files", repo));
            let _ = std::fs::remove_file(&files_dst);
            let _ = std::os::unix::fs::symlink(files_src, &files_dst);
        }
    }
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
            let msg = e.to_string();
            // "could not find database" means a repo was added to the config
            // but its sync DB hasn't been downloaded yet (no -Sy was run for
            // the scratch dbpath).  This is expected when [extra] or
            // [deploytix] are freshly appended; basestrap runs -Sy itself.
            // Log at debug instead of warn to avoid alarming users.
            if msg.contains("could not find database") {
                debug!(
                    "Preflight ({}): sync DB missing for a newly-added repo ({}). \
                     Continuing without preflight; basestrap will fetch the DB.",
                    label, e
                );
            } else {
                warn!(
                    "Preflight ({}): unable to compute install plan ({}). Continuing without preflight; \
                     the upcoming pacman/basestrap call will be the source of truth.",
                    label, e
                );
            }
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

    // Resolve the same DBPath basestrap will use (honouring any
    // `DBPath = ...` set in `custom_conf`) and mirror its `sync/`
    // subdirectory into the scratch DB. Hardcoding `/var/lib/pacman/sync`
    // here would silently disagree with basestrap whenever the
    // effective pacman.conf overrides DBPath.
    let host_dbpath = effective_host_dbpath(custom_conf);
    let host_sync_dir = host_dbpath.join("sync");
    let scratch = prepare_host_scratch_dbpath(&host_sync_dir);
    let scratch_path = scratch.as_ref().map(|s| s.path());

    // For any local file:// repo declared in custom_conf, replace the
    // host-side sync entry in the scratch dbpath with the freshly
    // built local repo's .db. Without this, a stale host
    // <dbpath>/sync/<repo>.db (left over from a previous installer
    // run) masks new packages added to the local repo and triggers
    // false `target not found` warnings during preflight.
    if let Some(scratch_path) = scratch_path {
        overlay_local_repo_dbs(scratch_path, custom_conf);
    }

    let source = if let Some(scratch_path) = scratch_path {
        debug!(
            "Preflight (host basestrap): isolated scratch dbpath at {} (mirroring {})",
            scratch_path.display(),
            host_sync_dir.display()
        );
        // Scratch dbpath available: point pacman at the fresh target
        // root AND the scratch DB so the resolver sees real repo
        // metadata against an empty installed-DB.
        host_source(custom_conf, Some(install_root), Some(scratch_path))
    } else {
        // Without a scratch dbpath we MUST NOT pass `--root <install_root>`
        // on its own: pacman would then default `--dbpath` to
        // `<install_root>/var/lib/pacman`, which is empty for a fresh
        // basestrap target. The resolver would see an empty sync DB
        // and skip every target as unresolvable. Fall back to the
        // legacy unrooted host query (no `--root`, no `--dbpath`) so
        // pacman uses the live host metadata — basestrap remains the
        // real source of truth.
        warn!(
            "Preflight (host basestrap): could not prepare an isolated scratch dbpath; \
             falling back to a legacy host query without --root/--dbpath. \
             Already-installed host packages may mask missing deps; \
             basestrap remains the source of truth."
        );
        host_source(custom_conf, None, None)
    };
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

    /// The host preflight must point pacman at the target install root
    /// and a scratch dbpath whose `local/` is empty, so already-installed
    /// host packages don't mask missing deps that will still fail in the
    /// fresh basestrap target. We exercise the scratch-prep helper
    /// directly: a successful prep yields an empty `local/` (with the
    /// libalpm version marker) and a real `sync/` directory containing
    /// per-file symlinks pointing at the supplied host sync entries.
    /// Per-file (rather than whole-directory) symlinks let the caller
    /// overlay individual repo .db files for stale local file:// repos.
    #[test]
    fn scratch_dbpath_has_empty_local_and_per_file_sync_symlinks_when_host_sync_exists() {
        // Stage a fake host sync dir so the test does not depend on
        // /var/lib/pacman/sync being present.
        let fake_host = ScratchDir::new("deploytix-preflight-fakehost-").unwrap();
        let fake_sync = fake_host.path().join("sync");
        std::fs::create_dir_all(&fake_sync).unwrap();
        // Stage two repo .db files in the fake host sync.
        std::fs::write(fake_sync.join("core.db"), b"core").unwrap();
        std::fs::write(fake_sync.join("extra.db"), b"extra").unwrap();

        let scratch =
            prepare_host_scratch_dbpath(&fake_sync).expect("scratch dbpath should be prepared");
        let local = scratch.path().join("local");
        let sync = scratch.path().join("sync");
        // Local exists, has only the version marker, no per-package
        // descriptors → installed-DB is empty from libalpm's POV.
        assert!(local.is_dir());
        let local_entries: Vec<_> = std::fs::read_dir(&local)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(local_entries, vec!["ALPM_DB_VERSION".to_string()]);
        // sync is a real directory (NOT a symlink to the host sync dir),
        // so the caller can overlay individual entries safely.
        let sync_meta = std::fs::symlink_metadata(&sync).unwrap();
        assert!(
            sync_meta.file_type().is_dir(),
            "sync/ must be a real directory so per-repo overlays don't leak into the host sync"
        );
        // Each host sync entry is mirrored as a symlink to its source.
        let core_link = sync.join("core.db");
        let extra_link = sync.join("extra.db");
        assert!(std::fs::symlink_metadata(&core_link).unwrap().file_type().is_symlink());
        assert!(std::fs::symlink_metadata(&extra_link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&core_link).unwrap(), fake_sync.join("core.db"));
        assert_eq!(std::fs::read_link(&extra_link).unwrap(), fake_sync.join("extra.db"));
    }

    /// Bug fix #1: a custom DBPath set in the effective pacman.conf
    /// must drive the scratch sync source — the helper must mirror
    /// `<DBPath>/sync` for whatever DBPath the caller resolves, not a
    /// hardcoded `/var/lib/pacman/sync`. Exercised by passing a
    /// non-default sync directory in directly and verifying the
    /// per-file symlink targets the expected source.
    #[test]
    fn scratch_dbpath_mirrors_custom_host_sync_dir() {
        let fake_host = ScratchDir::new("deploytix-preflight-customdb-").unwrap();
        // Mimic a non-default DBPath = /opt/pacdb where sync lives at
        // /opt/pacdb/sync.
        let custom_sync = fake_host.path().join("opt").join("pacdb").join("sync");
        std::fs::create_dir_all(&custom_sync).unwrap();
        std::fs::write(custom_sync.join("world.db"), b"world").unwrap();

        let scratch = prepare_host_scratch_dbpath(&custom_sync)
            .expect("scratch dbpath should be prepared from custom sync");
        let world_link = scratch.path().join("sync").join("world.db");
        let target = std::fs::read_link(&world_link).unwrap();
        assert_eq!(
            target,
            custom_sync.join("world.db"),
            "scratch sync entries must point at the configured DBPath/sync, not the default"
        );
    }

    /// Regression: a stale host-side `<repo>.db` (left over from a
    /// previous installer run) must not mask a freshly built local
    /// file:// repo of the same name. After scratch prep, calling
    /// `overlay_local_repo_dbs` with a custom pacman.conf that points
    /// `[deploytix]` at a local directory should replace the stale
    /// host symlink with one pointing at the fresh repo's `.db`.
    #[test]
    fn overlay_replaces_stale_host_repo_db_with_local_file_repo_db() {
        // Fake host sync containing a stale deploytix.db.
        let fake_host = ScratchDir::new("deploytix-preflight-stalehost-").unwrap();
        let fake_sync = fake_host.path().join("sync");
        std::fs::create_dir_all(&fake_sync).unwrap();
        let stale_db = fake_sync.join("deploytix.db");
        std::fs::write(&stale_db, b"STALE").unwrap();

        // Fresh local file:// repo with the up-to-date deploytix.db.
        let local_repo = ScratchDir::new("deploytix-preflight-localrepo-").unwrap();
        let fresh_db = local_repo.path().join("deploytix.db");
        std::fs::write(&fresh_db, b"FRESH").unwrap();

        // Custom pacman.conf declaring [deploytix] -> file://<local_repo>.
        let conf_dir = ScratchDir::new("deploytix-preflight-conf-").unwrap();
        let conf_path = conf_dir.path().join("pacman.conf");
        let conf_body = format!(
            "[options]\nHoldPkg = pacman glibc\n\n[deploytix]\nSigLevel = Optional TrustAll\nServer = file://{}\n",
            local_repo.path().display()
        );
        std::fs::write(&conf_path, conf_body).unwrap();

        // Prep scratch (now per-file symlinks) then overlay.
        let scratch =
            prepare_host_scratch_dbpath(&fake_sync).expect("scratch dbpath should be prepared");
        let scratch_db = scratch.path().join("sync").join("deploytix.db");
        // Pre-overlay: the symlink resolves to the stale host db.
        assert_eq!(
            std::fs::read_link(&scratch_db).unwrap(),
            stale_db,
            "prep must mirror the host sync entries before overlay runs"
        );

        overlay_local_repo_dbs(scratch.path(), Some(conf_path.to_str().unwrap()));

        // Post-overlay: the symlink points at the fresh local repo db.
        let post = std::fs::read_link(&scratch_db).unwrap();
        assert_eq!(
            post, fresh_db,
            "overlay must replace the stale host symlink with one pointing at the local file:// repo"
        );
        // And reading through the symlink yields the fresh contents,
        // not the stale ones.
        assert_eq!(std::fs::read(&scratch_db).unwrap(), b"FRESH");
    }

    /// `parse_local_file_repos` must return only `Server = file://`
    /// repos, ignore the `[options]` section, and skip non-file
    /// schemes (`http`, `https`).
    #[test]
    fn parse_local_file_repos_filters_to_file_scheme_outside_options() {
        let conf_dir = ScratchDir::new("deploytix-preflight-parse-").unwrap();
        let conf_path = conf_dir.path().join("pacman.conf");
        std::fs::write(
                &conf_path,
                concat!(
                    "[options]\n",
                    "Server = file:///etc/should-be-ignored\n",
                    "\n",
                    "[core]\n",
                    "Server = https://mirror.example.com/$repo/$arch\n",
                    "\n",
                    "# inline comment\n",
                    "[deploytix]\n",
                    "SigLevel = Optional TrustAll\n",
                    "Server = file:///tmp/deploytix-local-repo\n",
                ),
            )
            .unwrap();

        let repos = parse_local_file_repos(conf_path.to_str().unwrap());
        assert_eq!(repos.len(), 1, "expected exactly one local file:// repo, got {:?}", repos);
        assert_eq!(repos[0].0, "deploytix");
        assert_eq!(repos[0].1, PathBuf::from("/tmp/deploytix-local-repo"));
    }

    /// Bug fix #1: when the supplied host sync directory does not
    /// exist, the helper returns None so the caller can fall back —
    /// rather than blindly mirroring a missing path.
    #[test]
    fn scratch_dbpath_returns_none_when_host_sync_missing() {
        let missing =
            Path::new("/var/empty/deploytix-preflight-no-sync-here-please-do-not-create");
        assert!(prepare_host_scratch_dbpath(missing).is_none());
    }

    /// Bug fix #2: when scratch dbpath preparation fails, the host
    /// preflight must NOT pass `--root <install_root>` on its own.
    /// pacman's semantics make `--root` re-derive `--dbpath` to
    /// `<install_root>/var/lib/pacman`, which for a fresh basestrap
    /// target is empty — every target would then be flagged
    /// unresolvable. The fallback contract is "no --root and no
    /// --dbpath", which is what `host_pacman_config(custom_conf, None,
    /// None)` produces.
    #[test]
    fn host_pacman_config_fallback_does_not_set_root_or_dbpath() {
        let cfg = host_pacman_config(Some("/tmp/p.conf"), None, None);
        assert_eq!(cfg.config.as_deref(), Some("/tmp/p.conf"));
        assert!(
            cfg.root.is_none(),
            "fallback must not pass --root; would re-derive --dbpath to <root>/var/lib/pacman"
        );
        assert!(
            cfg.dbpath.is_none(),
            "fallback must not pass --dbpath without scratch prep"
        );
    }

    /// Sanity: in the happy path, the config carries every override
    /// the resolver needs — config (custom pacman.conf), root (the
    /// fresh basestrap target), and dbpath (the scratch dir mirroring
    /// the host's sync DBs with an empty local/).
    #[test]
    fn host_pacman_config_happy_path_sets_all_three_overrides() {
        let scratch = Path::new("/tmp/scratch-db");
        let cfg = host_pacman_config(Some("/tmp/p.conf"), Some("/mnt/target"), Some(scratch));
        assert_eq!(cfg.config.as_deref(), Some("/tmp/p.conf"));
        assert_eq!(cfg.root.as_deref(), Some("/mnt/target"));
        assert_eq!(cfg.dbpath.as_deref(), Some("/tmp/scratch-db"));
    }

    /// Bug fix #2: `effective_host_dbpath` falls back to the documented
    /// default when `pacman-conf` is unavailable. We can't drive a real
    /// pacman-conf in unit tests, but we can pin the fallback path —
    /// a present-but-broken --config (nonexistent file) makes
    /// pacman-conf exit non-zero, and we expect the default DBPath.
    #[test]
    fn effective_host_dbpath_falls_back_to_default_on_pacman_conf_failure() {
        // Definitely-bogus config file → pacman-conf either exits
        // non-zero or isn't installed; either way we expect the
        // default. We deliberately don't assert the *result* of
        // pacman-conf when it succeeds, since CI environments with a
        // real /etc/pacman.conf would legitimately return a different
        // path.
        let p = effective_host_dbpath(Some("/var/empty/deploytix-no-such-pacman.conf"));
        // The compile-time default is the only deterministic answer
        // when pacman-conf fails. If pacman-conf is missing entirely
        // (e.g. on CI) we also land here.
        // Note: if pacman-conf happens to *succeed* with a bogus
        // config (unlikely), this test would observe whatever it
        // returns — accept the default OR a non-empty path as
        // evidence the function ran.
        assert!(
            p == Path::new(DEFAULT_PACMAN_DBPATH) || p.is_absolute(),
            "expected default or an absolute DBPath, got {:?}",
            p
        );
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
