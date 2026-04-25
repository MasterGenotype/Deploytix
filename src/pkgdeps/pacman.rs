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
use std::collections::BTreeMap;
use std::process::Command;
use tracing::debug;

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
}

impl<E: CmdExec> PacmanSource<E> {
    pub fn new(exec: E, cfg: PacmanConfig) -> Self {
        Self {
            exec,
            cfg,
            databases: std::sync::OnceLock::new(),
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
        for a in args {
            all.push((*a).to_string());
        }
        self.exec.run("pactree", &all)
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
        // `pacman -Ssq <name>` returns names of packages whose name OR
        // provides matches; refine with -Si on each candidate. As a
        // simpler approach we use `pactree --provides` style: ask pacman
        // -Si <virtual>; if it succeeds the provider is whatever pacman
        // itself resolves.
        if let Some(p) = self.package(virtual_name)? {
            return Ok(Some(p.name));
        }
        // Fall back: scan sync repos with `pacman -Ss '^<name>$'`.
        match self.pacman(&["-Ss", &format!("^{}$", virtual_name)]) {
            Ok(out) => Ok(parse_pacman_ss_first(&out)),
            Err(DeploytixError::CommandFailed { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn required_by(&self, name: &str) -> Result<Vec<String>> {
        // `pactree -r <name>` lists reverse runtime deps. The first line
        // is the target itself; skip duplicates.
        let out = match self.pactree(&["-r", "-u", name]) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(parse_pactree_unique(&out, name))
    }

    fn optional_for(&self, name: &str) -> Result<Vec<String>> {
        // pactree has no first-class reverse-optdep mode, but `expac`
        // does: `expac -Q '%n %O' | grep <name>`. Try it; if expac is
        // missing, return empty rather than fail.
        let out = match self.exec.run(
            "expac",
            &[
                "-S".to_string(),
                "%n %O".to_string(),
            ],
        ) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        let mut hits = Vec::new();
        for line in out.lines() {
            let mut parts = line.splitn(2, ' ');
            let pkg = match parts.next() {
                Some(p) => p,
                None => continue,
            };
            let optdeps_field = parts.next().unwrap_or("");
            for tok in optdeps_field.split_whitespace() {
                let dep = Dep::parse(tok);
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
        if clean_root {
            // When planning for a chroot/clean root, skip running
            // hooks and treat the dbpath/root as authoritative — the
            // caller is expected to have set --root/--dbpath to the
            // chroot's pacman state.
            args.push("--noconfirm".into());
        }
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

fn parse_pacman_ss_first(text: &str) -> Option<String> {
    for line in text.lines() {
        // Format: `repo/name version ...`; subsequent description lines
        // start with whitespace.
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        if let Some((repo_name, _)) = line.split_once(' ') {
            if let Some((_, name)) = repo_name.split_once('/') {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn parse_pactree_unique(text: &str, target: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .lines()
        .map(|l| l.trim_start_matches([' ', '\t', '|', '`', '-']).trim().to_string())
        .filter(|l| !l.is_empty() && l != target)
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Mock executor that returns canned stdout per (program, joined-args).
    struct CannedExec {
        responses: RefCell<HashMap<String, std::result::Result<String, DeploytixError>>>,
    }

    impl CmdExec for CannedExec {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            let key = format!("{} {}", program, args.join(" "));
            let key_prefix_match = self
                .responses
                .borrow()
                .keys()
                .find(|k| key.starts_with(k.as_str()))
                .cloned();
            let lookup = key_prefix_match
                .or_else(|| {
                    if self.responses.borrow().contains_key(&key) {
                        Some(key.clone())
                    } else {
                        None
                    }
                });
            if let Some(k) = lookup {
                let mut map = self.responses.borrow_mut();
                let v = map
                    .remove(&k)
                    .unwrap_or_else(|| Err(DeploytixError::CommandNotFound(program.into())));
                v
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
            responses: RefCell::new(map),
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
        let exec = canned(&[(
            "pacman -S --print --print-format %r/%n %v foo",
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

    #[test]
    fn parse_pactree_strips_indent() {
        let blob = "target\n├─dep1\n│ └─dep2\n└─dep3\n";
        let names = parse_pactree_unique(blob, "target");
        assert!(names.contains(&"dep1".to_string()));
        assert!(names.contains(&"dep2".to_string()));
        assert!(names.contains(&"dep3".to_string()));
        assert!(!names.contains(&"target".to_string()));
    }
}
