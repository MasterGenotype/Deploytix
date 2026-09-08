//! Dependency preview for the Update tab, built on [`crate::pkgdeps`].
//!
//! # Why the updater needs this more than a normal system does
//!
//! On a mutable system a surprising transaction is annoying; you undo it. Here
//! `/usr` is read-only, so the only undo is a reboot into the previous
//! snapshot set. Finding out what a package drags in *after* staging it is
//! therefore the wrong order, and the resolver to answer it beforehand has been
//! in the tree all along — `deploytix deps` uses it, the GUI never did.
//!
//! # Two questions, two sources
//!
//! [`crate::pkgdeps::source::MetadataSource::install_plan`] answers "what would
//! this transaction actually do", via `pacman -S --print`: already-installed
//! packages are omitted, conflicts appear as removals, and sizes come from
//! pacman rather than being summed by hand.
//!
//! `resolve_closure` answers "what is reachable from here", which is a
//! different question and the one that surfaces **unresolved** names. That
//! matters because the AUR is invisible to `pkgdeps` by design — its module doc
//! is explicit that it reads the sync database and never scrapes. So a package
//! that is not in any sync DB is not necessarily a typo; it is very often an
//! AUR package. This module reports that distinction rather than showing an
//! empty result for it.
//!
//! Everything here is read-only. No transaction, no root, nothing to undo.

use crate::pkgdeps::model::InstallPlan;
use crate::pkgdeps::pacman::{PacmanConfig, PacmanSource};
use crate::pkgdeps::resolver::{resolve_closure, ResolveOpts};
use crate::pkgdeps::source::MetadataSource;
use crate::utils::error::Result;

/// What resolving a set of package names turned up.
#[derive(Debug, Clone, Default)]
pub struct Preview {
    /// The names asked about.
    pub targets: Vec<String>,
    /// What pacman says the transaction would do. `None` when planning failed
    /// (no network, unknown target); `notes` then says why.
    pub plan: Option<InstallPlan>,
    /// Targets no sync database knows. Usually AUR packages, sometimes typos —
    /// this module cannot tell the difference and does not pretend to.
    pub unknown: Vec<String>,
    /// Non-fatal explanations for the user.
    pub notes: Vec<String>,
}

impl Preview {
    /// Packages the transaction would newly install.
    pub fn install_count(&self) -> usize {
        self.plan.as_ref().map_or(0, |p| p.to_install.len())
    }

    /// Packages the transaction would remove to satisfy conflicts.
    pub fn removal_count(&self) -> usize {
        self.plan.as_ref().map_or(0, |p| p.to_remove.len())
    }

    /// Whether anything about this needs the user's attention before staging.
    ///
    /// Removals are the loud case: on a read-only root an unexpected removal is
    /// the hardest thing to walk back.
    pub fn needs_attention(&self) -> bool {
        self.removal_count() > 0 || !self.unknown.is_empty()
    }

    /// One-line summary for a collapsed view.
    pub fn summary(&self) -> String {
        if self.targets.is_empty() {
            return String::new();
        }
        let mut parts = Vec::new();
        match &self.plan {
            Some(p) if !p.to_install.is_empty() => {
                parts.push(format!("{} to install", p.to_install.len()));
            }
            Some(_) => parts.push("nothing to install".to_string()),
            None => {}
        }
        if self.removal_count() > 0 {
            parts.push(format!("{} to remove", self.removal_count()));
        }
        if !self.unknown.is_empty() {
            parts.push(format!("{} not in any repo", self.unknown.len()));
        }
        if let Some(size) = self.plan.as_ref().and_then(|p| p.download_size) {
            parts.push(format!("{} to download", human_size(size)));
        }
        if parts.is_empty() {
            "Nothing to do".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// Render a byte count the way a package manager does.
///
/// Base 1024 with one decimal, matching pacman's own output so the number in
/// the GUI matches the number in the log pane.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Resolve `targets` against `source`.
///
/// Split from [`resolve`] so the whole thing is testable against
/// [`crate::pkgdeps::source::MockSource`] with no pacman on the machine.
pub fn resolve_with<S: MetadataSource + ?Sized>(source: &S, targets: &[String]) -> Preview {
    let mut preview = Preview {
        targets: targets.to_vec(),
        ..Default::default()
    };
    if targets.is_empty() {
        return preview;
    }

    let refs: Vec<&str> = targets.iter().map(String::as_str).collect();

    // Which targets no sync DB knows. Asked first, because it explains a
    // failing plan: pacman cannot plan a transaction for a package it has
    // never heard of.
    match resolve_closure(source, &refs, ResolveOpts::default()) {
        Ok(closure) => {
            preview.unknown = closure
                .unresolved
                .iter()
                .filter(|name| targets.iter().any(|t| t == *name))
                .cloned()
                .collect();
            preview.notes.extend(closure.warnings);
        }
        Err(e) => preview
            .notes
            .push(format!("Dependency resolution failed: {e}")),
    }

    if !preview.unknown.is_empty() {
        preview.notes.push(format!(
            "Not in any configured repository: {}. AUR packages are not in a \
             sync database, so this is expected for them and needs a helper to \
             build.",
            preview.unknown.join(", ")
        ));
    }

    // What pacman would actually do. Planning is attempted even with unknown
    // targets, so a mixed repo/AUR selection still previews its repo half.
    let plannable: Vec<&str> = refs
        .iter()
        .copied()
        .filter(|t| !preview.unknown.iter().any(|u| u == *t))
        .collect();
    if !plannable.is_empty() {
        match source.install_plan(&plannable, false) {
            Ok(plan) => {
                preview.notes.extend(plan.warnings.clone());
                preview.plan = Some(plan);
            }
            Err(e) => preview.notes.push(format!("Could not plan install: {e}")),
        }
    }

    preview
}

/// Resolve `targets` against the live system's pacman databases.
///
/// Read-only and unprivileged: it shells out to `pacman -S --print` and
/// friends, which need no root and change nothing. Blocking, so callers run it
/// on a worker thread.
pub fn resolve(targets: &[String]) -> Result<Preview> {
    let source = PacmanSource::system(PacmanConfig::default());
    Ok(resolve_with(&source, targets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkgdeps::model::{Dep, Package, PlannedPackage};
    use crate::pkgdeps::source::MockSource;

    fn pkg(name: &str, deps: &[&str]) -> Package {
        let mut p = Package::new(name, "1.0", "extra");
        p.depends = deps.iter().map(|d| Dep::parse(d)).collect();
        p
    }

    fn source_with(packages: Vec<Package>) -> MockSource {
        let mut s = MockSource::default();
        for p in packages {
            s.insert(p);
        }
        s
    }

    #[test]
    fn empty_targets_resolve_to_an_empty_preview() {
        let s = source_with(vec![]);
        let p = resolve_with(&s, &[]);
        assert!(p.targets.is_empty());
        assert!(p.plan.is_none());
        assert!(p.summary().is_empty());
        assert!(!p.needs_attention());
    }

    #[test]
    fn a_known_package_is_not_reported_unknown() {
        let s = source_with(vec![pkg("vim", &["glibc"]), pkg("glibc", &[])]);
        let p = resolve_with(&s, &["vim".to_string()]);
        assert!(p.unknown.is_empty(), "unknown: {:?}", p.unknown);
    }

    #[test]
    fn a_package_no_repo_knows_is_flagged_as_probably_aur() {
        let s = source_with(vec![pkg("vim", &[])]);
        let p = resolve_with(&s, &["hhd-git".to_string()]);
        assert_eq!(p.unknown, vec!["hhd-git".to_string()]);
        assert!(p.needs_attention());
        assert!(
            p.notes.iter().any(|n| n.contains("AUR")),
            "the note should explain why it is missing: {:?}",
            p.notes
        );
    }

    #[test]
    fn only_targets_are_reported_unknown_not_transitive_deps() {
        // A missing transitive dep is pacman's problem to report at install
        // time; surfacing it here as "probably AUR" would be wrong and noisy.
        let s = source_with(vec![pkg("vim", &["some-missing-lib"])]);
        let p = resolve_with(&s, &["vim".to_string()]);
        assert!(
            p.unknown.is_empty(),
            "transitive miss leaked into unknown: {:?}",
            p.unknown
        );
    }

    #[test]
    fn a_mixed_selection_still_previews_its_repo_half() {
        let s = source_with(vec![pkg("vim", &[])]);
        let p = resolve_with(&s, &["vim".to_string(), "hhd-git".to_string()]);
        assert_eq!(p.unknown, vec!["hhd-git".to_string()]);
        // MockSource plans only what it knows; the point is that an unknown
        // target does not abort the whole preview.
        assert_eq!(p.targets.len(), 2);
    }

    #[test]
    fn removals_demand_attention() {
        let p = Preview {
            targets: vec!["a".into()],
            plan: Some(InstallPlan {
                to_install: vec![PlannedPackage {
                    repo: "extra".into(),
                    name: "a".into(),
                    version: "1".into(),
                }],
                to_remove: vec!["conflicting".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(p.needs_attention());
        assert!(p.summary().contains("1 to remove"));
    }

    #[test]
    fn a_clean_install_does_not_demand_attention() {
        let p = Preview {
            targets: vec!["a".into()],
            plan: Some(InstallPlan {
                to_install: vec![PlannedPackage {
                    repo: "extra".into(),
                    name: "a".into(),
                    version: "1".into(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!p.needs_attention());
        assert_eq!(p.install_count(), 1);
    }

    #[test]
    fn summary_reports_download_size_when_pacman_gave_one() {
        let p = Preview {
            targets: vec!["a".into()],
            plan: Some(InstallPlan {
                to_install: vec![PlannedPackage {
                    repo: "extra".into(),
                    name: "a".into(),
                    version: "1".into(),
                }],
                download_size: Some(5 * 1024 * 1024),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(p.summary().contains("5.0 MiB"), "got: {}", p.summary());
    }

    #[test]
    fn sizes_render_in_the_units_pacman_uses() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
