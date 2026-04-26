//! Integration tests for the dependency-resolution preflight used before
//! basestrap (on the host) and `pacman -S` (in the chroot).
//!
//! These tests drive the public preflight API with a `MockSource` that
//! stands in for pacman, so they exercise the resolver path that the
//! real `PacmanSource` triggers without needing pacman/expac on $PATH.

use deploytix::pkgdeps::model::{Dep, Package};
use deploytix::pkgdeps::preflight::{preflight_chroot, preflight_host, resolve, PreflightReport};
use deploytix::pkgdeps::source::MockSource;

fn pkg(name: &str, version: &str, repo: &str, deps: &[&str]) -> Package {
    let mut p = Package::new(name, version, repo);
    p.depends = deps.iter().map(|d| Dep::parse(d)).collect();
    p
}

/// Mirrors a stripped-down basestrap target list for an Artix install:
/// base + linux-zen + an init-system base package, with a virtual
/// provider for `sh` to ensure that path is exercised end-to-end.
fn basestrap_universe() -> MockSource {
    let mut s = MockSource::default();
    s.set_databases(vec!["system".into(), "world".into(), "extra".into()]);
    s.insert(pkg("base", "1.0", "system", &["glibc", "sh"]));
    s.insert(pkg("glibc", "2.39", "system", &[]));
    s.insert(pkg("linux-zen", "6.10", "system", &[]));
    s.insert(pkg("linux-zen-headers", "6.10", "system", &[]));
    s.insert(pkg("linux-firmware", "20240101", "system", &[]));
    s.insert(pkg("runit", "2.2", "world", &["base"]));
    s.insert(pkg("base-devel", "1.0", "system", &[]));
    let mut bash = pkg("bash", "5.2", "system", &[]);
    bash.provides = vec![Dep::unversioned("sh")];
    s.insert(bash);
    s
}

#[test]
fn preflight_resolves_basestrap_targets_via_metadata_source() {
    let src = basestrap_universe();
    // The targets a typical basestrap call would receive — same shape
    // as `build_package_list` produces for a runit Artix system.
    let targets = vec![
        "base".to_string(),
        "base-devel".to_string(),
        "runit".to_string(),
        "linux-zen".to_string(),
        "linux-zen-headers".to_string(),
        "linux-firmware".to_string(),
    ];

    let report: PreflightReport = resolve(&src, &targets, true, "test basestrap").unwrap();

    assert!(!report.skipped, "preflight should not be skipped");
    assert!(
        report.is_resolvable(),
        "all basestrap targets must resolve; unresolved = {:?}",
        report.unresolved
    );
    // glibc + bash come in transitively from `base`'s deps; verify the
    // plan reflects pacman's real transaction shape.
    assert!(
        report.planned_install_count >= targets.len() + 2,
        "expected transitive deps in plan, got {}",
        report.planned_install_count
    );
}

#[test]
fn preflight_flags_unresolvable_basestrap_target() {
    let src = basestrap_universe();
    // Add a typo'd package name to simulate a broken
    // `build_package_list` output.
    let targets = vec!["base".to_string(), "linux-zenn".to_string()];
    let report = resolve(&src, &targets, true, "test").unwrap();
    assert!(!report.is_resolvable());
    assert!(report.unresolved.iter().any(|u| u == "linux-zenn"));
    // The good target is still planned.
    assert!(report.planned_install_count >= 1);
}

#[test]
fn preflight_surfaces_conflict_driven_removals() {
    // Simulate the case where the chroot already has an `oldpkg` and
    // we're asking pacman to install something that `Conflicts =` it.
    // The preflight must surface the removal so an unattended install
    // doesn't silently lose user data.
    let mut src = basestrap_universe();
    let mut newpkg = pkg("newpkg", "1.0", "world", &[]);
    newpkg.conflicts = vec![Dep::unversioned("oldpkg")];
    src.insert(newpkg);
    src.insert(pkg("oldpkg", "0.9", "world", &[]));
    src.mark_installed("oldpkg");

    let report = resolve(&src, &["newpkg".to_string()], false, "chroot").unwrap();
    assert!(report.is_resolvable());
    assert!(
        report.to_remove.iter().any(|n| n == "oldpkg"),
        "expected oldpkg in to_remove; got {:?}",
        report.to_remove
    );
}

#[test]
fn dry_run_skips_host_preflight() {
    // dry_run=true must short-circuit without trying to invoke pacman.
    let r = preflight_host(None, &["base".to_string()], true).unwrap();
    assert!(r.skipped);
    assert!(r.warnings.iter().any(|w| w.contains("dry-run")));
}

#[test]
fn dry_run_skips_chroot_preflight() {
    let r = preflight_chroot("/install", &["base".to_string()], true).unwrap();
    assert!(r.skipped);
}

#[test]
fn chroot_preflight_skipped_when_pacman_conf_missing() {
    // Live (dry_run=false) but the install root has no pacman.conf yet
    // (e.g. preflight called too early in the install pipeline). The
    // helper must report skipped + a clear warning rather than blowing
    // up.
    let r = preflight_chroot(
        "/var/empty/deploytix-no-such-install-root",
        &["base".to_string()],
        false,
    )
    .unwrap();
    assert!(r.skipped);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("pacman.conf not found")),
        "warnings = {:?}",
        r.warnings
    );
}

#[test]
fn empty_target_list_returns_clean_report() {
    let src = basestrap_universe();
    let r = resolve(&src, &[], true, "test").unwrap();
    assert!(!r.skipped);
    assert!(r.is_resolvable());
    assert_eq!(r.planned_install_count, 0);
    assert!(r.unresolved.is_empty());
    assert!(r.to_remove.is_empty());
}
