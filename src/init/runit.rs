//! runit.

use super::InitModule;
use crate::config::InitSystem;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

pub const MODULE: InitModule = InitModule {
    id: InitSystem::Runit,
    base_package: "runit",
    service_dir: "/etc/runit/sv",
    enabled_dir: "/run/runit/service",
    no_service_package: &[],
    enable,
    sync_repository: None,
    commit_database: None,
};

/// Enable a runit service by creating symlink from runsvdir/default to sv/
fn enable(_cmd: &CommandRunner, service: &str, install_root: &str) -> Result<()> {
    // Path to check if service exists (within install_root)
    let service_dir_check = format!("{}/etc/runit/sv/{}", install_root, service);
    // Symlink target - path relative to installed system root (not install_root)
    let service_dir_target = format!("/etc/runit/sv/{}", service);
    // Directory where symlinks are created
    let enabled_dir = format!("{}/etc/runit/runsvdir/default", install_root);
    let link_path = format!("{}/{}", enabled_dir, service);

    // Check if service exists in installed system
    if !Path::new(&service_dir_check).exists() {
        warn!(
            "Service {} not found at {}, skipping",
            service, service_dir_check
        );
        return Ok(());
    }

    // Create enabled directory if needed
    fs::create_dir_all(&enabled_dir)?;

    // Create symlink pointing to path relative to installed system root
    if !Path::new(&link_path).exists() {
        std::os::unix::fs::symlink(&service_dir_target, &link_path)?;
        info!("Created symlink {} -> {}", link_path, service_dir_target);
    }

    Ok(())
}
