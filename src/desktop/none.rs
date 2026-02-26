//! Headless/server installation (no desktop environment)

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use tracing::info;

/// Install headless/server configuration (no desktop)
#[allow(dead_code)]
pub fn install(
    _cmd: &CommandRunner,
    _config: &DeploymentConfig,
    _install_root: &str,
) -> Result<()> {
    info!("No desktop environment selected - headless/server mode");
    // Nothing to install for headless mode
    Ok(())
}

/// Generate generic desktop file content (no DE-specific features)
pub fn desktop_file_content(bindir: &str) -> String {
    format!(
        r#"[Desktop Entry]
Type=Application
Name=Deploytix
GenericName=Artix Linux Installer
Comment=Automated Artix Linux deployment installer
Exec=pkexec {}/deploytix-gui
Icon=system-software-install
NoDisplay=false
StartupNotify=true
Terminal=false
Categories=System;Settings;
Keywords=linux;installer;artix;deployment;
"#,
        bindir
    )
}
