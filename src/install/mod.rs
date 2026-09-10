//! Installation orchestration

mod basestrap;
mod chroot;
pub mod crypttab;
pub mod fstab;
mod installer;
/// Installing software into the target: kernels, GPU drivers, the gaming
/// stack, AUR builds. Everything in `configure/` sets up a system that is
/// already installed; this puts more software on it.
pub mod packages;

pub use basestrap::*;
pub use chroot::*;
pub use fstab::*;
pub use installer::*;
