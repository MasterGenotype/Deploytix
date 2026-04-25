//! Metadata source abstraction.
//!
//! Anything that can answer "what does package `X` depend on?" plugs in
//! here: the production [`super::pacman::PacmanSource`] shells out to
//! pacman/pactree, while [`MockSource`] is a deterministic in-memory
//! implementation used by tests and `--offline` mode.
//!
//! Keeping this as a trait — and dependency-injecting it into the
//! resolver — is what lets the test suite run inside sandboxes where
//! pacman is not available.

use super::model::{InstallPlan, Package};
use crate::utils::error::Result;
use std::collections::{BTreeMap, HashMap};

/// Read-only access to a pacman-style package universe.
pub trait MetadataSource {
    /// Look up a package by name. `None` means the package is not in any
    /// configured sync database. Implementations MUST NOT mutate system
    /// state (no `pacman -Sy`, no installs).
    fn package(&self, name: &str) -> Result<Option<Package>>;

    /// Resolve a virtual name (a `provides` entry) to a concrete package
    /// name. Returns `None` when nothing in the universe provides it.
    /// Implementations should be deterministic — typically by preferring
    /// an installed provider, then alphabetical order — so JSON output
    /// stays stable.
    fn provider_of(&self, virtual_name: &str) -> Result<Option<String>>;

    /// Reverse runtime deps: packages whose `depends` list mentions
    /// `name`. Equivalent to `pactree -r <name>` at depth 1.
    fn required_by(&self, name: &str) -> Result<Vec<String>>;

    /// Reverse optional deps: packages whose `optdepends` list mentions
    /// `name`.
    fn optional_for(&self, name: &str) -> Result<Vec<String>>;

    /// Whether `name` is currently installed on the host. Used so the
    /// resolver can omit already-installed packages from the install
    /// plan unless `clean_root` was requested.
    fn is_installed(&self, name: &str) -> Result<bool>;

    /// Names of the sync databases that were searched. Surfaced in JSON
    /// output for auditability.
    fn databases(&self) -> Vec<String>;

    /// Hint that the local sync DB looks stale (e.g., last refreshed
    /// more than N days ago, or unavailable). Returned warnings are
    /// merged into `DepClosure::warnings` / `InstallPlan::warnings`.
    fn staleness_warnings(&self) -> Vec<String> {
        Vec::new()
    }

    /// Ask the underlying tooling what `pacman -S --print` would do for
    /// the given targets. This is intentionally separate from the
    /// metadata accessors above because it consults pacman's
    /// transaction resolver, which knows about installed state,
    /// conflicts, and replacements.
    fn install_plan(&self, targets: &[&str], clean_root: bool) -> Result<InstallPlan>;
}

/// In-memory metadata source for tests and `--offline` mode.
///
/// Build with [`MockSource::builder`] or directly populate `packages`
/// and `installed`.
#[derive(Debug, Default, Clone)]
pub struct MockSource {
    packages: BTreeMap<String, Package>,
    installed: std::collections::BTreeSet<String>,
    databases: Vec<String>,
    /// Optional override for `provider_of` — useful when more than one
    /// package provides the same virtual name and the test wants to
    /// pin which one wins.
    provider_overrides: HashMap<String, String>,
}

impl MockSource {
    pub fn builder() -> MockSourceBuilder {
        MockSourceBuilder::default()
    }

    pub fn insert(&mut self, pkg: Package) {
        self.packages.insert(pkg.name.clone(), pkg);
    }

    pub fn mark_installed(&mut self, name: &str) {
        self.installed.insert(name.to_string());
    }

    pub fn set_databases(&mut self, dbs: Vec<String>) {
        self.databases = dbs;
    }

    pub fn set_provider(&mut self, virtual_name: &str, chosen: &str) {
        self.provider_overrides
            .insert(virtual_name.to_string(), chosen.to_string());
    }
}

#[derive(Default)]
pub struct MockSourceBuilder {
    inner: MockSource,
}

impl MockSourceBuilder {
    pub fn package(mut self, pkg: Package) -> Self {
        self.inner.insert(pkg);
        self
    }

    pub fn installed(mut self, name: &str) -> Self {
        self.inner.mark_installed(name);
        self
    }

    pub fn database<S: Into<String>>(mut self, db: S) -> Self {
        self.inner.databases.push(db.into());
        self
    }

    pub fn provider(mut self, virtual_name: &str, chosen: &str) -> Self {
        self.inner.set_provider(virtual_name, chosen);
        self
    }

    pub fn build(self) -> MockSource {
        self.inner
    }
}

impl MetadataSource for MockSource {
    fn package(&self, name: &str) -> Result<Option<Package>> {
        Ok(self.packages.get(name).cloned())
    }

    fn provider_of(&self, virtual_name: &str) -> Result<Option<String>> {
        if let Some(forced) = self.provider_overrides.get(virtual_name) {
            return Ok(Some(forced.clone()));
        }
        // Direct package match wins (e.g. `sh` → `bash` would normally
        // need an override, but `glibc` → `glibc` should resolve).
        if self.packages.contains_key(virtual_name) {
            return Ok(Some(virtual_name.to_string()));
        }
        // Otherwise scan provides; prefer installed, then alphabetical.
        let mut candidates: Vec<&Package> = self
            .packages
            .values()
            .filter(|p| p.provides.iter().any(|d| d.name == virtual_name))
            .collect();
        candidates.sort_by(|a, b| {
            let a_inst = self.installed.contains(&a.name);
            let b_inst = self.installed.contains(&b.name);
            b_inst.cmp(&a_inst).then_with(|| a.name.cmp(&b.name))
        });
        Ok(candidates.first().map(|p| p.name.clone()))
    }

    fn required_by(&self, name: &str) -> Result<Vec<String>> {
        let mut out: Vec<String> = self
            .packages
            .values()
            .filter(|p| p.depends.iter().any(|d| d.name == name))
            .map(|p| p.name.clone())
            .collect();
        out.sort();
        Ok(out)
    }

    fn optional_for(&self, name: &str) -> Result<Vec<String>> {
        let mut out: Vec<String> = self
            .packages
            .values()
            .filter(|p| p.optdepends.iter().any(|d| d.name == name))
            .map(|p| p.name.clone())
            .collect();
        out.sort();
        Ok(out)
    }

    fn is_installed(&self, name: &str) -> Result<bool> {
        Ok(self.installed.contains(name))
    }

    fn databases(&self) -> Vec<String> {
        self.databases.clone()
    }

    fn install_plan(&self, targets: &[&str], clean_root: bool) -> Result<InstallPlan> {
        // Synthesize a transaction plan from metadata: walk runtime
        // deps, skip installed packages unless clean_root is set,
        // collect conflicts/replacements as removals.
        use super::resolver;
        let opts = resolver::ResolveOpts {
            include_optional: false,
            include_make: false,
            include_check: false,
        };
        let closure = resolver::resolve_closure(self, targets, opts)?;

        let mut to_install = Vec::new();
        let mut to_remove = std::collections::BTreeSet::new();
        for pkg in &closure.nodes {
            if !clean_root && self.installed.contains(&pkg.name) {
                continue;
            }
            to_install.push(super::model::PlannedPackage {
                repo: pkg.repo.clone(),
                name: pkg.name.clone(),
                version: pkg.version.clone(),
            });
            for c in &pkg.conflicts {
                if self.installed.contains(&c.name) {
                    to_remove.insert(c.name.clone());
                }
            }
            for r in &pkg.replaces {
                if self.installed.contains(&r.name) {
                    to_remove.insert(r.name.clone());
                }
            }
        }

        Ok(InstallPlan {
            targets: targets.iter().map(|s| s.to_string()).collect(),
            to_install,
            to_remove: to_remove.into_iter().collect(),
            download_size: None,
            installed_size: None,
            warnings: closure.warnings,
            clean_root,
        })
    }
}
