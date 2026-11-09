//! AUR support: what the system can build with, and where it builds.
//!
//! Split by what each part answers:
//!
//! * [`build`] — *where* a build happens. The disk-backed scratch that keeps
//!   package builds off the chroot's half-RAM `/tmp`.
//! * [`helper`] — *what* drives the build. Detection of installed AUR helpers
//!   and the command each one needs, since a deployed system may have paru,
//!   yay, something else, or nothing.
//! * [`capability`] — *whether* a build can run at all: helper, build user and
//!   `base-devel`, each with a reason when missing.
//! * [`keys`] — *whether a build can verify its sources*: the PGP keys its
//!   PKGBUILD names, imported from more than one keyserver before building.
//! * [`keyring`] — the keyring builds verify with: deploytix's own, without
//!   keyboxd, whose locks a chroot's PID namespace strands.
//! * [`srcinfo`] — *what a base builds from*: its `.SRCINFO`, read once per
//!   batch for the key check above.
//! * [`rpc`] — *what exists*: a read-only client for the AUR's RPC interface.
//! * [`source`] — AUR packages as a `pkgdeps` metadata source, and the
//!   composite that resolves a dependency graph spanning the repositories and
//!   the AUR together.
//! * [`upgrade`] — *what is out of date*: installed AUR packages the AUR has a
//!   newer version of, for `deploytix update --aur`.
//!
//! Nothing here starts a transaction. Building on an immutable root goes
//! through [`crate::immutable::update::run_in_new_set`] like every other
//! package operation, so it inherits snapshot, activate and
//! discard-on-failure without reimplementing them.

pub mod build;
pub mod capability;
pub mod helper;
pub mod install;
pub mod keyring;
pub mod keys;
pub mod rpc;
pub mod search;
pub mod source;
pub mod srcinfo;
pub mod upgrade;

pub use capability::{AurCapability, Blocker, BuildUser, BuildUserSource};
pub use helper::AurHelper;
pub use install::{run_aur_install, AurInstallOptions};
pub use search::{rank, RankedHit};
pub use source::{system_source, AurSource, CompositeSource};
