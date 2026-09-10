//! Turning an installed base system into a usable one: identity, accounts,
//! networking, services, swap, and the initramfs that boots it.

pub mod hooks;
pub mod locale;
pub mod mkinitcpio;
pub mod network;
pub mod services;
pub mod swap;
pub mod users;
