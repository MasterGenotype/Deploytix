//! Headless/server installation (no desktop environment)

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use tracing::info;

/// Install headless/server configuration (no desktop)
pub fn install(
    _cmd: &CommandRunner,
    _config: &DeploymentConfig,
    _install_root: &str,
) -> Result<()> {
    info!("No desktop environment selected - headless/server mode");
    // Nothing to install for headless mode
    Ok(())
}
