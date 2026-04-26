//! Production [`MetadataSource`] backed by pacman / pactree / expac.
//!
//! All shell-out goes through the small [`CmdExec`] trait so tests can
//! drive the parser without pacman being installed. The real runner is
//! [`SystemExec`], which uses [`std::process::Command`] and respects
//! optional pacman config / dbpath / root overrides.
//!
//! Output parsing notes:
//! * `pacman -Si <pkg>` is consumed for metadata (multi-line `Field : value`).
//! * `pactree -s <pkg>` (one name per line) gives the dependency list,
//!   but `-Si` has the version constraints — so we use both.
//! * `pacman -Qq <pkg>` (`-Q`, not `-S`) tells us whether the package is
//!   currently installed.
//! * `pacman -S --print --print-format '%r/%n %v'` is the transaction
//!   resolver — equivalent to `pacman -S` minus the actual download.

use super::model::{Dep, InstallPlan, Package, PlannedPackage};
use super::source::MetadataSource;
use crate::utils::error::{DeploytixError, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use tracing::debug;

/// Explicit list delimiter passed to `expac -l`. We pick ASCII unit
/// separator (`U+001F`) because:
///   - it is reserved precisely for "subfield separator" in the spec,
///   - it cannot legally appear in pacman package names, version
///     constraints, or human-readable optdep descriptions,
///   - it survives shell argument passing (we go through
///     `std::process::Command`, not a shell), and
///   - it is not the documented two-space default for `%O`/`%S`, so
///     the parser is unambiguous regardless of expac version drift.
pub(crate) const EXPAC_LIST_DELIM: &str = "\x1f";

/// Split an expac list-field value emitted with `-l <EXPAC_LIST_DELIM>`
/// into its constituent records. Falls back to the documented default
/// two-space delimiter for `%O`/`%S` if no unit-separator characters
/// are present (older expac that ignores `-l`, or a custom build that
/// emits with the default delimiter regardless). The two-space split
/// is per `expac(1)` for list fields and is robust to single spaces
/// inside descriptions like `git: for AUR helpers`.
pub(crate) fn split_expac_list(field: &str) -> Vec<&str> {
    let trimmed = field.trim_matches(|c: char| c == '\n' || c == '\r');
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.contains(EXPAC_LIST_DELIM) {
        return trimmed
            .split(EXPAC_LIST_DELIM)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
    }
    // Default expac list delimiter for %O/%S is two spaces.
    trimmed
        .split("  ")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Indirection so tests can mock pacman/pactree output without invoking
/// the real binaries.
pub trait CmdExec: Send + Sync {
    /// Run a command and return its stdout, or an error containing
    /// stderr if the command exited non-zero.
    fn run(&self, program: &str, args: &[String]) -> Result<String>;
}

/// Default executor — runs the real binary via `std::process::Command`.
#[derive(Debug, Default)]
pub struct SystemExec;

impl CmdExec for SystemExec {
    fn run(&self, program: &str, args: &[String]) -> Result<String> {
        debug!("pkgdeps exec: {} {}", program, args.join(" "));
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    DeploytixError::CommandNotFound(program.to_string())
                } else {
                    DeploytixError::Io(e)
                }
            })?;
        if !output.status.success() {
            return Err(DeploytixError::CommandFailed {
                command: format!("{} {}", program, args.join(" ")),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// pacman/pactree options that need to be threaded through every
/// invocation. All fields are optional — when unset, pacman falls back
/// to the system defaults (`/etc/pacman.conf`, `/var/lib/pacman`, `/`).
#[derive(Debug, Clone, Default)]
pub struct PacmanConfig {
    pub config: Option<String>,
    pub dbpath: Option<String>,
    pub root: Option<String>,
}

impl PacmanConfig {
    fn extra_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(c) = &self.config {
            args.push("--config".into());
            args.push(c.clone());
        }
        if let Some(d) = &self.dbpath {
            args.push("--dbpath".into());
            args.push(d.clone());
        }
        if let Some(r) = &self.root {
            args.push("--root".into());
            args.push(r.clone());
        }
        args
    }
}

pub struct PacmanSource<E: CmdExec> {
    exec: E,
    cfg: PacmanConfig,
    /// Cached repository list discovered on first call.
    databases: std::sync::OnceLock<Vec<String>>,
    /// Cached map of `virtual name → list of providing package names`,
    /// built once from sync DB metadata. `None` means we tried to build
    /// the index but the underlying tool wasn't available, so callers
    /// must fall back; an empty map means it was built and is empty.
    provides_index: std::sync::OnceLock<Option<BTreeMap<String, Vec<String>>>>,
}

impl<E: CmdExec> PacmanSource<E> {
    pub fn new(exec: E, cfg: PacmanConfig) -> Self {
        Self {
            exec,
            cfg,
            databases: std::sync::OnceLock::new(),
            provides_index: std::sync::OnceLock::new(),
        }
    }

    fn pacman(&self, args: &[&str]) -> Result<String> {
        let mut all = self.cfg.extra_args();
        for a in args {
            all.push((*a).to_string());
        }
        self.exec.run("pacman", &all)
    }

    fn pactree(&self, args: &[&str]) -> Result<String> {
        let mut all = Vec::new();
        if let Some(c) = &self.cfg.config {
            all.push("--config".into());
            all.push(c.clone());
        }
        if let Some(d) = &self.cfg.dbpath {
            all.push("--dbpath".into());
            all.push(d.clone());
        }
        if let Some(r) = &self.cfg.root {
            all.push("--root".into());
            all.push(r.clone());
        }
        for a in args {
            all.push((*a).to_string());
        }
        self.exec.run("pactree", &all)
    }

    /// Build (or return cached) `virtual_name → [providing pkg]` map by
    /// reading sync DB Provides fields.
    ///
    /// Strategy:
    /// 1. Try `expac -S -l <LIST_DELIM> '%n\t%S'` — emits one line per
    ///    sync package with name and a list of provides separated by
    ///    `LIST_DELIM` (we use ASCII unit separator `\x1f` so names with
    ///    `=` constraints stay intact). This is the canonical alpm-backed
    ///    enumeration. Without `-l`, expac would emit `%S` items joined
    ///    by the default two-space delimiter — fine for `Provides`
    ///    today but fragile, so we pin the delimiter explicitly.
    /// 2. If expac is unavailable, try parsing the output of
    ///    `pacman -Sl` + `pacman -Si` per repo. We avoid that fallback
    ///    here for cost reasons; callers handle a `None` index by
    ///    treating the lookup as inconclusive.
    fn build_provides_index(&self) -> Option<BTreeMap<String, Vec<String>>> {
        let mut expac_args: Vec<String> = Vec::new();
        if let Some(c) = &self.cfg.config {
            expac_args.push("--config".into());
            expac_args.push(c.clone());
        }
        // -S queries sync DBs; -l pins the list delimiter so each %S
        // entry is recoverable as a single token even if expac's
        // default ever changes. %n = name, %S = provides list.
        expac_args.push("-S".into());
        expac_args.push("-l".into());
        expac_args.push(EXPAC_LIST_DELIM.into());
        expac_args.push("%n\t%S".into());
        let out = match self.exec.run("expac", &expac_args) {
            Ok(s) => s,
            Err(_) => return None,
        };
        let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for line in out.lines() {
            let mut parts = line.splitn(2, '\t');
            let name = match parts.next() {
                Some(n) if !n.is_empty() => n,
                _ => continue,
            };
            let provides_field = parts.next().unwrap_or("");
            for tok in split_expac_list(provides_field) {
                let dep = Dep::parse(tok);
                if dep.name.is_empty() {
                    continue;
                }
                map.entry(dep.name).or_default().push(name.to_string());
            }
        }
        // Stable order, dedup.
        for v in map.values_mut() {
            v.sort();
            v.dedup();
        }
        Some(map)
    }

    fn provides_index(&self) -> Option<&BTreeMap<String, Vec<String>>> {
        self.provides_index
            .get_or_init(|| self.build_provides_index())
            .as_ref()
    }

    /// Pick the deterministic winner among multiple providers: prefer an
    /// installed provider, then fall back to alphabetical order.
    fn choose_provider(&self, candidates: &[String]) -> Option<String> {
        if candidates.is_empty() {
            return None;
        }
        let mut sorted = candidates.to_vec();
        sorted.sort();
        sorted.dedup();
        for c in &sorted {
            if self.is_installed(c).unwrap_or(false) {
                return Some(c.clone());
            }
        }
        sorted.into_iter().next()
    }
}

impl PacmanSource<SystemExec> {
    pub fn system(cfg: PacmanConfig) -> Self {
        Self::new(SystemExec, cfg)
    }
}

impl<E: CmdExec> MetadataSource for PacmanSource<E> {
    fn package(&self, name: &str) -> Result<Option<Package>> {
        // `pacman -Si` searches sync DBs only.
        let out = match self.pacman(&["-Si", name]) {
            Ok(s) => s,
            Err(DeploytixError::CommandFailed { stderr, .. })
                if stderr.contains("was not found") || stderr.contains("target not found") =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        Ok(Some(parse_pacman_si(&out)?))
    }

    fn provider_of(&self, virtual_name: &str) -> Result<Option<String>> {
        // 1. Direct sync-DB hit: a real package whose name matches wins.
        //    `pacman -Si` consults sync DB Name fields.
        if let Some(p) = self.package(virtual_name)? {
            return Ok(Some(p.name));
        }

        // 2. Consult actual Provides metadata via the cached index built
        //    from `expac -S '%n\t%S'`. This is the libalpm-backed path —
        //    `pacman -Ss` would only match name/description and miss
        //    soname-style virtual deps such as `sh` or `libfoo.so=1-64`.
        if let Some(index) = self.provides_index() {
            if let Some(candidates) = index.get(virtual_name) {
                if let Some(chosen) = self.choose_provider(candidates) {
                    return Ok(Some(chosen));
                }
            }
            // Index built and the virtual name is genuinely unknown.
            return Ok(None);
        }

        // 3. expac wasn't available — fall back to `pacman -Sii` parsing.
        //    `-Sii` prints each sync package's full record including the
        //    Provides field; we scan it for the virtual name. This is
        //    slower than the index but avoids the original `pacman -Ss`
        //    bug where matches came from package descriptions, not
        //    provides.
        match self.pacman(&["-Sii"]) {
            Ok(out) => Ok(scan_si_for_provider(
                &out,
                virtual_name,
                |pkg| self.is_installed(pkg).unwrap_or(false),
            )),
            Err(DeploytixError::CommandFailed { .. }) => Ok(None),
            Err(_) => Ok(None),
        }
    }

    fn required_by(&self, name: &str) -> Result<Vec<String>> {
        // `pactree -s -r -u <name>`: -s consults the sync DB (so we
        // catch reverse deps for packages not installed locally), -r
        // walks reverse, -u dedupes. Without `-s`, pactree(8) reads
        // only the local package DB and silently misses repository
        // reverse deps for not-yet-installed packages.
        let out = match self.pactree(&["-s", "-r", "-u", name]) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(parse_pactree_unique(&out, name))
    }

    fn optional_for(&self, name: &str) -> Result<Vec<String>> {
        // pactree has no first-class reverse-optdep mode, but `expac`
        // does. The challenge is that `%O` is a list field whose entries
        // are `pkgname[: free-form description with spaces]`, joined by
        // expac's list delimiter (default two spaces). Splitting on
        // arbitrary whitespace would shred a description like
        // `git: for AUR helpers` into four fragments, so:
        //   - we pin the list delimiter to ASCII unit separator (\x1f)
        //     via `-l`, so each optdepend entry survives as one token;
        //   - we use a TAB between %n and %O so the package name is
        //     trivially separable even if the optdep list is empty.
        // Threads the same --config override pacman uses.
        let mut expac_args: Vec<String> = Vec::new();
        if let Some(c) = &self.cfg.config {
            expac_args.push("--config".into());
            expac_args.push(c.clone());
        }
        expac_args.push("-S".into());
        expac_args.push("-l".into());
        expac_args.push(EXPAC_LIST_DELIM.into());
        expac_args.push("%n\t%O".into());
        let out = match self.exec.run("expac", &expac_args) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        let mut hits = Vec::new();
        for line in out.lines() {
            let mut parts = line.splitn(2, '\t');
            let pkg = match parts.next() {
                Some(p) if !p.is_empty() => p,
                _ => continue,
            };
            let optdeps_field = parts.next().unwrap_or("");
            for tok in split_expac_list(optdeps_field) {
                let dep = Dep::parse(tok);
                // Match against the dep name (with any version
                // constraint stripped by Dep::parse), NOT against
                // description fragments.
                if dep.name == name {
                    hits.push(pkg.to_string());
                    break;
                }
            }
        }
        hits.sort();
        hits.dedup();
        Ok(hits)
    }

    fn is_installed(&self, name: &str) -> Result<bool> {
        match self.exec.run("pacman", &["-Qq".to_string(), name.to_string()]) {
            Ok(_) => Ok(true),
            Err(DeploytixError::CommandFailed { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn databases(&self) -> Vec<String> {
        self.databases
            .get_or_init(|| {
                // `pacman-conf --repo-list` is the canonical way.
                match self.exec.run(
                    "pacman-conf",
                    &self
                        .cfg
                        .config
                        .as_ref()
                        .map(|c| vec!["--config".to_string(), c.clone(), "--repo-list".to_string()])
                        .unwrap_or_else(|| vec!["--repo-list".to_string()]),
                ) {
                    Ok(s) => s
                        .lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect(),
                    Err(_) => Vec::new(),
                }
            })
            .clone()
    }

    fn install_plan(&self, targets: &[&str], clean_root: bool) -> Result<InstallPlan> {
        let mut args: Vec<String> = self.cfg.extra_args();
        args.push("-S".into());
        args.push("--print".into());
        args.push("--print-format".into());
        args.push("%r/%n %v".into());
        // `pacman -S --print` is a planning-only invocation, but pacman
        // can still prompt for things like package-group selection
        // (`pacman -S gnome` asks which members of the group to keep)
        // and for any conflict it wants the user to confirm before
        // it folds into the plan. Because we run pacman through
        // `Command::output` with captured stdio, those prompts are
        // invisible and would block the resolver indefinitely. Always
        // pass `--noconfirm` (regardless of `clean_root`) so the
        // preflight is guaranteed non-interactive — pacman picks the
        // default answer to every prompt. `--noprogressbar` keeps the
        // captured stdout terse for parsing.
        args.push("--noconfirm".into());
        args.push("--noprogressbar".into());
        for t in targets {
            args.push((*t).to_string());
        }
        let out = match self.exec.run("pacman", &args) {
            Ok(s) => s,
            Err(e) => return Err(e),
        };
        let mut plan = InstallPlan {
            targets: targets.iter().map(|s| s.to_string()).collect(),
            clean_root,
            ..Default::default()
        };
        for line in out.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some((repo_name, version)) = line.split_once(' ') {
                if let Some((repo, name)) = repo_name.split_once('/') {
                    plan.to_install.push(PlannedPackage {
                        repo: repo.to_string(),
                        name: name.to_string(),
                        version: version.to_string(),
                    });
                }
            }
        }
        Ok(plan)
    }
}

/// Parse a `pacman -Si <pkg>` output blob into a [`Package`]. Public for
/// the unit tests; in normal use this is called via `package()`.
pub fn parse_pacman_si(text: &str) -> Result<Package> {
    // We collect both a folded single-line view (for whitespace-separated
    // fields like Depends On) and a list of trimmed lines (for fields
    // whose entries are one-per-line, like Optional Deps).
    let mut folded: BTreeMap<String, String> = BTreeMap::new();
    let mut lines_per_field: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current_key: Option<String> = None;

    for raw in text.lines() {
        if raw.is_empty() {
            current_key = None;
            continue;
        }
        if raw.starts_with(' ') || raw.starts_with('\t') {
            if let Some(k) = &current_key {
                let entry = folded.entry(k.clone()).or_default();
                entry.push(' ');
                entry.push_str(raw.trim());
                lines_per_field
                    .entry(k.clone())
                    .or_default()
                    .push(raw.trim().to_string());
            }
            continue;
        }
        if let Some((key, value)) = raw.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            current_key = Some(key.clone());
            folded.insert(key.clone(), value.clone());
            lines_per_field.entry(key).or_default().push(value);
        }
    }

    let name = folded
        .get("Name")
        .cloned()
        .ok_or_else(|| DeploytixError::ConfigError("pacman -Si: missing Name".into()))?;
    let version = folded.get("Version").cloned().unwrap_or_default();
    let repo = folded.get("Repository").cloned().unwrap_or_default();

    let multi = |k: &str| -> Vec<String> {
        folded
            .get(k)
            .map(|v| {
                v.split_whitespace()
                    .filter(|t| *t != "None")
                    .map(|t| t.to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    let multi_dep = |k: &str| -> Vec<Dep> { multi(k).into_iter().map(|t| Dep::parse(&t)).collect() };

    // For optdepends, prefer line-per-entry. If pacman emitted the field
    // on a single line (e.g. with two-space separators), fall back to
    // splitting on "  ".
    let optdepends: Vec<Dep> = match lines_per_field.get("Optional Deps") {
        Some(lines) if lines.len() > 1 => lines
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && *l != "None")
            .map(Dep::parse)
            .collect(),
        _ => {
            let raw = folded.get("Optional Deps").cloned().unwrap_or_default();
            if raw.is_empty() || raw == "None" {
                Vec::new()
            } else {
                raw.split("  ")
                    .map(|t| t.trim())
                    .filter(|t| !t.is_empty())
                    .map(Dep::parse)
                    .collect()
            }
        }
    };

    Ok(Package {
        name,
        version,
        repo,
        arch: folded.get("Architecture").cloned().unwrap_or_default(),
        description: folded.get("Description").cloned().unwrap_or_default(),
        url: folded.get("URL").cloned().unwrap_or_default(),
        licenses: multi("Licenses"),
        groups: multi("Groups"),
        depends: multi_dep("Depends On"),
        makedepends: multi_dep("Build Depends"),
        checkdepends: multi_dep("Check Depends"),
        optdepends,
        provides: multi_dep("Provides"),
        conflicts: multi_dep("Conflicts With"),
        replaces: multi_dep("Replaces"),
        required_by: multi("Required By"),
        optional_for: multi("Optional For"),
    })
}

/// Scan `pacman -Sii` output for any package whose `Provides` field
/// lists `virtual_name`. Returns the deterministically-chosen winner
/// (installed package preferred, then alphabetical).
fn scan_si_for_provider<F>(text: &str, virtual_name: &str, is_installed: F) -> Option<String>
where
    F: Fn(&str) -> bool,
{
    let mut current_name: Option<String> = None;
    let mut current_provides: String = String::new();
    let mut hits: BTreeSet<String> = BTreeSet::new();

    let flush = |name: &Option<String>, provides: &str, hits: &mut BTreeSet<String>| {
        if let Some(n) = name {
            for tok in provides.split_whitespace() {
                if tok.is_empty() {
                    continue;
                }
                let dep = Dep::parse(tok);
                if dep.name == virtual_name {
                    hits.insert(n.clone());
                    break;
                }
            }
        }
    };

    let mut current_key: Option<String> = None;
    for raw in text.lines() {
        if raw.is_empty() {
            // Record boundary.
            flush(&current_name, &current_provides, &mut hits);
            current_name = None;
            current_provides.clear();
            current_key = None;
            continue;
        }
        if raw.starts_with(' ') || raw.starts_with('\t') {
            if current_key.as_deref() == Some("Provides") {
                current_provides.push(' ');
                current_provides.push_str(raw.trim());
            }
            continue;
        }
        if let Some((key, value)) = raw.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            current_key = Some(key.clone());
            match key.as_str() {
                "Name" => current_name = Some(value),
                "Provides" => current_provides = value,
                _ => {}
            }
        }
    }
    flush(&current_name, &current_provides, &mut hits);

    if hits.is_empty() {
        return None;
    }
    let mut sorted: Vec<String> = hits.into_iter().collect();
    sorted.sort();
    for c in &sorted {
        if is_installed(c) {
            return Some(c.clone());
        }
    }
    sorted.into_iter().next()
}

fn parse_pactree_unique(text: &str, target: &str) -> Vec<String> {
    // pactree(8) prefixes children with ASCII tree art (`|-`, `\` -`,
    // `-`) and, when stdout is a TTY, with the Unicode box-drawing
    // glyphs `├`, `─`, `│`, `└`. Strip both forms before extracting
    // the package name on each line.
    let strip = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ' ' | '\t' | '|' | '`' | '-' | '├' | '─' | '│' | '└' | '┬' | '┐' | '┘'
            )
    };
    let mut out: Vec<String> = text
        .lines()
        .map(|l| l.trim_start_matches(strip).trim().to_string())
        .filter(|l| !l.is_empty() && l != target)
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Mock executor that returns canned stdout per (program, joined-args).
    /// Uses `Mutex` rather than `RefCell` because [`CmdExec`] requires
    /// `Sync` (the production source is shared across threads).
    struct CannedExec {
        responses: Mutex<HashMap<String, std::result::Result<String, DeploytixError>>>,
    }

    impl CmdExec for CannedExec {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            let key = format!("{} {}", program, args.join(" "));
            let mut map = self.responses.lock().unwrap();
            let key_prefix_match = map
                .keys()
                .find(|k| key.starts_with(k.as_str()))
                .cloned();
            let lookup = key_prefix_match.or_else(|| {
                if map.contains_key(&key) {
                    Some(key.clone())
                } else {
                    None
                }
            });
            if let Some(k) = lookup {
                map.remove(&k)
                    .unwrap_or_else(|| Err(DeploytixError::CommandNotFound(program.into())))
            } else {
                Err(DeploytixError::CommandNotFound(program.into()))
            }
        }
    }

    fn canned(pairs: &[(&str, std::result::Result<&str, DeploytixError>)]) -> CannedExec {
        let mut map = HashMap::new();
        for (k, v) in pairs {
            let v: std::result::Result<String, DeploytixError> = match v {
                Ok(s) => Ok((*s).to_string()),
                Err(_e) => Err(DeploytixError::CommandFailed {
                    command: k.to_string(),
                    stderr: "error: target not found".into(),
                }),
            };
            map.insert(k.to_string(), v);
        }
        CannedExec {
            responses: Mutex::new(map),
        }
    }

    #[test]
    fn parses_pacman_si_blob() {
        let blob = "\
Repository      : extra
Name            : foo
Version         : 1.2.3-4
Description     : a test package
Architecture    : x86_64
URL             : https://example.com
Licenses        : GPL3
Groups          : None
Provides        : foo-impl=1.2
Depends On      : glibc>=2.39  bar
Optional Deps   : git: needed for git remotes  python: optional scripting
Conflicts With  : oldfoo
Replaces        : ancientfoo
Required By     : alpha beta
Optional For    : gamma
";
        let pkg = parse_pacman_si(blob).unwrap();
        assert_eq!(pkg.name, "foo");
        assert_eq!(pkg.version, "1.2.3-4");
        assert_eq!(pkg.repo, "extra");
        assert_eq!(pkg.depends.len(), 2);
        assert_eq!(pkg.depends[0].name, "glibc");
        assert_eq!(pkg.depends[0].constraint.as_deref(), Some(">=2.39"));
        assert!(pkg
            .optdepends
            .iter()
            .any(|d| d.name == "git" && d.description.is_some()));
        assert_eq!(pkg.required_by, vec!["alpha", "beta"]);
        assert_eq!(pkg.provides[0].name, "foo-impl");
    }

    #[test]
    fn install_plan_parses_print_format() {
        // Prefix match — the canned key just needs to be a prefix of
        // the assembled command line. Targets come last.
        let exec = canned(&[(
            "pacman -S --print --print-format %r/%n %v --noconfirm --noprogressbar foo",
            Ok("system/glibc 2.39-1\nworld/foo 1.2.3-4\n"),
        )]);
        let src = PacmanSource::new(exec, PacmanConfig::default());
        let plan = src.install_plan(&["foo"], false).unwrap();
        assert_eq!(plan.to_install.len(), 2);
        assert_eq!(plan.to_install[1].repo, "world");
        assert_eq!(plan.to_install[1].name, "foo");
        assert_eq!(plan.to_install[1].version, "1.2.3-4");
    }

    #[test]
    fn install_plan_threads_config_overrides() {
        // The canned key includes the `--config` prefix, proving the
        // config was passed through. Order matches PacmanConfig::extra_args:
        // config, dbpath, root.
        let exec = canned(&[(
            "pacman --config /tmp/p.conf --dbpath /mnt/var/lib/pacman --root /mnt -S --print",
            Ok("custom/foo 0.1-1\n"),
        )]);
        let src = PacmanSource::new(
            exec,
            PacmanConfig {
                config: Some("/tmp/p.conf".into()),
                root: Some("/mnt".into()),
                dbpath: Some("/mnt/var/lib/pacman".into()),
            },
        );
        let plan = src.install_plan(&["foo"], true).unwrap();
        assert_eq!(plan.to_install.len(), 1);
        assert_eq!(plan.to_install[0].repo, "custom");
        assert!(plan.clean_root);
    }

    /// Bug fix: `install_plan` is the path the chroot preflight uses
    /// with `clean_root=false`. Even there, pacman can prompt
    /// (group-target selection like `pacman -S gnome`, conflict
    /// confirmations) and our captured-stdio executor would hang on
    /// the prompt. The argv MUST always carry `--noconfirm` so the
    /// resolver is non-interactive regardless of `clean_root`.
    #[test]
    fn install_plan_always_noninteractive_even_when_clean_root_false() {
        let exec = RecordingExec::new(vec![(
            "pacman -S --print",
            Ok("extra/gnome-shell 46-1\n"),
        )]);
        let src = PacmanSource::new(exec, PacmanConfig::default());
        // Group target — the kind of input pacman would prompt about.
        let _plan = src.install_plan(&["gnome"], false).unwrap();
        let calls = src.exec.calls();
        let pacman_call = calls
            .iter()
            .find(|c| c.starts_with("pacman -S --print"))
            .expect("pacman -S --print not invoked");
        assert!(
            pacman_call.contains("--noconfirm"),
            "install_plan must always pass --noconfirm to avoid prompts; got: {}",
            pacman_call
        );
    }

    /// Sanity: `--noconfirm` is also present when clean_root=true. The
    /// previous code path only added it for clean_root; this test
    /// pins the new contract that it is unconditional.
    #[test]
    fn install_plan_passes_noconfirm_when_clean_root_true() {
        let exec = RecordingExec::new(vec![(
            "pacman -S --print",
            Ok("system/glibc 2.39-1\n"),
        )]);
        let src = PacmanSource::new(exec, PacmanConfig::default());
        let _plan = src.install_plan(&["glibc"], true).unwrap();
        let calls = src.exec.calls();
        assert!(calls.iter().any(|c| c.contains("--noconfirm")));
    }

    #[test]
    fn parse_pactree_strips_indent() {
        let blob = "target\n├─dep1\n│ └─dep2\n└─dep3\n";
        let names = parse_pactree_unique(blob, "target");
        assert!(names.contains(&"dep1".to_string()));
        assert!(names.contains(&"dep2".to_string()));
        assert!(names.contains(&"dep3".to_string()));
        assert!(!names.contains(&"target".to_string()));
    }

    /// A `CmdExec` that records every call and can answer multiple
    /// queries from a fixed table without consuming entries — required
    /// when a single provider lookup makes several pacman/expac calls.
    /// `Mutex` (not `RefCell`) because [`CmdExec`] is `Send + Sync`.
    struct RecordingExec {
        responses: Vec<(String, std::result::Result<String, DeploytixError>)>,
        calls: Mutex<Vec<String>>,
    }

    impl RecordingExec {
        fn new(pairs: Vec<(&str, std::result::Result<&str, DeploytixError>)>) -> Self {
            let responses = pairs
                .into_iter()
                .map(|(k, v)| {
                    let val: std::result::Result<String, DeploytixError> = match v {
                        Ok(s) => Ok(s.to_string()),
                        Err(_) => Err(DeploytixError::CommandFailed {
                            command: k.to_string(),
                            stderr: "error: target not found".into(),
                        }),
                    };
                    (k.to_string(), val)
                })
                .collect();
            Self {
                responses,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CmdExec for RecordingExec {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            let key = format!("{} {}", program, args.join(" "));
            self.calls.lock().unwrap().push(key.clone());
            // Prefer exact match, then longest prefix match — keeps
            // disambiguation deterministic when one stored key is a
            // prefix of another (e.g. "pacman -Si" vs "pacman -Si sh").
            let mut best: Option<&(String, std::result::Result<String, DeploytixError>)> = None;
            for entry in &self.responses {
                if entry.0 == key {
                    best = Some(entry);
                    break;
                }
                if key.starts_with(entry.0.as_str())
                    && best
                        .as_ref()
                        .map(|b| entry.0.len() > b.0.len())
                        .unwrap_or(true)
                {
                    best = Some(entry);
                }
            }
            if let Some((_, val)) = best {
                return match val {
                    Ok(s) => Ok(s.clone()),
                    Err(DeploytixError::CommandFailed { command, stderr }) => {
                        Err(DeploytixError::CommandFailed {
                            command: command.clone(),
                            stderr: stderr.clone(),
                        })
                    }
                    Err(_) => Err(DeploytixError::CommandNotFound(program.into())),
                };
            }
            Err(DeploytixError::CommandNotFound(program.into()))
        }
    }

    /// Bug fix #1 (provider_of): virtual deps such as `sh` must resolve
    /// to packages whose `Provides` field lists the virtual name, not
    /// to packages whose name or description happens to match a regex.
    #[test]
    fn provider_of_resolves_via_expac_provides_index() {
        // `pacman -Si sh` fails (sh isn't a real package). expac then
        // reports that bash provides sh. The lookup MUST succeed
        // without falling back to the pacman -Ss name/description
        // search.
        let exec = RecordingExec::new(vec![
            (
                "pacman -Si sh",
                Err(DeploytixError::CommandFailed {
                    command: "pacman -Si sh".into(),
                    stderr: "error: package 'sh' was not found".into(),
                }),
            ),
            (
                "expac -S -l \x1f %n\t%S",
                Ok("bash\tsh=5.2\x1fbash=5.2\nzsh\tzsh=5.9\nglibc\t\n"),
            ),
            // is_installed checks (called by choose_provider).
            (
                "pacman -Qq bash",
                Err(DeploytixError::CommandFailed {
                    command: "pacman -Qq bash".into(),
                    stderr: "error: package 'bash' was not found".into(),
                }),
            ),
        ]);
        let src = PacmanSource::new(exec, PacmanConfig::default());
        let chosen = src.provider_of("sh").unwrap();
        assert_eq!(chosen.as_deref(), Some("bash"));

        // Crucially: we must NOT have invoked `pacman -Ss '^sh$'` —
        // that's the pacman name/description search that misses true
        // virtual provides.
        let calls = src.exec.calls();
        assert!(
            !calls.iter().any(|c| c.contains("-Ss")),
            "provider_of fell back to pacman -Ss name/description search; calls: {:?}",
            calls
        );
    }

    /// Bug fix #1 (provider_of): when multiple packages provide the
    /// same virtual name, prefer an installed one so the choice is
    /// deterministic and matches what pacman itself would do.
    #[test]
    fn provider_of_prefers_installed_provider() {
        let exec = RecordingExec::new(vec![
            (
                "pacman -Si sh",
                Err(DeploytixError::CommandFailed {
                    command: "pacman -Si sh".into(),
                    stderr: "error: package 'sh' was not found".into(),
                }),
            ),
            ("expac -S -l \x1f %n\t%S", Ok("bash\tsh\ndash\tsh\n")),
            // dash is installed; bash is not. choose_provider must
            // pick dash even though bash sorts first alphabetically.
            (
                "pacman -Qq bash",
                Err(DeploytixError::CommandFailed {
                    command: "pacman -Qq bash".into(),
                    stderr: "error: package 'bash' was not found".into(),
                }),
            ),
            ("pacman -Qq dash", Ok("dash\n")),
        ]);
        let src = PacmanSource::new(exec, PacmanConfig::default());
        let chosen = src.provider_of("sh").unwrap();
        assert_eq!(chosen.as_deref(), Some("dash"));
    }

    /// Bug fix #1 (provider_of): the index built from `expac -S` must
    /// preserve version constraints in `Provides` entries — the dep
    /// parser strips the `=1.2` so the bare virtual name is what we
    /// key on.
    #[test]
    fn provider_of_strips_version_constraint_from_provides() {
        let exec = RecordingExec::new(vec![
            (
                "pacman -Si libcrypto.so",
                Err(DeploytixError::CommandFailed {
                    command: "pacman -Si libcrypto.so".into(),
                    stderr: "error: package 'libcrypto.so' was not found".into(),
                }),
            ),
            (
                "expac -S -l \x1f %n\t%S",
                Ok("openssl\tlibcrypto.so=3-64\x1flibssl.so=3-64\n"),
            ),
            ("pacman -Qq openssl", Ok("openssl\n")),
        ]);
        let src = PacmanSource::new(exec, PacmanConfig::default());
        let chosen = src.provider_of("libcrypto.so").unwrap();
        assert_eq!(chosen.as_deref(), Some("openssl"));
    }

    /// Bug fix #2 (required_by): pactree must be invoked with `-s`
    /// (sync DB) so that reverse-deps for repository packages that
    /// aren't currently installed are returned.
    #[test]
    fn required_by_passes_sync_flag_to_pactree() {
        let exec = RecordingExec::new(vec![(
            "pactree -s -r -u glibc",
            Ok("glibc\nbase\nfilesystem\n"),
        )]);
        let src = PacmanSource::new(exec, PacmanConfig::default());
        let parents = src.required_by("glibc").unwrap();
        assert!(parents.contains(&"base".to_string()));
        assert!(parents.contains(&"filesystem".to_string()));

        let calls = src.exec.calls();
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("pactree ") && c.contains(" -s ")),
            "required_by must invoke pactree with -s; got calls {:?}",
            calls
        );
        // And it must not use the local-DB form (no -s).
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("pactree -r ") || c.starts_with("pactree -r -u ")),
            "required_by used local-DB pactree form; calls {:?}",
            calls
        );
    }

    /// Bug fix #2 (required_by): pactree must thread the same
    /// --config/--dbpath/--root overrides as pacman, ahead of the
    /// `-s -r -u` flags, so chroot/clean-root targets stay consistent.
    #[test]
    fn required_by_threads_config_overrides_to_pactree() {
        let exec = RecordingExec::new(vec![(
            "pactree --config /tmp/p.conf --dbpath /mnt/var/lib/pacman --root /mnt -s -r -u glibc",
            Ok("glibc\nbase\n"),
        )]);
        let src = PacmanSource::new(
            exec,
            PacmanConfig {
                config: Some("/tmp/p.conf".into()),
                dbpath: Some("/mnt/var/lib/pacman".into()),
                root: Some("/mnt".into()),
            },
        );
        let parents = src.required_by("glibc").unwrap();
        assert!(parents.contains(&"base".to_string()));
        let calls = src.exec.calls();
        assert!(
            calls
                .iter()
                .any(|c| c.contains("--config /tmp/p.conf") && c.contains(" -s ")),
            "config overrides not threaded; calls {:?}",
            calls
        );
    }

    /// scan_si_for_provider: the `-Sii` parser must associate Provides
    /// fields with the right package, including the multi-line form
    /// where additional provides appear on indented continuation lines.
    #[test]
    fn scan_si_for_provider_handles_multiline_provides() {
        let blob = "\
Repository      : core
Name            : bash
Version         : 5.2
Provides        : sh=5.2
                  bash-rl=5.2
Depends On      : glibc

Repository      : extra
Name            : zsh
Version         : 5.9
Provides        : None
Depends On      : glibc
";
        let chosen = scan_si_for_provider(blob, "sh", |_| false);
        assert_eq!(chosen.as_deref(), Some("bash"));
        let chosen2 = scan_si_for_provider(blob, "bash-rl", |_| false);
        assert_eq!(chosen2.as_deref(), Some("bash"));
        // virtual that nobody provides
        assert!(scan_si_for_provider(blob, "ksh", |_| false).is_none());
    }

    /// Bug fix #3 (optional_for): each entry in `%O` is `name[: free
    /// description]`. Descriptions routinely contain spaces. Splitting on
    /// arbitrary whitespace shreds `git: for AUR helpers` into four
    /// fragments and `Dep::parse("for")` produces a bogus name `for`,
    /// so the literal description word would falsely match a target
    /// looking for any of `for`, `AUR`, `helpers`. Ensure we recover
    /// each optdep as a complete record using the explicit list
    /// delimiter.
    #[test]
    fn optional_for_parses_descriptions_with_spaces() {
        // Two packages list `aur-helper` as an optional dep with a
        // description that contains spaces. The expected match must
        // be against the dep NAME (`aur-helper`), not against
        // description fragments.
        let exec = RecordingExec::new(vec![(
            "expac -S -l \x1f %n\t%O",
            Ok("git\taur-helper: for AUR helpers\x1fpython: optional scripting\n\
pacman\taur-helper: for sync db operations\n\
unrelated\tfoo: bar baz\n"),
        )]);
        let src = PacmanSource::new(exec, PacmanConfig::default());
        let parents = src.optional_for("aur-helper").unwrap();
        assert_eq!(parents, vec!["git".to_string(), "pacman".to_string()]);
    }

    /// Bug fix #3 (optional_for): description-only words must not
    /// produce false positives. Looking for `helpers` or `for` (which
    /// appear inside the description text) must NOT match.
    #[test]
    fn optional_for_does_not_match_description_words() {
        let exec = RecordingExec::new(vec![(
            "expac -S -l \x1f %n\t%O",
            Ok("git\taur-helper: for AUR helpers\x1fpython: optional scripting\n"),
        )]);
        let src = PacmanSource::new(exec, PacmanConfig::default());

        for word in ["for", "AUR", "helpers", "optional", "scripting"] {
            let parents = src.optional_for(word).unwrap();
            assert!(
                parents.is_empty(),
                "description word `{}` falsely matched: {:?}",
                word,
                parents
            );
        }
    }

    /// Bug fix #3 (optional_for): if expac is an older version that
    /// ignores `-l` and emits `%O` with the documented default two-space
    /// list delimiter, we must still parse the records correctly. We
    /// detect the absence of unit-separator characters and fall back
    /// to splitting on `"  "`.
    #[test]
    fn optional_for_falls_back_to_two_space_delim_when_unit_sep_absent() {
        let exec = RecordingExec::new(vec![(
            "expac -S -l \x1f %n\t%O",
            // Two-space-separated entries — the historical expac
            // default for `%O`. Each entry has a description with
            // single spaces inside, which two-space splitting handles
            // but whitespace splitting would not.
            Ok("git\taur-helper: for AUR helpers  python: optional scripting\n"),
        )]);
        let src = PacmanSource::new(exec, PacmanConfig::default());
        let parents = src.optional_for("aur-helper").unwrap();
        assert_eq!(parents, vec!["git".to_string()]);
        let parents2 = src.optional_for("python").unwrap();
        assert_eq!(parents2, vec!["git".to_string()]);
    }

    /// Bug fix #3 (optional_for): the expac invocation must use a
    /// robust delimiter strategy. Assert the constructed command pins
    /// the list delimiter via `-l`, ahead of the format string.
    #[test]
    fn optional_for_invokes_expac_with_explicit_list_delimiter() {
        // Even with empty output, we should be able to inspect the
        // call shape.
        let exec = RecordingExec::new(vec![("expac -S -l \x1f %n\t%O", Ok(""))]);
        let src = PacmanSource::new(exec, PacmanConfig::default());
        let parents = src.optional_for("anything").unwrap();
        assert!(parents.is_empty());

        let calls = src.exec.calls();
        assert!(
            calls.iter().any(|c| c.contains(" -l \x1f ") && c.contains("%O")),
            "optional_for did not pin expac list delimiter; calls: {:?}",
            calls
        );
    }

    /// Bug fix #3 (optional_for): `--config` overrides used by pacman
    /// must also be threaded into the expac invocation, otherwise a
    /// chroot-style query reads from the wrong sync DB and silently
    /// returns wrong results.
    #[test]
    fn optional_for_threads_config_override() {
        let exec = RecordingExec::new(vec![(
            "expac --config /tmp/p.conf -S -l \x1f %n\t%O",
            Ok("foo\taur-helper: in chroot\n"),
        )]);
        let src = PacmanSource::new(
            exec,
            PacmanConfig {
                config: Some("/tmp/p.conf".into()),
                ..Default::default()
            },
        );
        let parents = src.optional_for("aur-helper").unwrap();
        assert_eq!(parents, vec!["foo".to_string()]);
    }

    /// `split_expac_list` round-trip: when expac emits with the unit
    /// separator we get clean records; when it falls back to the
    /// two-space default each record still survives intact even with
    /// spaces inside its description.
    #[test]
    fn split_expac_list_handles_both_delimiters() {
        // Unit-separator path.
        let v = split_expac_list("git: for AUR helpers\x1fpython: scripts");
        assert_eq!(v, vec!["git: for AUR helpers", "python: scripts"]);

        // Two-space fallback path.
        let v = split_expac_list("git: for AUR helpers  python: optional scripting");
        assert_eq!(
            v,
            vec!["git: for AUR helpers", "python: optional scripting"]
        );

        // Empty / None.
        assert!(split_expac_list("").is_empty());
        assert!(split_expac_list("\n").is_empty());

        // Single entry, no delimiters.
        let v = split_expac_list("git: for AUR helpers");
        assert_eq!(v, vec!["git: for AUR helpers"]);
    }

    /// `parse_pacman_si` already handles single-line `Optional Deps`
    /// using the documented two-space delimiter — make sure
    /// descriptions with internal spaces still survive end-to-end and
    /// the parsed dep has the correct name AND preserved description.
    #[test]
    fn pacman_si_optdeps_preserve_description_with_spaces() {
        let blob = "\
Repository      : extra
Name            : foo
Version         : 1.0-1
Optional Deps   : git: for AUR helpers  python: optional scripting feature
";
        let pkg = parse_pacman_si(blob).unwrap();
        assert_eq!(pkg.optdepends.len(), 2);
        assert_eq!(pkg.optdepends[0].name, "git");
        assert_eq!(
            pkg.optdepends[0].description.as_deref(),
            Some("for AUR helpers")
        );
        assert_eq!(pkg.optdepends[1].name, "python");
        assert_eq!(
            pkg.optdepends[1].description.as_deref(),
            Some("optional scripting feature")
        );
    }
}
