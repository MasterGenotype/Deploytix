//! greetd display manager configuration

use crate::config::{DeploymentConfig, DesktopEnvironment};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// Configure greetd for automatic login to desktop session
pub fn configure_greetd(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    // Only configure greetd if a desktop environment is selected
    if config.desktop.environment == DesktopEnvironment::None {
        info!("Skipping greetd configuration (no desktop environment selected)");
        return Ok(());
    }

    info!("Configuring greetd for user '{}' with session '{}'",
        config.user.name, get_session_command(&config.desktop.environment));

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure /etc/greetd/config.toml");
        println!("    user: {}", config.user.name);
        println!("    session: {}", get_session_command(&config.desktop.environment));
        return Ok(());
    }

    let username = &config.user.name;

    // Determine session command based on desktop environment
    let session_cmd = get_session_command(&config.desktop.environment);

    let config_content = format!(
        r#"[terminal]
vt = 1

[default_session]
command = "{session}"
user = "{user}"
"#,
        session = session_cmd,
        user = username,
    );

    let greetd_dir = format!("{}/etc/greetd", install_root);
    fs::create_dir_all(&greetd_dir)?;
    fs::write(format!("{}/config.toml", greetd_dir), config_content)?;

    info!("greetd config written to /etc/greetd/config.toml for user '{}'", username);
    Ok(())
}

/// Get the session command for a desktop environment
fn get_session_command(de: &DesktopEnvironment) -> &'static str {
    match de {
        DesktopEnvironment::Kde => "startplasma-wayland",
        DesktopEnvironment::Gnome => "gnome-session",
        DesktopEnvironment::Xfce => "startxfce4",
        DesktopEnvironment::None => "",
    }
}
