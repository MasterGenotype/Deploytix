//! `deploytix deps …` subcommand handlers.
//!
//! Each handler accepts a [`MetadataSource`] so the CLI is agnostic to
//! whether resolution comes from real pacman or a fixture.

use super::graph::{to_dot, DotOpts};
use super::model::{DepClosure, InstallPlan};
use super::pacman::{PacmanConfig, PacmanSource};
use super::resolver::{self, ResolveOpts};
use super::source::{MetadataSource, MockSource};
use crate::utils::error::{DeploytixError, Result};
use serde::Serialize;
use std::path::Path;

/// Resolved CLI arguments after parsing — kept simple to allow this
/// module to be reused from a future GUI frontend.
#[derive(Debug, Clone, Default)]
pub struct DepsArgs {
    pub config: Option<String>,
    pub dbpath: Option<String>,
    pub root: Option<String>,
    pub include_optional: bool,
    pub include_make: bool,
    pub include_check: bool,
    pub json: bool,
    pub dot: bool,
    /// Path to an offline fixture (JSON list of `Package`) to use
    /// instead of pacman. Useful for CI.
    pub offline: Option<String>,
}

impl DepsArgs {
    pub fn pacman_config(&self) -> PacmanConfig {
        PacmanConfig {
            config: self.config.clone(),
            dbpath: self.dbpath.clone(),
            root: self.root.clone(),
        }
    }

    pub fn resolve_opts(&self) -> ResolveOpts {
        ResolveOpts {
            include_optional: self.include_optional,
            include_make: self.include_make,
            include_check: self.include_check,
        }
    }
}

/// Construct the metadata source the CLI should use given the args.
pub fn build_source(args: &DepsArgs) -> Result<Box<dyn MetadataSource>> {
    if let Some(path) = &args.offline {
        let mock = load_offline_fixture(Path::new(path))?;
        Ok(Box::new(mock))
    } else {
        Ok(Box::new(PacmanSource::system(args.pacman_config())))
    }
}

/// Read a JSON fixture from disk into a [`MockSource`]. Format:
/// `{ "packages": [Package, …], "installed": ["pkg", …], "databases": [...] }`.
pub fn load_offline_fixture(path: &Path) -> Result<MockSource> {
    let text = std::fs::read_to_string(path)?;
    #[derive(serde::Deserialize)]
    struct Fixture {
        #[serde(default)]
        packages: Vec<super::model::Package>,
        #[serde(default)]
        installed: Vec<String>,
        #[serde(default)]
        databases: Vec<String>,
        #[serde(default)]
        providers: Vec<super::model::ProviderChoice>,
    }
    let fixture: Fixture = serde_json::from_str(&text).map_err(|e| {
        DeploytixError::ConfigError(format!("invalid offline fixture {}: {}", path.display(), e))
    })?;

    // Build via the chainable builder so the JSON-fixture loader and any
    // future programmatic callers share one composition path.
    let mut builder = MockSource::builder();
    for p in fixture.packages {
        builder = builder.package(p);
    }
    for n in fixture.installed {
        builder = builder.installed(&n);
    }
    for db in fixture.databases {
        builder = builder.database(db);
    }
    for pc in fixture.providers {
        builder = builder.provider(&pc.virtual_name, &pc.chosen);
    }
    Ok(builder.build())
}

#[derive(Serialize)]
struct ResolveOutput<'a> {
    closure: &'a DepClosure,
}

pub fn cmd_resolve(source: &dyn MetadataSource, package: &str, args: &DepsArgs) -> Result<()> {
    let closure = resolver::resolve_closure(source, &[package], args.resolve_opts())?;
    if args.json {
        print_json(&ResolveOutput { closure: &closure })?;
    } else if args.dot {
        print!("{}", to_dot(&closure, DotOpts::default()));
    } else {
        for pkg in &closure.nodes {
            println!("{} {} ({})", pkg.name, pkg.version, pkg.repo);
        }
        if !closure.unresolved.is_empty() {
            eprintln!("\nunresolved: {}", closure.unresolved.join(", "));
        }
        emit_warnings(&closure.warnings);
    }
    Ok(())
}

pub fn cmd_tree(source: &dyn MetadataSource, package: &str, args: &DepsArgs) -> Result<()> {
    let closure = resolver::resolve_closure(source, &[package], args.resolve_opts())?;
    if args.json {
        print_json(&closure)?;
    } else if args.dot {
        print!("{}", to_dot(&closure, DotOpts::default()));
    } else {
        print_tree(&closure, package, 0, &mut std::collections::BTreeSet::new());
        emit_warnings(&closure.warnings);
    }
    Ok(())
}

fn print_tree(
    closure: &DepClosure,
    name: &str,
    depth: usize,
    seen: &mut std::collections::BTreeSet<String>,
) {
    let prefix = "  ".repeat(depth);
    println!("{}{}", prefix, name);
    if !seen.insert(name.to_string()) {
        return;
    }
    for edge in closure.edges.iter().filter(|e| e.from == name) {
        print_tree(closure, &edge.to, depth + 1, seen);
    }
}

pub fn cmd_reverse(source: &dyn MetadataSource, package: &str, args: &DepsArgs) -> Result<()> {
    let closure = resolver::resolve_reverse(source, package, true, args.include_optional)?;
    if args.json {
        print_json(&closure)?;
    } else if args.dot {
        print!("{}", to_dot(&closure, DotOpts::default()));
    } else {
        for pkg in &closure.nodes {
            if pkg.name == package {
                continue;
            }
            println!("{}", pkg.name);
        }
        emit_warnings(&closure.warnings);
    }
    Ok(())
}

pub fn cmd_graph(
    source: &dyn MetadataSource,
    package: &str,
    output: Option<&str>,
    args: &DepsArgs,
) -> Result<()> {
    let closure = resolver::resolve_closure(source, &[package], args.resolve_opts())?;
    let dot = to_dot(&closure, DotOpts::default());
    match output {
        Some(path) => {
            std::fs::write(path, dot)?;
            println!("✓ Wrote graph to {}", path);
        }
        None => print!("{}", dot),
    }
    emit_warnings(&closure.warnings);
    Ok(())
}

pub fn cmd_plan_install(
    source: &dyn MetadataSource,
    package: &str,
    clean_root: bool,
    args: &DepsArgs,
) -> Result<()> {
    let plan = source.install_plan(&[package], clean_root)?;
    if args.json {
        print_json(&plan)?;
    } else {
        print_plan_human(&plan);
    }
    Ok(())
}

fn print_plan_human(plan: &InstallPlan) {
    println!(
        "Targets: {}  ({}clean root)",
        plan.targets.join(" "),
        if plan.clean_root { "" } else { "non-" }
    );
    println!("Would install ({}):", plan.to_install.len());
    for p in &plan.to_install {
        println!(
            "  {}/{} {}",
            if p.repo.is_empty() { "?" } else { &p.repo },
            p.name,
            p.version
        );
    }
    if !plan.to_remove.is_empty() {
        println!("Would remove ({}):", plan.to_remove.len());
        for p in &plan.to_remove {
            println!("  {}", p);
        }
    }
    emit_warnings(&plan.warnings);
}

pub fn cmd_metadata(source: &dyn MetadataSource, package: &str, args: &DepsArgs) -> Result<()> {
    let pkg = source.package(package)?.ok_or_else(|| {
        DeploytixError::ConfigError(format!(
            "package '{}' not found in any sync database",
            package
        ))
    })?;
    if args.json {
        print_json(&pkg)?;
    } else {
        println!("Name        : {}", pkg.name);
        println!("Version     : {}", pkg.version);
        println!("Repository  : {}", pkg.repo);
        println!("Description : {}", pkg.description);
        println!("URL         : {}", pkg.url);
        println!("Licenses    : {}", pkg.licenses.join(" "));
        println!(
            "Depends     : {}",
            pkg.depends
                .iter()
                .map(|d| d.to_token())
                .collect::<Vec<_>>()
                .join(" ")
        );
        println!(
            "MakeDepends : {}",
            pkg.makedepends
                .iter()
                .map(|d| d.to_token())
                .collect::<Vec<_>>()
                .join(" ")
        );
        println!(
            "CheckDepends: {}",
            pkg.checkdepends
                .iter()
                .map(|d| d.to_token())
                .collect::<Vec<_>>()
                .join(" ")
        );
        println!("Optional Deps:");
        for d in &pkg.optdepends {
            println!("  {}", d.to_token());
        }
        println!(
            "Provides    : {}",
            pkg.provides
                .iter()
                .map(|d| d.to_token())
                .collect::<Vec<_>>()
                .join(" ")
        );
        println!(
            "Conflicts   : {}",
            pkg.conflicts
                .iter()
                .map(|d| d.to_token())
                .collect::<Vec<_>>()
                .join(" ")
        );
        println!(
            "Replaces    : {}",
            pkg.replaces
                .iter()
                .map(|d| d.to_token())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    Ok(())
}

#[derive(Serialize)]
struct CompareOutput<'a> {
    a: &'a super::model::Package,
    b: &'a super::model::Package,
    differences: Vec<String>,
}

pub fn cmd_compare(
    source: &dyn MetadataSource,
    a: &str,
    b: &str,
    args: &DepsArgs,
) -> Result<()> {
    let pa = source.package(a)?.ok_or_else(|| {
        DeploytixError::ConfigError(format!("package '{}' not found", a))
    })?;
    let pb = source.package(b)?.ok_or_else(|| {
        DeploytixError::ConfigError(format!("package '{}' not found", b))
    })?;
    let differences = resolver::diff_packages(&pa, &pb);
    if args.json {
        print_json(&CompareOutput {
            a: &pa,
            b: &pb,
            differences,
        })?;
    } else if differences.is_empty() {
        println!("{} and {} have identical metadata", a, b);
    } else {
        for line in differences {
            println!("{}", line);
        }
    }
    Ok(())
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value).map_err(|e| {
        DeploytixError::ConfigError(format!("json serialization failed: {}", e))
    })?;
    println!("{}", text);
    Ok(())
}

fn emit_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("warning: {}", w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkgdeps::model::{Dep, Package};

    fn fixture_source() -> MockSource {
        let mut s = MockSource::default();
        let mut foo = Package::new("foo", "1.0", "world");
        foo.depends = vec![Dep::parse("bar>=1.0"), Dep::parse("sh")];
        foo.optdepends = vec![Dep::parse("git: optional plugin")];
        s.insert(foo);

        let bar = Package::new("bar", "1.2", "world");
        s.insert(bar);

        let mut bash = Package::new("bash", "5.2", "system");
        bash.provides = vec![Dep::unversioned("sh")];
        s.insert(bash);

        s.insert(Package::new("git", "2.45", "extra"));
        s.set_databases(vec!["world".into(), "system".into(), "extra".into()]);
        s
    }

    #[test]
    fn json_output_is_stable() {
        let src = fixture_source();
        let closure = resolver::resolve_closure(&src, &["foo"], ResolveOpts::default()).unwrap();
        let json = serde_json::to_string_pretty(&closure).unwrap();
        // Property: known root is present, dependency is included.
        assert!(json.contains("\"name\": \"foo\""));
        assert!(json.contains("\"name\": \"bar\""));
        // Provider mapping recorded for `sh -> bash`.
        assert!(json.contains("\"virtual_name\": \"sh\""));
        // databases_used preserved.
        assert!(json.contains("\"databases_used\""));
    }

    #[test]
    fn install_plan_skips_installed() {
        let mut src = fixture_source();
        src.mark_installed("bar");
        let plan = src.install_plan(&["foo"], false).unwrap();
        assert!(plan
            .to_install
            .iter()
            .any(|p| p.name == "foo"));
        assert!(!plan
            .to_install
            .iter()
            .any(|p| p.name == "bar"));
    }

    #[test]
    fn install_plan_clean_root_includes_all() {
        let mut src = fixture_source();
        src.mark_installed("bar");
        let plan = src.install_plan(&["foo"], true).unwrap();
        assert!(plan.clean_root);
        assert!(plan.to_install.iter().any(|p| p.name == "bar"));
    }

    #[test]
    fn fixture_loader_round_trip() {
        let dir = tempdir();
        let path = dir.join("fixture.json");
        let text = serde_json::json!({
            "packages": [{
                "name": "foo",
                "version": "1.0",
                "repo": "world",
                "depends": [{"name": "bar"}],
            }, {
                "name": "bar",
                "version": "1.0",
                "repo": "world"
            }],
            "installed": ["bar"],
            "databases": ["world"],
        })
        .to_string();
        std::fs::write(&path, text).unwrap();
        let mock = load_offline_fixture(&path).unwrap();
        assert_eq!(mock.databases(), vec!["world".to_string()]);
        let plan = mock.install_plan(&["foo"], false).unwrap();
        assert!(plan.to_install.iter().any(|p| p.name == "foo"));
        assert!(!plan.to_install.iter().any(|p| p.name == "bar"));
    }

    #[test]
    fn different_configs_yield_different_results() {
        // Same target, two different mock sources standing in for two
        // different pacman.conf files. The result must differ.
        let mut a = MockSource::default();
        a.insert(Package::new("foo", "1.0", "world"));
        a.set_databases(vec!["world".into()]);

        let mut b = MockSource::default();
        let mut foo = Package::new("foo", "2.0", "testing");
        foo.depends = vec![Dep::parse("newdep")];
        b.insert(foo);
        b.insert(Package::new("newdep", "0.1", "testing"));
        b.set_databases(vec!["testing".into()]);

        let ca = resolver::resolve_closure(&a, &["foo"], ResolveOpts::default()).unwrap();
        let cb = resolver::resolve_closure(&b, &["foo"], ResolveOpts::default()).unwrap();
        assert_ne!(ca.nodes.len(), cb.nodes.len());
        assert_ne!(ca.databases_used, cb.databases_used);
    }

    fn tempdir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("deploytix_pkgdeps_test_{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
