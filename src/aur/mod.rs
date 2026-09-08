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
//! * [`rpc`] — *what exists*: a read-only client for the AUR's RPC interface.
//! * [`source`] — AUR packages as a `pkgdeps` metadata source, and the
//!   composite that resolves a dependency graph spanning the repositories and
//!   the AUR together.
//!
//! Nothing here starts a transaction. Building on an immutable root goes
//! through [`crate::immutable::update::run_in_new_set`] like every other
//! package operation, so it inherits snapshot, activate and
//! discard-on-failure without reimplementing them.

pub mod build;
pub mod capability;
pub mod helper;
pub mod rpc;
pub mod source;

pub use capability::{AurCapability, Blocker, BuildUser, BuildUserSource};
pub use helper::AurHelper;
pub use source::{system_source, AurSource, CompositeSource};
