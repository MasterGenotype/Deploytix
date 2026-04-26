//! Graphviz DOT serializer for [`DepClosure`] graphs.
//!
//! Output is intended to be byte-stable so it can be diffed in CI:
//! nodes and edges are emitted in the order they appear in the closure
//! (which the resolver already sorts), and node identifiers are quoted
//! to survive package names that contain hyphens or `+`.

use super::model::{DepClosure, EdgeKind};
use std::fmt::Write;

#[derive(Debug, Clone, Copy)]
pub struct DotOpts {
    /// Whether the roots should be emphasized in the output (bold).
    pub highlight_roots: bool,
}

impl Default for DotOpts {
    fn default() -> Self {
        Self {
            highlight_roots: true,
        }
    }
}

pub fn to_dot(closure: &DepClosure, opts: DotOpts) -> String {
    let mut out = String::new();
    out.push_str("digraph deps {\n");
    out.push_str("    rankdir=LR;\n");
    out.push_str("    node [shape=box, style=rounded, fontname=\"monospace\"];\n");
    out.push_str("    edge [fontname=\"monospace\", fontsize=9];\n");

    for pkg in &closure.nodes {
        let mut attrs = format!("label=\"{}\\n{}\"", escape(&pkg.name), escape(&pkg.version));
        if opts.highlight_roots && closure.roots.iter().any(|r| r == &pkg.name) {
            attrs.push_str(", style=\"rounded,bold\", color=\"navy\"");
        }
        let _ = writeln!(out, "    \"{}\" [{}];", escape(&pkg.name), attrs);
    }

    for unr in &closure.unresolved {
        let _ = writeln!(
            out,
            "    \"{}\" [label=\"{}\\n(unresolved)\", style=\"dashed\", color=\"gray60\"];",
            escape(unr),
            escape(unr)
        );
    }

    for edge in &closure.edges {
        let mut attrs = format!(
            "color=\"{}\", style=\"{}\", label=\"{}\"",
            edge.kind.dot_color(),
            edge.kind.dot_style(),
            edge_label(edge)
        );
        if matches!(edge.kind, EdgeKind::ReverseRuntime | EdgeKind::ReverseOptional) {
            attrs.push_str(", arrowhead=invempty");
        }
        let _ = writeln!(
            out,
            "    \"{}\" -> \"{}\" [{}];",
            escape(&edge.from),
            escape(&edge.to),
            attrs
        );
    }

    out.push_str("}\n");
    out
}

fn edge_label(edge: &super::model::DepEdge) -> String {
    match (&edge.constraint, edge.kind) {
        (Some(c), EdgeKind::Runtime | EdgeKind::Make | EdgeKind::Check) => {
            format!("{} {}", edge.kind.label(), escape(c))
        }
        _ => edge.kind.label().to_string(),
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkgdeps::model::{Dep, DepEdge, EdgeKind, Package};

    #[test]
    fn dot_basic_structure() {
        let mut closure = DepClosure {
            roots: vec!["foo".into()],
            nodes: vec![
                Package::new("foo", "1.0", "system"),
                Package::new("bar", "2.0", "system"),
            ],
            edges: vec![DepEdge {
                from: "foo".into(),
                to: "bar".into(),
                kind: EdgeKind::Runtime,
                constraint: Some(">=2.0".into()),
            }],
            unresolved: Vec::new(),
            providers: Vec::new(),
            databases_used: vec!["system".into()],
            warnings: Vec::new(),
        };
        // Force a single optdep to ensure dashed style is emitted.
        closure.edges.push(DepEdge {
            from: "foo".into(),
            to: "git".into(),
            kind: EdgeKind::Optional,
            constraint: None,
        });
        let dot = to_dot(&closure, DotOpts::default());
        assert!(dot.starts_with("digraph deps {"));
        assert!(dot.contains("\"foo\" [label=\"foo\\n1.0\""));
        assert!(dot.contains("\"foo\" -> \"bar\""));
        assert!(dot.contains("style=\"solid\""));
        assert!(dot.contains("style=\"dashed\""));
        assert!(dot.contains("depends >=2.0"));
    }

    #[test]
    fn dot_quotes_special_chars() {
        let mut p = Package::new("foo+", "1.0", "system");
        p.depends = vec![Dep::unversioned("bar-baz")];
        let closure = DepClosure {
            roots: vec!["foo+".into()],
            nodes: vec![p, Package::new("bar-baz", "1.0", "system")],
            edges: vec![DepEdge {
                from: "foo+".into(),
                to: "bar-baz".into(),
                kind: EdgeKind::Runtime,
                constraint: None,
            }],
            ..Default::default()
        };
        let dot = to_dot(&closure, DotOpts::default());
        assert!(dot.contains("\"foo+\""));
        assert!(dot.contains("\"bar-baz\""));
    }
}
