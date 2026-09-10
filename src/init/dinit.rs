//! dinit.

use super::InitModule;
use crate::config::InitSystem;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

pub const MODULE: InitModule = InitModule {
    id: InitSystem::Dinit,
    base_package: "dinit",
    service_dir: "/etc/dinit.d",
    enabled_dir: "/etc/dinit.d/boot.d",
    no_service_package: &[],
    enable,
    sync_repository: None,
    commit_database: None,
};

/// Enable a dinit service
fn enable(_cmd: &CommandRunner, service: &str, install_root: &str) -> Result<()> {
    let service_file_check = format!("{}/etc/dinit.d/{}", install_root, service);
    // Symlink target - path relative to installed system root
    let service_file_target = format!("/etc/dinit.d/{}", service);
    let enabled_dir = format!("{}/etc/dinit.d/boot.d", install_root);
    let link_path = format!("{}/{}", enabled_dir, service);

    if !Path::new(&service_file_check).exists() {
        warn!(
            "Service {} not found at {}, skipping",
            service, service_file_check
        );
        return Ok(());
    }

    fs::create_dir_all(&enabled_dir)?;

    if !Path::new(&link_path).exists() {
        std::os::unix::fs::symlink(&service_file_target, &link_path)?;
        info!("Created symlink {} -> {}", link_path, service_file_target);
    }

    Ok(())
}
