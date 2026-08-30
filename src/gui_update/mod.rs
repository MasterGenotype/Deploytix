//! Graphical front-end for `deploytix update` / `deploytix rollback`.
//!
//! Only meaningful on an immutable deployment, where the read-only `/usr`
//! makes `deploytix update` the sole way to install anything. The package that
//! carries this binary is installed only when `immutable_root` is set (see
//! `build_package_list` in [`crate::install::basestrap`]), so a mutable system
//! never receives it; [`app::Blocked`] is the belt-and-braces runtime check for
//! a hand-installed or migrated system.
//!
//! Kept separate from [`crate::gui`], which is the fullscreen install wizard.
//! Only `theme` and `widgets` are shared.

mod app;
pub mod model;
mod panels;
pub mod state;

pub use app::{Blocked, UpdateGui};
