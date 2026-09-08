//! AUR packages as a [`MetadataSource`], and the composite that joins them to
//! the repositories.
//!
//! # Why a composite rather than a replacement
//!
//! A real query spans both universes. `hhd-git` lives in the AUR, but nearly
//! everything it depends on (`python`, `python-evdev`) is a repo package. A
//! resolver pointed at only one of the two answers half the question.
//!
//! So [`CompositeSource`] asks the repositories first and the AUR only for what
//! they do not have. That ordering is not arbitrary:
//!
//! * it matches what a helper actually installs — a name in both places comes
//!   from the repo;
//! * it keeps the fast, local, always-available source on the hot path, so
//!   resolving an all-repo selection makes no network calls at all;
//! * it means an unreachable AUR degrades to today's behaviour rather than
//!   breaking repo resolution.
//!
//! One resolver, one closure algorithm, one graph renderer — the AUR becomes
//! another source rather than a parallel code path.
//!
//! # Caching
//!
//! [`AurSource`] memoises every lookup, negatives included. The resolver walks
//! a dependency graph and revisits names constantly; without this, a closure
//! over a handful of AUR packages would issue dozens of duplicate requests. A
//! negative is as worth caching as a hit: "the AUR does not have `glibc`" is
//! asked once per package that depends on it.
//!
//! The cache lives as long as the source. Callers build one per resolve, so it
//! never serves results across user actions and there is no staleness window to
//! reason about.

use crate::aur::rpc::{AurRpc, HttpGet, AUR_REPO};
use crate::pkgdeps::model::{InstallPlan, Package};
use crate::pkgdeps::source::MetadataSource;
use crate::utils::error::Result;
use std::collections::HashMap;
use std::sync::Mutex;

/// The AUR as a metadata source.
pub struct AurSource<H: HttpGet> {
    rpc: AurRpc<H>,
    /// `name -> Some(package)` or `name -> None` for a confirmed miss.
    cache: Mutex<HashMap<String, Option<Package>>>,
    /// Requests that failed, so the caller can explain a thin result instead of
    /// reporting the packages as nonexistent.
    errors: Mutex<Vec<String>>,
    /// How many requests actually left the machine. The cache is only worth
    /// having if this stays small, so it is measurable rather than assumed.
    requests: Mutex<usize>,
}

impl<H: HttpGet> AurSource<H> {
    pub fn new(http: H) -> Self {
        Self {
            rpc: AurRpc::new(http),
            cache: Mutex::new(HashMap::new()),
            errors: Mutex::new(Vec::new()),
            requests: Mutex::new(0),
        }
    }

    /// Requests issued to the AUR so far.
    pub fn rpc_calls(&self) -> usize {
        *self.requests.lock().unwrap()
    }

    fn count_request(&self) {
        *self.requests.lock().unwrap() += 1;
    }

    /// Warm the cache for several names in one request.
    ///
    /// The resolver asks for one name at a time, so without this a selection of
    /// five AUR packages costs five round trips before the walk even starts.
    /// Failures are recorded, not raised: a prefetch is an optimisation, and
    /// the per-name lookups behind it will surface any real problem.
    pub fn prefetch(&self, names: &[&str]) {
        let unknown: Vec<&str> = {
            let cache = self.cache.lock().unwrap();
            names
                .iter()
                .copied()
                .filter(|n| !cache.contains_key(*n))
                .collect()
        };
        if unknown.is_empty() {
            return;
        }
        self.count_request();
        match self.rpc.info(&unknown) {
            Ok(found) => {
                let mut cache = self.cache.lock().unwrap();
                for pkg in found {
                    cache.insert(pkg.name.clone(), Some(pkg));
                }
                // Anything asked for and not returned is a confirmed miss.
                for name in unknown {
                    cache.entry(name.to_string()).or_insert(None);
                }
            }
            Err(e) => self.errors.lock().unwrap().push(e.to_string()),
        }
    }

    /// Problems encountered while talking to the AUR.
    pub fn errors(&self) -> Vec<String> {
        self.errors.lock().unwrap().clone()
    }

    fn cached(&self, name: &str) -> Option<Option<Package>> {
        self.cache.lock().unwrap().get(name).cloned()
    }
}

impl<H: HttpGet> MetadataSource for AurSource<H> {
    fn package(&self, name: &str) -> Result<Option<Package>> {
        if let Some(hit) = self.cached(name) {
            return Ok(hit);
        }
        self.count_request();
        let found = match self.rpc.info(&[name]) {
            Ok(mut results) => results.pop(),
            Err(e) => {
                // Not fatal: an unreachable AUR must degrade to "no such
                // package here" so repo resolution still completes. The reason
                // is kept so the caller can say why the answer is thin.
                self.errors.lock().unwrap().push(e.to_string());
                None
            }
        };
        self.cache
            .lock()
            .unwrap()
            .insert(name.to_string(), found.clone());
        Ok(found)
    }

    fn provider_of(&self, virtual_name: &str) -> Result<Option<String>> {
        // A direct hit wins: a package named for the virtual it provides is the
        // provider, and it costs a cache lookup rather than a search.
        if let Ok(Some(p)) = self.package(virtual_name) {
            return Ok(Some(p.name));
        }
        self.count_request();
        match self.rpc.providers_of(virtual_name) {
            // Alphabetical, so the choice is reproducible across runs — the
            // determinism the trait asks for.
            Ok(mut providers) => {
                providers.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(providers.into_iter().next().map(|p| p.name))
            }
            Err(e) => {
                self.errors.lock().unwrap().push(e.to_string());
                Ok(None)
            }
        }
    }

    /// Not answerable from the AUR.
    ///
    /// Reverse dependencies mean "what, of everything, depends on this", and
    /// the RPC has no such index — answering it would mean downloading the
    /// whole package base. An empty list is honest: nothing *known to this
    /// source* depends on the name.
    fn required_by(&self, _name: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn optional_for(&self, _name: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// The AUR has no idea what is installed locally; pacman does. In a
    /// [`CompositeSource`] this question never reaches here.
    fn is_installed(&self, _name: &str) -> Result<bool> {
        Ok(false)
    }

    fn databases(&self) -> Vec<String> {
        vec![AUR_REPO.to_string()]
    }

    fn staleness_warnings(&self) -> Vec<String> {
        self.errors()
            .into_iter()
            .map(|e| format!("AUR metadata may be incomplete: {e}"))
            .collect()
    }

    /// pacman cannot plan a transaction for a package it has no database entry
    /// for, and an AUR install is a build rather than a transaction anyway.
    fn install_plan(&self, targets: &[&str], clean_root: bool) -> Result<InstallPlan> {
        Ok(InstallPlan {
            targets: targets.iter().map(|t| t.to_string()).collect(),
            clean_root,
            warnings: vec![
                "AUR packages are built, not installed from a repository, so pacman \
                 cannot pre-plan the transaction."
                    .to_string(),
            ],
            ..Default::default()
        })
    }
}

/// Repositories first, AUR for the remainder.
pub struct CompositeSource<P: MetadataSource, A: MetadataSource> {
    primary: P,
    fallback: A,
}

impl<P: MetadataSource, A: MetadataSource> CompositeSource<P, A> {
    pub fn new(primary: P, fallback: A) -> Self {
        Self { primary, fallback }
    }

    pub fn primary(&self) -> &P {
        &self.primary
    }

    pub fn fallback(&self) -> &A {
        &self.fallback
    }
}

impl<P: MetadataSource, A: MetadataSource> MetadataSource for CompositeSource<P, A> {
    fn package(&self, name: &str) -> Result<Option<Package>> {
        // A primary error is not a reason to skip the fallback: pacman failing
        // on one name should not make an AUR package unresolvable.
        if let Ok(Some(pkg)) = self.primary.package(name) {
            return Ok(Some(pkg));
        }
        self.fallback.package(name)
    }

    fn provider_of(&self, virtual_name: &str) -> Result<Option<String>> {
        if let Ok(Some(name)) = self.primary.provider_of(virtual_name) {
            return Ok(Some(name));
        }
        self.fallback.provider_of(virtual_name)
    }

    /// Reverse deps come from the primary only. The AUR cannot answer them, and
    /// merging in its empty list would just hide that.
    fn required_by(&self, name: &str) -> Result<Vec<String>> {
        self.primary.required_by(name)
    }

    fn optional_for(&self, name: &str) -> Result<Vec<String>> {
        self.primary.optional_for(name)
    }

    /// Always the primary: only the local pacman database knows what is
    /// installed, including packages that were originally built from the AUR.
    fn is_installed(&self, name: &str) -> Result<bool> {
        self.primary.is_installed(name)
    }

    fn databases(&self) -> Vec<String> {
        let mut dbs = self.primary.databases();
        dbs.extend(self.fallback.databases());
        dbs
    }

    fn staleness_warnings(&self) -> Vec<String> {
        let mut w = self.primary.staleness_warnings();
        w.extend(self.fallback.staleness_warnings());
        w
    }

    /// Planning is pacman's job, so it goes to the primary unchanged. Callers
    /// are expected to have filtered AUR targets out first — see
    /// `gui_update::preview`, which partitions by repo before planning.
    fn install_plan(&self, targets: &[&str], clean_root: bool) -> Result<InstallPlan> {
        self.primary.install_plan(targets, clean_root)
    }
}

/// A composite over the live pacman databases and the real AUR.
pub fn system_source() -> CompositeSource<
    crate::pkgdeps::pacman::PacmanSource<crate::pkgdeps::pacman::SystemExec>,
    AurSource<crate::aur::rpc::CurlGet>,
> {
    use crate::pkgdeps::pacman::{PacmanConfig, PacmanSource};
    CompositeSource::new(
        PacmanSource::system(PacmanConfig::default()),
        AurSource::new(crate::aur::rpc::CurlGet),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aur::rpc::HttpGet;
    use crate::pkgdeps::model::Dep;
    use crate::pkgdeps::resolver::{resolve_closure, ResolveOpts};
    use crate::pkgdeps::source::MockSource;
    use crate::utils::error::DeploytixError;
    use std::sync::Mutex as StdMutex;

    struct Canned {
        by_url: Vec<(String, String)>,
        calls: StdMutex<Vec<String>>,
    }

    impl Canned {
        fn new(pairs: &[(&str, &str)]) -> Self {
            Self {
                by_url: pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                calls: StdMutex::new(Vec::new()),
            }
        }
        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl HttpGet for Canned {
        fn get(&self, url: &str) -> Result<String> {
            self.calls.lock().unwrap().push(url.to_string());
            for (fragment, body) in &self.by_url {
                if url.contains(fragment.as_str()) {
                    return Ok(body.clone());
                }
            }
            Ok(r#"{"resultcount":0,"results":[],"type":"multiinfo"}"#.to_string())
        }
    }

    struct Offline;
    impl HttpGet for Offline {
        fn get(&self, _url: &str) -> Result<String> {
            Err(DeploytixError::CommandFailed {
                command: "curl".into(),
                stderr: "network unreachable".into(),
            })
        }
    }

    fn hhd_body() -> &'static str {
        r#"{"resultcount":1,"results":[{
            "Name":"hhd-git","Version":"3.1.3-1",
            "Depends":["python","python-evdev"]
        }],"type":"multiinfo"}"#
    }

    fn repo_source() -> MockSource {
        let mut s = MockSource::default();
        let mut python = Package::new("python", "3.12", "extra");
        python.depends = vec![Dep::unversioned("glibc")];
        s.insert(python);
        s.insert(Package::new("python-evdev", "1.7", "extra"));
        s.insert(Package::new("glibc", "2.39", "core"));
        s
    }

    #[test]
    fn an_aur_package_resolves_and_its_repo_dependencies_come_from_the_repo() {
        // The whole point of the composite: one closure spanning both.
        let composite = CompositeSource::new(
            repo_source(),
            AurSource::new(Canned::new(&[("arg[]=hhd-git", hhd_body())])),
        );
        let closure = resolve_closure(&composite, &["hhd-git"], ResolveOpts::default()).unwrap();

        let names: Vec<&str> = closure.nodes.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"hhd-git"), "{names:?}");
        assert!(names.contains(&"python"), "{names:?}");
        assert!(names.contains(&"python-evdev"), "{names:?}");
        // Transitive, through a repo package.
        assert!(names.contains(&"glibc"), "{names:?}");
        assert!(closure.unresolved.is_empty(), "{:?}", closure.unresolved);
    }

    #[test]
    fn resolved_packages_carry_the_universe_they_came_from() {
        let composite = CompositeSource::new(
            repo_source(),
            AurSource::new(Canned::new(&[("arg[]=hhd-git", hhd_body())])),
        );
        let closure = resolve_closure(&composite, &["hhd-git"], ResolveOpts::default()).unwrap();
        let hhd = closure.nodes.iter().find(|p| p.name == "hhd-git").unwrap();
        let python = closure.nodes.iter().find(|p| p.name == "python").unwrap();
        assert_eq!(
            hhd.repo, AUR_REPO,
            "must be identifiable as needing a build"
        );
        assert_ne!(python.repo, AUR_REPO);
    }

    #[test]
    fn the_repository_wins_when_a_name_exists_in_both() {
        // A helper would install the repo copy, so the preview must agree.
        let mut repo = MockSource::default();
        repo.insert(Package::new("shared", "1.0", "extra"));
        let aur = AurSource::new(Canned::new(&[(
            "arg[]=shared",
            r#"{"resultcount":1,"results":[{"Name":"shared","Version":"9.9"}],"type":"multiinfo"}"#,
        )]));
        let composite = CompositeSource::new(repo, aur);
        let pkg = composite.package("shared").unwrap().unwrap();
        assert_eq!(pkg.repo, "extra");
        assert_eq!(pkg.version, "1.0");
    }

    #[test]
    fn an_all_repo_selection_never_touches_the_network() {
        let canned = Canned::new(&[]);
        let composite = CompositeSource::new(repo_source(), AurSource::new(canned));
        resolve_closure(&composite, &["python"], ResolveOpts::default()).unwrap();
        assert_eq!(
            composite.fallback().rpc_calls(),
            0,
            "repo-only resolution must stay local"
        );
    }

    #[test]
    fn a_name_is_looked_up_once_however_often_it_is_asked_for() {
        let source = AurSource::new(Canned::new(&[("arg[]=hhd-git", hhd_body())]));
        for _ in 0..5 {
            source.package("hhd-git").unwrap();
        }
        assert_eq!(source.rpc_calls(), 1);
    }

    #[test]
    fn a_confirmed_miss_is_cached_too() {
        // Otherwise every package depending on glibc re-asks the AUR about it.
        let source = AurSource::new(Canned::new(&[]));
        for _ in 0..5 {
            assert!(source.package("glibc").unwrap().is_none());
        }
        assert_eq!(source.rpc_calls(), 1);
    }

    #[test]
    fn prefetch_collapses_many_lookups_into_one_request() {
        let source = AurSource::new(Canned::new(&[("arg[]=hhd-git", hhd_body())]));
        source.prefetch(&["hhd-git", "paru", "yay"]);
        assert_eq!(source.rpc_calls(), 1);
        // And the results are then served from cache.
        source.package("hhd-git").unwrap();
        source.package("paru").unwrap();
        assert_eq!(source.rpc_calls(), 1);
    }

    #[test]
    fn an_unreachable_aur_still_lets_repo_resolution_finish() {
        let composite = CompositeSource::new(repo_source(), AurSource::new(Offline));
        let closure = resolve_closure(&composite, &["python"], ResolveOpts::default()).unwrap();
        let names: Vec<&str> = closure.nodes.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"python"));
        assert!(names.contains(&"glibc"));
    }

    #[test]
    fn an_unreachable_aur_says_so_rather_than_reporting_no_such_package() {
        let source = AurSource::new(Offline);
        assert!(source.package("hhd-git").unwrap().is_none());
        let warnings = source.staleness_warnings();
        assert!(!warnings.is_empty(), "a network failure must be reported");
        assert!(warnings[0].contains("network unreachable"), "{warnings:?}");
    }

    #[test]
    fn installed_state_always_comes_from_the_local_database() {
        // Including for packages originally built from the AUR: only pacman
        // knows they are installed.
        let mut repo = MockSource::default();
        repo.insert(Package::new("hhd-git", "3.1.3", "extra"));
        repo.mark_installed("hhd-git");
        let composite = CompositeSource::new(repo, AurSource::new(Canned::new(&[])));
        assert!(composite.is_installed("hhd-git").unwrap());
    }

    #[test]
    fn databases_list_both_universes() {
        let composite = CompositeSource::new(repo_source(), AurSource::new(Canned::new(&[])));
        assert!(composite.databases().iter().any(|d| d == AUR_REPO));
    }
}
