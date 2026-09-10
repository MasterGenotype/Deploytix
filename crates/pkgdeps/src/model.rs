//! Normalized package metadata types.
//!
//! These types intentionally mirror the fields produced by libalpm /
//! `pacman -Si` so that a `Package` can be losslessly serialized as JSON
//! and compared across hosts.

use serde::{Deserialize, Serialize};

/// A versioned dependency expression as it appears in pacman metadata.
///
/// `name` is the package or virtual provider; `constraint` is the literal
/// version constraint suffix (`>=1.2.3`, `=4.5`, etc.) when present, or
/// `None` for an unversioned dep. `description` only applies to optional
/// dependencies (`optdepends` ships a free-form trailing description after
/// `: `).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dep {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub constraint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub description: Option<String>,
}

impl Dep {
    pub fn unversioned<S: Into<String>>(name: S) -> Self {
        Self {
            name: name.into(),
            constraint: None,
            description: None,
        }
    }

    /// Parse a single pacman dep token like `glibc>=2.39`, `sh`, or
    /// (for optdepends) `git: needed for AUR helpers`.
    ///
    /// Optional descriptions are split on the first `: ` — pacman's own
    /// format. Version constraint operators recognised: `>=`, `<=`, `=`,
    /// `>`, `<`.
    pub fn parse(token: &str) -> Self {
        let (head, description) = match token.split_once(": ") {
            Some((h, d)) => (h.trim(), Some(d.trim().to_string())),
            None => (token.trim(), None),
        };

        for op in [">=", "<=", "=", ">", "<"] {
            if let Some(idx) = head.find(op) {
                let (name, rest) = head.split_at(idx);
                return Self {
                    name: name.trim().to_string(),
                    constraint: Some(rest.trim().to_string()),
                    description,
                };
            }
        }

        Self {
            name: head.to_string(),
            constraint: None,
            description,
        }
    }

    /// Serialize back to the canonical `name<op><constraint>` form pacman
    /// emits. Description (optdepends only) is preserved on a separate
    /// `: ` suffix to round-trip cleanly.
    pub fn to_token(&self) -> String {
        let base = match &self.constraint {
            Some(c) => format!("{}{}", self.name, c),
            None => self.name.clone(),
        };
        match &self.description {
            Some(d) => format!("{}: {}", base, d),
            None => base,
        }
    }
}

/// Edge kinds for dependency graph output. Reverse edges share the
/// graph but are tagged separately so consumers can filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Runtime,
    Make,
    Check,
    Optional,
    ReverseRuntime,
    ReverseOptional,
}

impl EdgeKind {
    pub fn dot_color(self) -> &'static str {
        match self {
            EdgeKind::Runtime => "black",
            EdgeKind::Make => "blue",
            EdgeKind::Check => "darkgreen",
            EdgeKind::Optional => "gray50",
            EdgeKind::ReverseRuntime => "red",
            EdgeKind::ReverseOptional => "orange",
        }
    }

    pub fn dot_style(self) -> &'static str {
        match self {
            EdgeKind::Optional | EdgeKind::ReverseOptional => "dashed",
            _ => "solid",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EdgeKind::Runtime => "depends",
            EdgeKind::Make => "makedepends",
            EdgeKind::Check => "checkdepends",
            EdgeKind::Optional => "optdepends",
            EdgeKind::ReverseRuntime => "required_by",
            EdgeKind::ReverseOptional => "optional_for",
        }
    }
}

/// Normalized package metadata. Mirrors `pacman -Si` and libalpm's
/// `alpm_pkg_t` accessors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub repo: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub licenses: Vec<String>,
    #[serde(default)]
    pub groups: Vec<String>,
    #[serde(default)]
    pub depends: Vec<Dep>,
    #[serde(default)]
    pub makedepends: Vec<Dep>,
    #[serde(default)]
    pub checkdepends: Vec<Dep>,
    #[serde(default)]
    pub optdepends: Vec<Dep>,
    #[serde(default)]
    pub provides: Vec<Dep>,
    #[serde(default)]
    pub conflicts: Vec<Dep>,
    #[serde(default)]
    pub replaces: Vec<Dep>,
    /// Reverse runtime deps (`pactree -r`).
    #[serde(default)]
    pub required_by: Vec<String>,
    /// Reverse optional deps (`pactree -ro` equivalent).
    #[serde(default)]
    pub optional_for: Vec<String>,
}

impl Package {
    pub fn new<S: Into<String>>(name: S, version: S, repo: S) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            repo: repo.into(),
            arch: String::new(),
            description: String::new(),
            url: String::new(),
            licenses: Vec::new(),
            groups: Vec::new(),
            depends: Vec::new(),
            makedepends: Vec::new(),
            checkdepends: Vec::new(),
            optdepends: Vec::new(),
            provides: Vec::new(),
            conflicts: Vec::new(),
            replaces: Vec::new(),
            required_by: Vec::new(),
            optional_for: Vec::new(),
        }
    }
}

/// Result of recursively closing the dependency set of one or more
/// roots. `nodes` is keyed by package name so callers can join back to
/// metadata; `edges` preserves which kind of dep produced each link
/// for graph rendering.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DepClosure {
    pub roots: Vec<String>,
    pub nodes: Vec<Package>,
    pub edges: Vec<DepEdge>,
    /// Names that were referenced but could not be resolved to a real
    /// package (typically virtual providers with no installed/repo
    /// satisfier, or packages not in any configured sync DB).
    #[serde(default)]
    pub unresolved: Vec<String>,
    /// Provider mappings actually used (`virtual -> chosen`). Useful for
    /// debugging and stable JSON output.
    #[serde(default)]
    pub providers: Vec<ProviderChoice>,
    /// Repository databases consulted while resolving.
    #[serde(default)]
    pub databases_used: Vec<String>,
    /// Non-fatal warnings (stale db, missing repo, etc.).
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepEdge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub constraint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderChoice {
    pub virtual_name: String,
    pub chosen: String,
}

/// What `pacman -S --print` would actually do for a given target. Kept
/// distinct from `DepClosure` because metadata-says ≠ transaction-will:
/// already-installed packages are omitted, conflicts trigger removals,
/// replacements alter the node set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstallPlan {
    pub targets: Vec<String>,
    /// Packages the transaction would install (`repo/name version`).
    pub to_install: Vec<PlannedPackage>,
    /// Packages that would be removed due to conflicts/replacements.
    #[serde(default)]
    pub to_remove: Vec<String>,
    /// Total download size in bytes if reported by pacman.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub download_size: Option<u64>,
    /// Total installed size in bytes if reported by pacman.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub installed_size: Option<u64>,
    #[serde(default)]
    pub warnings: Vec<String>,
    /// True if planning was done against a clean root (no existing
    /// installed-db considered) — i.e. equivalent to a fresh chroot.
    #[serde(default)]
    pub clean_root: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedPackage {
    pub repo: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_unversioned_dep() {
        let d = Dep::parse("glibc");
        assert_eq!(d.name, "glibc");
        assert!(d.constraint.is_none());
    }

    #[test]
    fn parse_versioned_dep() {
        let d = Dep::parse("glibc>=2.39");
        assert_eq!(d.name, "glibc");
        assert_eq!(d.constraint.as_deref(), Some(">=2.39"));
        assert_eq!(d.to_token(), "glibc>=2.39");
    }

    #[test]
    fn parse_optdep_with_description() {
        let d = Dep::parse("git: needed for AUR helpers");
        assert_eq!(d.name, "git");
        assert!(d.constraint.is_none());
        assert_eq!(d.description.as_deref(), Some("needed for AUR helpers"));
        assert_eq!(d.to_token(), "git: needed for AUR helpers");
    }

    #[test]
    fn parse_optdep_versioned() {
        let d = Dep::parse("python>=3.10: for the foo plugin");
        assert_eq!(d.name, "python");
        assert_eq!(d.constraint.as_deref(), Some(">=3.10"));
        assert_eq!(d.description.as_deref(), Some("for the foo plugin"));
    }

    #[test]
    fn edge_kind_styles() {
        assert_eq!(EdgeKind::Optional.dot_style(), "dashed");
        assert_eq!(EdgeKind::Runtime.dot_style(), "solid");
    }
}
