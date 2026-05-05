//! Integration tests for the `pkgdeps` module: drive the public API end-to-end
//! with a `MockSource` standing in for pacman.

use deploytix::pkgdeps::cli as deps_cli;
use deploytix::pkgdeps::graph::{to_dot, DotOpts};
use deploytix::pkgdeps::model::{Dep, EdgeKind, Package};
use deploytix::pkgdeps::resolver::{resolve_closure, resolve_reverse, ResolveOpts};
use deploytix::pkgdeps::source::{MetadataSource, MockSource};
use std::path::PathBuf;

fn artix_universe() -> MockSource {
    let mut s = MockSource::default();
    s.set_databases(vec!["system".into(), "world".into(), "extra".into()]);

    // Realistic Artix-ish dep graph: every C package transitively depends on
    // glibc, mirroring how `pacman -Si` actually reports things on a real
    // system. Without these glibc edges the closures look unnaturally sparse
    // and miss the most important transitive dep in the universe.
    let mut base = Package::new("base", "1.0", "system");
    base.depends = vec![Dep::parse("glibc>=2.39"), Dep::parse("pacman")];
    base.optdepends = vec![Dep::parse("man-db: read manpages")];
    s.insert(base);

    let glibc = Package::new("glibc", "2.39-1", "system");
    s.insert(glibc);

    let mut pacman = Package::new("pacman", "6.1.0-1", "system");
    pacman.depends = vec![Dep::parse("glibc>=2.39"), Dep::parse("libalpm")];
    pacman.makedepends = vec![Dep::parse("meson")];
    pacman.checkdepends = vec![Dep::parse("python-pytest")];
    pacman.conflicts = vec![Dep::parse("pacman-mirrorlist")];
    pacman.replaces = vec![Dep::parse("pacman-contrib<1.0")];
    s.insert(pacman);

    let mut libalpm = Package::new("libalpm", "13.0", "system");
    libalpm.depends = vec![Dep::parse("glibc>=2.39")];
    s.insert(libalpm);

    let meson = Package::new("meson", "1.4", "extra");
    s.insert(meson);

    let pytest = Package::new("python-pytest", "8.0", "extra");
    s.insert(pytest);

    let mandb = Package::new("man-db", "2.12", "world");
    s.insert(mandb);

    // virtual provider: `sh` is provided by bash.
    let mut bash = Package::new("bash", "5.2", "system");
    bash.provides = vec![Dep::unversioned("sh")];
    s.insert(bash);

    // user-pkg depends on `sh` (virtual) and base.
    let mut user_pkg = Package::new("user-pkg", "0.1", "world");
    user_pkg.depends = vec![Dep::unversioned("sh"), Dep::unversioned("base")];
    user_pkg.optdepends = vec![Dep::parse("git: for AUR helpers")];
    s.insert(user_pkg);

    s
}

#[test]
fn full_runtime_closure_includes_transitive_deps() {
    let src = artix_universe();
    let closure = resolve_closure(&src, &["user-pkg"], ResolveOpts::default()).unwrap();
    let names: Vec<&str> = closure.nodes.iter().map(|p| p.name.as_str()).collect();
    for required in ["user-pkg", "base", "glibc", "pacman", "libalpm", "bash"] {
        assert!(names.contains(&required), "missing: {}", required);
    }
    // optdeps of base / user-pkg must NOT appear in default closure.
    assert!(!names.contains(&"man-db"));
    assert!(!names.contains(&"git"));
    // Provider mapping captured.
    assert!(closure
        .providers
        .iter()
        .any(|p| p.virtual_name == "sh" && p.chosen == "bash"));
    // Version constraint preserved on edges.
    assert!(closure
        .edges
        .iter()
        .any(|e| e.to == "glibc" && e.constraint.as_deref() == Some(">=2.39")));
    // databases_used reported.
    assert!(closure.databases_used.iter().any(|d| d == "system"));
}

#[test]
fn make_and_check_deps_kept_separate() {
    let src = artix_universe();

    let runtime = resolve_closure(&src, &["pacman"], ResolveOpts::default()).unwrap();
    assert!(!runtime.nodes.iter().any(|p| p.name == "meson"));
    assert!(!runtime.nodes.iter().any(|p| p.name == "python-pytest"));

    let with_make = resolve_closure(
        &src,
        &["pacman"],
        ResolveOpts {
            include_make: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(with_make.nodes.iter().any(|p| p.name == "meson"));
    assert!(!with_make.nodes.iter().any(|p| p.name == "python-pytest"));

    let with_check = resolve_closure(
        &src,
        &["pacman"],
        ResolveOpts {
            include_check: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(with_check.nodes.iter().any(|p| p.name == "python-pytest"));

    // Edges are tagged with the right kind.
    let make_edge = with_make
        .edges
        .iter()
        .find(|e| e.to == "meson")
        .expect("meson edge");
    assert!(matches!(make_edge.kind, EdgeKind::Make));
}

#[test]
fn reverse_dependency_walk() {
    let src = artix_universe();
    let rev = resolve_reverse(&src, "glibc", true, false).unwrap();
    let names: Vec<&str> = rev.nodes.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"base"));
    assert!(names.contains(&"glibc"));
    // user-pkg is a reverse-runtime of base, so transitively of glibc.
    assert!(names.contains(&"user-pkg"));
}

#[test]
fn install_plan_skips_already_installed() {
    let mut src = artix_universe();
    src.mark_installed("glibc");
    src.mark_installed("libalpm");
    let plan = src.install_plan(&["pacman"], false).unwrap();
    let install_names: Vec<&str> = plan.to_install.iter().map(|p| p.name.as_str()).collect();
    assert!(install_names.contains(&"pacman"));
    assert!(!install_names.contains(&"glibc"));
    assert!(!install_names.contains(&"libalpm"));
    assert!(!plan.clean_root);
}

#[test]
fn install_plan_clean_root_treats_all_as_missing() {
    let mut src = artix_universe();
    src.mark_installed("glibc");
    let plan = src.install_plan(&["pacman"], true).unwrap();
    let install_names: Vec<&str> = plan.to_install.iter().map(|p| p.name.as_str()).collect();
    assert!(install_names.contains(&"glibc"));
    assert!(plan.clean_root);
}

#[test]
fn dot_output_contains_expected_edges() {
    let src = artix_universe();
    let closure = resolve_closure(
        &src,
        &["base"],
        ResolveOpts {
            include_optional: true,
            ..Default::default()
        },
    )
    .unwrap();
    let dot = to_dot(&closure, DotOpts::default());
    assert!(dot.contains("digraph deps"));
    assert!(dot.contains("\"base\" -> \"glibc\""));
    assert!(dot.contains("\"base\" -> \"pacman\""));
    assert!(dot.contains("\"base\" -> \"man-db\""));
    // optdep edge is dashed.
    let mandb_line = dot
        .lines()
        .find(|l| l.contains("\"base\" -> \"man-db\""))
        .unwrap();
    assert!(mandb_line.contains("dashed"));
    // version constraint shows in edge label
    assert!(dot.contains("depends >=2.39"));
}

#[test]
fn json_output_schema_round_trips() {
    let src = artix_universe();
    let closure = resolve_closure(
        &src,
        &["user-pkg"],
        ResolveOpts {
            include_optional: true,
            ..Default::default()
        },
    )
    .unwrap();
    let json = serde_json::to_string(&closure).unwrap();
    let round: deploytix::pkgdeps::DepClosure = serde_json::from_str(&json).unwrap();
    assert_eq!(round.roots, closure.roots);
    assert_eq!(round.nodes.len(), closure.nodes.len());
    assert_eq!(round.edges.len(), closure.edges.len());
    // Stable, sorted ordering.
    let mut sorted_names = closure
        .nodes
        .iter()
        .map(|p| p.name.clone())
        .collect::<Vec<_>>();
    sorted_names.sort();
    assert_eq!(
        closure
            .nodes
            .iter()
            .map(|p| p.name.clone())
            .collect::<Vec<_>>(),
        sorted_names
    );
}

#[test]
fn different_configs_produce_different_results() {
    // Universe A: minimal `world` repo only.
    let mut a = MockSource::default();
    a.set_databases(vec!["world".into()]);
    a.insert(Package::new("foo", "1.0", "world"));

    // Universe B: same package but with deps in `testing` repo.
    let mut b = MockSource::default();
    b.set_databases(vec!["testing".into()]);
    let mut foo = Package::new("foo", "2.0", "testing");
    foo.depends = vec![Dep::unversioned("newdep")];
    b.insert(foo);
    b.insert(Package::new("newdep", "0.1", "testing"));

    let ca = resolve_closure(&a, &["foo"], ResolveOpts::default()).unwrap();
    let cb = resolve_closure(&b, &["foo"], ResolveOpts::default()).unwrap();
    assert_ne!(ca.nodes.len(), cb.nodes.len());
    assert_ne!(ca.databases_used, cb.databases_used);
    let foo_a = ca.nodes.iter().find(|p| p.name == "foo").unwrap();
    let foo_b = cb.nodes.iter().find(|p| p.name == "foo").unwrap();
    assert_ne!(foo_a.version, foo_b.version);
    assert_ne!(foo_a.repo, foo_b.repo);
}

/// Regression test for the bug where a virtual dep (e.g. `sh`) would
/// be marked unresolved when the underlying source could not match it
/// by name/description. The MockSource models a Provides-aware lookup;
/// the real `PacmanSource` was changed to consult sync DB Provides
/// metadata via expac rather than `pacman -Ss`. In both cases, the
/// closure must include the real provider node and an entry in
/// `providers`, with no `unresolved` for the virtual name.
#[test]
fn virtual_dep_resolves_to_provider_node_not_unresolved() {
    let src = artix_universe();
    let closure = resolve_closure(&src, &["user-pkg"], ResolveOpts::default()).unwrap();
    // `sh` is a virtual provided by bash; it must NOT appear as
    // unresolved.
    assert!(
        !closure.unresolved.iter().any(|u| u == "sh"),
        "virtual dep `sh` was recorded as unresolved; closure: {:?}",
        closure
    );
    // The real provider (bash) must be present as a node.
    assert!(closure.nodes.iter().any(|p| p.name == "bash"));
    // Provider mapping must record the choice.
    assert!(closure
        .providers
        .iter()
        .any(|p| p.virtual_name == "sh" && p.chosen == "bash"));
    // Edges originally pointing at `sh` must have been rewritten to
    // point at the real provider, so graph output references real
    // packages.
    assert!(closure
        .edges
        .iter()
        .any(|e| e.from == "user-pkg" && e.to == "bash"));
    assert!(!closure
        .edges
        .iter()
        .any(|e| e.from == "user-pkg" && e.to == "sh"));
}

#[test]
fn offline_fixture_round_trip() {
    let dir = std::env::temp_dir().join(format!(
        "deploytix_pkgdeps_offline_{}_{}",
        std::process::id(),
        line!()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path: PathBuf = dir.join("fix.json");
    let json = serde_json::json!({
        "packages": [
            {
                "name": "alpha",
                "version": "1.0",
                "repo": "world",
                "depends": [{"name": "beta", "constraint": ">=0.5"}],
                "optdepends": [{"name": "gamma", "description": "extra"}]
            },
            {"name": "beta", "version": "0.7", "repo": "world"}
        ],
        "installed": [],
        "databases": ["world"]
    })
    .to_string();
    std::fs::write(&path, json).unwrap();

    let mock = deps_cli::load_offline_fixture(&path).unwrap();
    let closure = resolve_closure(
        &mock,
        &["alpha"],
        ResolveOpts {
            include_optional: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(closure.nodes.len(), 2);
    assert_eq!(mock.databases(), vec!["world"]);
    let alpha = closure.nodes.iter().find(|p| p.name == "alpha").unwrap();
    assert_eq!(alpha.depends[0].constraint.as_deref(), Some(">=0.5"));
}
