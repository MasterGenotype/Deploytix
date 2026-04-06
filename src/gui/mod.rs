//! Deploytix GUI module
//!
//! Provides a graphical wizard interface for configuring and running
//! Artix Linux deployments.

mod app;
mod panels;
pub mod state;
pub mod theme;
pub mod widgets;

pub use app::DeploytixGui;
