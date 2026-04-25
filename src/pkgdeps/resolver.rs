//! Recursive dependency closure with virtual-provider, conflict, and
//! reverse-dep handling.
//!
//! The resolver is intentionally pure-Rust on top of [`MetadataSource`]
//! so it works against either the real `PacmanSource` or `MockSource`.

use super::model::{Dep, DepClosure, DepEdge, EdgeKind, Package, ProviderChoice};
use super::source::MetadataSource;
use crate::utils::error::Result;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone, Copy, Default)]
pub struct ResolveOpts {
    pub include_optional: bool,
    pub include_make: bool,
    pub include_check: bool,
}

/// Compute the full dependency closure for one or more roots.
///
/// Visits each package once; tracks which `provides` virtual names mapped
/// to which concrete packages so the result is reproducible. Packages that
/// cannot be resolved are recorded in `unresolved` rather than aborting —
/// pacman itself will refuse such a transaction at install time, but
/// graph generation should still succeed.
pub fn resolve_closure<S: MetadataSource + ?Sized>(
    source: &S,
    roots: &[&str],
    opts: ResolveOpts,
) -> Result<DepClosure> {
    let mut visited: BTreeMap<String, Package> = BTreeMap::new();
    let mut edges: Vec<DepEdge> = Vec::new();
    let mut unresolved: BTreeSet<String> = BTreeSet::new();
    let mut providers: BTreeMap<String, String> = BTreeMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    for r in roots {
        queue.push_back(r.to_string());
    }

    while let Some(name) = queue.pop_front() {
        if visited.contains_key(&name) {
            continue;
        }
        let resolved = match source.package(&name)? {
            Some(p) => p,
            None => {
                // Maybe `name` is a virtual provider — try resolving.
                match source.provider_of(&name)? {
                    Some(provider) => {
                        providers.insert(name.clone(), provider.clone());
                        if provider == name {
                            unresolved.insert(name.clone());
                            continue;
                        }
                        if visited.contains_key(&provider) {
                            continue;
                        }
                        queue.push_back(provider);
                        continue;
                    }
                    None => {
                        unresolved.insert(name.clone());
                        continue;
                    }
                }
            }
        };

        let pkg_name = resolved.name.clone();

        // Enqueue dependencies.
        for d in &resolved.depends {
            edges.push(DepEdge {
                from: pkg_name.clone(),
                to: d.name.clone(),
                kind: EdgeKind::Runtime,
                constraint: d.constraint.clone(),
            });
            if !visited.contains_key(&d.name) {
                queue.push_back(d.name.clone());
            }
        }
        if opts.include_make {
            for d in &resolved.makedepends {
                edges.push(DepEdge {
                    from: pkg_name.clone(),
                    to: d.name.clone(),
                    kind: EdgeKind::Make,
                    constraint: d.constraint.clone(),
                });
                if !visited.contains_key(&d.name) {
                    queue.push_back((d.name.clone(), Vec::new()));
                }
            }
        }
        if opts.include_check {
            for d in &resolved.checkdepends {
                edges.push(DepEdge {
                    from: pkg_name.clone(),
                    to: d.name.clone(),
                    kind: EdgeKind::Check,
                    constraint: d.constraint.clone(),
                });
                if !visited.contains_key(&d.name) {
                    queue.push_back((d.name.clone(), Vec::new()));
                }
            }
        }
        if opts.include_optional {
            for d in &resolved.optdepends {
                edges.push(DepEdge {
                    from: pkg_name.clone(),
                    to: d.name.clone(),
                    kind: EdgeKind::Optional,
                    constraint: d.constraint.clone(),
                });
                if !visited.contains_key(&d.name) {
                    queue.push_back((d.name.clone(), Vec::new()));
                }
            }
        }

        visited.insert(pkg_name, resolved);
    }

    // Rewrite edge targets that point at virtual names whose provider
    // we already resolved, so the rendered graph references real
    // packages — pacman would have done this internally.
    for edge in edges.iter_mut() {
        if let Some(real) = providers.get(&edge.to) {
            edge.to = real.clone();
        }
    }

    let mut nodes: Vec<Package> = visited.into_values().collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    edges.sort_by(|a, b| {
        a.from
            .cmp(&b.from)
            .then_with(|| a.to.cmp(&b.to))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    edges.dedup();

    let mut warnings = source.staleness_warnings();
    if nodes.is_empty() && !roots.is_empty() {
        warnings.push(format!(
            "no packages resolved for roots: {} — sync database may be empty or stale",
            roots.join(", ")
        ));
    }

    Ok(DepClosure {
        roots: roots.iter().map(|s| s.to_string()).collect(),
        nodes,
        edges,
        unresolved: unresolved.into_iter().collect(),
        providers: providers
            .into_iter()
            .map(|(virtual_name, chosen)| ProviderChoice {
                virtual_name,
                chosen,
            })
            .collect(),
        databases_used: source.databases(),
        warnings,
    })
}

/// Build the reverse dependency closure ("what would break if we removed
/// `name`?"). Equivalent to `pactree -r -s <pkg>` when `recursive` is
/// true. Optional reverse deps (`optional_for`) are included when
/// `include_optional` is set.
pub fn resolve_reverse<S: MetadataSource + ?Sized>(
    source: &S,
    target: &str,
    recursive: bool,
    include_optional: bool,
) -> Result<DepClosure> {
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut nodes: BTreeMap<String, Package> = BTreeMap::new();
    let mut edges: Vec<DepEdge> = Vec::new();
    let mut queue: VecDeque<String> = VecDeque::new();

    queue.push_back(target.to_string());

    while let Some(name) = queue.pop_front() {
        if !visited.insert(name.clone()) {
            continue;
        }
        if let Some(p) = source.package(&name)? {
            nodes.entry(name.clone()).or_insert(p);
        }
        let parents = source.required_by(&name)?;
        for parent in parents {
            edges.push(DepEdge {
                from: parent.clone(),
                to: name.clone(),
                kind: EdgeKind::ReverseRuntime,
                constraint: None,
            });
            if recursive && !visited.contains(&parent) {
                queue.push_back(parent.clone());
            }
            if let Some(p) = source.package(&parent)? {
                nodes.entry(parent).or_insert(p);
            }
        }
        if include_optional {
            let opt_parents = source.optional_for(&name)?;
            for parent in opt_parents {
                edges.push(DepEdge {
                    from: parent.clone(),
                    to: name.clone(),
                    kind: EdgeKind::ReverseOptional,
                    constraint: None,
                });
                if recursive && !visited.contains(&parent) {
                    queue.push_back(parent.clone());
                }
                if let Some(p) = source.package(&parent)? {
                    nodes.entry(parent).or_insert(p);
                }
            }
        }
    }

    let mut node_vec: Vec<Package> = nodes.into_values().collect();
    node_vec.sort_by(|a, b| a.name.cmp(&b.name));
    edges.sort_by(|a, b| a.from.cmp(&b.from).then_with(|| a.to.cmp(&b.to)));
    edges.dedup();

    let mut warnings = source.staleness_warnings();
    if node_vec.len() <= 1 && edges.is_empty() {
        warnings.push(format!(
            "no packages depend on {} (or sync db not refreshed)",
            target
        ));
    }

    Ok(DepClosure {
        roots: vec![target.to_string()],
        nodes: node_vec,
        edges,
        unresolved: Vec::new(),
        providers: Vec::new(),
        databases_used: source.databases(),
        warnings,
    })
}

/// Compare two package metadata records and report differences. Used by
/// `deps compare`. Returns a list of human-readable lines, empty when
/// the two packages are dependency-identical.
pub fn diff_packages(a: &Package, b: &Package) -> Vec<String> {
    let mut out = Vec::new();
    if a.version != b.version {
        out.push(format!(
            "version: {} -> {}",
            a.version, b.version
        ));
    }
    let diff_list = |label: &str, av: &[Dep], bv: &[Dep], out: &mut Vec<String>| {
        let aset: BTreeSet<String> = av.iter().map(|d| d.to_token()).collect();
        let bset: BTreeSet<String> = bv.iter().map(|d| d.to_token()).collect();
        for added in bset.difference(&aset) {
            out.push(format!("+ {}: {}", label, added));
        }
        for removed in aset.difference(&bset) {
            out.push(format!("- {}: {}", label, removed));
        }
    };
    diff_list("depends", &a.depends, &b.depends, &mut out);
    diff_list("makedepends", &a.makedepends, &b.makedepends, &mut out);
    diff_list("checkdepends", &a.checkdepends, &b.checkdepends, &mut out);
    diff_list("optdepends", &a.optdepends, &b.optdepends, &mut out);
    diff_list("provides", &a.provides, &b.provides, &mut out);
    diff_list("conflicts", &a.conflicts, &b.conflicts, &mut out);
    diff_list("replaces", &a.replaces, &b.replaces, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkgdeps::model::Dep;
    use crate::pkgdeps::source::MockSource;

    fn pkg(name: &str, version: &str, deps: &[&str]) -> Package {
        let mut p = Package::new(name, version, "system");
        p.depends = deps.iter().map(|d| Dep::parse(d)).collect();
        p
    }

    #[test]
    fn direct_deps_resolved() {
        let mut s = MockSource::default();
        s.insert(pkg("foo", "1.0", &["bar", "baz>=2.0"]));
        s.insert(pkg("bar", "1.0", &[]));
        s.insert(pkg("baz", "2.5", &[]));
        s.set_databases(vec!["system".into()]);
        let closure = resolve_closure(&s, &["foo"], ResolveOpts::default()).unwrap();
        let names: Vec<&str> = closure.nodes.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["bar", "baz", "foo"]);
        let constraints: Vec<&str> = closure
            .edges
            .iter()
            .filter_map(|e| e.constraint.as_deref())
            .collect();
        assert!(constraints.contains(&">=2.0"));
    }

    #[test]
    fn recursive_closure() {
        let mut s = MockSource::default();
        s.insert(pkg("a", "1", &["b"]));
        s.insert(pkg("b", "1", &["c"]));
        s.insert(pkg("c", "1", &["d"]));
        s.insert(pkg("d", "1", &[]));
        let closure = resolve_closure(&s, &["a"], ResolveOpts::default()).unwrap();
        let names: Vec<&str> = closure.nodes.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn provider_resolution() {
        let mut s = MockSource::default();
        let mut a = pkg("a", "1", &["sh"]);
        a.depends = vec![Dep::unversioned("sh")];
        s.insert(a);
        let mut bash = pkg("bash", "5.2", &[]);
        bash.provides = vec![Dep::unversioned("sh")];
        s.insert(bash);
        let closure = resolve_closure(&s, &["a"], ResolveOpts::default()).unwrap();
        let names: Vec<&str> = closure.nodes.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"bash"));
        assert!(closure
            .providers
            .iter()
            .any(|p| p.virtual_name == "sh" && p.chosen == "bash"));
    }

    #[test]
    fn unresolved_recorded_not_fatal() {
        let mut s = MockSource::default();
        s.insert(pkg("a", "1", &["missing"]));
        let closure = resolve_closure(&s, &["a"], ResolveOpts::default()).unwrap();
        assert!(closure.unresolved.contains(&"missing".to_string()));
    }

    #[test]
    fn optional_only_when_requested() {
        let mut s = MockSource::default();
        let mut a = pkg("a", "1", &[]);
        a.optdepends = vec![Dep {
            name: "git".into(),
            constraint: None,
            description: Some("for AUR".into()),
        }];
        s.insert(a);
        s.insert(pkg("git", "2.45", &[]));

        let closure = resolve_closure(&s, &["a"], ResolveOpts::default()).unwrap();
        assert!(!closure
            .nodes
            .iter()
            .any(|p| p.name == "git"));

        let closure_opt = resolve_closure(
            &s,
            &["a"],
            ResolveOpts {
                include_optional: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(closure_opt.nodes.iter().any(|p| p.name == "git"));
    }

    #[test]
    fn make_and_check_deps_separate() {
        let mut s = MockSource::default();
        let mut a = pkg("a", "1", &[]);
        a.makedepends = vec![Dep::unversioned("rustc")];
        a.checkdepends = vec![Dep::unversioned("python-pytest")];
        s.insert(a);
        s.insert(pkg("rustc", "1.80", &[]));
        s.insert(pkg("python-pytest", "8.0", &[]));

        let runtime = resolve_closure(&s, &["a"], ResolveOpts::default()).unwrap();
        assert_eq!(runtime.nodes.len(), 1);

        let with_make = resolve_closure(
            &s,
            &["a"],
            ResolveOpts {
                include_make: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(with_make.nodes.iter().any(|p| p.name == "rustc"));
        assert!(!with_make.nodes.iter().any(|p| p.name == "python-pytest"));
    }

    #[test]
    fn reverse_lookup() {
        let mut s = MockSource::default();
        s.insert(pkg("a", "1", &["target"]));
        s.insert(pkg("b", "1", &["target"]));
        s.insert(pkg("target", "1", &[]));
        let rev = resolve_reverse(&s, "target", true, false).unwrap();
        let names: Vec<&str> = rev.nodes.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
        assert!(names.contains(&"target"));
    }

    #[test]
    fn diff_versions_and_deps() {
        let a = pkg("foo", "1.0", &["bar"]);
        let b = pkg("foo", "1.1", &["bar", "baz"]);
        let diff = diff_packages(&a, &b);
        assert!(diff.iter().any(|l| l.contains("version")));
        assert!(diff.iter().any(|l| l == "+ depends: baz"));
    }
}
