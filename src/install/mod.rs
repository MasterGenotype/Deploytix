//! Installation orchestration

mod basestrap;
mod chroot;
mod fstab;
mod installer;

pub use basestrap::*;
pub use chroot::*;
pub use fstab::*;
pub use installer::*;
