//! Package dependency tracking for Artix/Arch packages.
//!
//! Resolves runtime, build-time, optional, and reverse dependencies from
//! the pacman/libalpm sync database — never by scraping the Artix website.
//! See `docs` and the project README for the rationale.
//!
//! Submodules:
//! * [`model`] — normalized package metadata types (`Package`, `Dep`,
//!   `EdgeKind`, `DepClosure`, etc.).
//! * [`source`] — the [`source::MetadataSource`] trait and an in-memory
//!   [`source::MockSource`] used by tests and `--offline` mode.
//! * [`pacman`] — production backend that shells out to
//!   `pacman` / `pactree` / `expac` through a [`crate::utils::command::CommandRunner`].
//! * [`resolver`] — recursive closure, virtual provider resolution,
//!   conflicts, and reverse-dep walking.
//! * [`graph`] — Graphviz DOT output equivalent to `pactree -s -g`.
//! * [`cli`] — the `deploytix deps …` subcommand handlers.

pub mod cli;
pub mod graph;
pub mod model;
pub mod pacman;
pub mod preflight;
pub mod resolver;
pub mod source;

pub use model::{Dep, DepClosure, DepKind, EdgeKind, InstallPlan, Package, PackageRef};
pub use preflight::{preflight_chroot, preflight_host, PreflightReport};
pub use source::{MetadataSource, MockSource};
