//! System configuration modules.
//!
//! Four unrelated jobs live here, grouped into a directory each: `crypto`
//! (encryption, keys, verity, SecureBoot), `system` (identity, accounts,
//! network, services, initramfs), `boot` (GRUB) and `gaming` (the graphical
//! session and handheld quirks).
//!
//! The individual modules are re-exported flat, so call sites elsewhere name
//! `configure::services` rather than `configure::system::services`. The
//! grouping is for whoever is reading the folder, not for the callers.

pub mod boot;
pub mod crypto;
pub mod gaming;
pub mod system;

pub use boot::{bootloader, grub_btrfs};
pub use crypto::{encryption, keyfiles, secureboot, verity};
pub use gaming::{display_manager, gamescope_update, greetd, handheld_quirks, session_switching};
pub use system::{hooks, locale, mkinitcpio, network, services, swap, users};
