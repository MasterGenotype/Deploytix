//! Deploytix library - Artix Linux deployment automation

pub mod cleanup;
pub mod config;
pub mod configure;
pub mod desktop;
pub mod disk;
pub mod install;
pub mod resources;
pub mod utils;

pub use config::DeploymentConfig;
pub use utils::error::DeploytixError;
