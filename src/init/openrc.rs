//! OpenRC.

use super::InitModule;
use crate::config::InitSystem;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::path::Path;
use tracing::{info, warn};

pub const MODULE: InitModule = InitModule {
    id: InitSystem::OpenRC,
    base_package: "openrc",
    service_dir: "/etc/init.d",
    enabled_dir: "/etc/runlevels/default",
    no_service_package: &[],
    enable,
    sync_repository: None,
    commit_database: None,
};

/// Enable an OpenRC service
fn enable(cmd: &CommandRunner, service: &str, install_root: &str) -> Result<()> {
    let service_path = format!("{}/etc/init.d/{}", install_root, service);

    if !Path::new(&service_path).exists() {
        warn!(
            "Service {} not found at {}, skipping",
            service, service_path
        );
        return Ok(());
    }

    cmd.run_in_chroot(install_root, &format!("rc-update add {} default", service))?;
    info!("Enabled OpenRC service {}", service);

    Ok(())
}
