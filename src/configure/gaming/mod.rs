//! The gaming and handheld stack: the graphical session that starts at boot,
//! switching between desktop and game modes, and the controller quirks a
//! handheld needs before either works.

pub mod display_manager;
pub mod gamescope_update;
pub mod greetd;
pub mod handheld_quirks;
pub mod session_switching;
