//! greetd display manager configuration

use crate::config::{DeploymentConfig, DesktopEnvironment, InitSystem};
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

/// Construct s6 service files for greetd
///
/// Creates the s6 longrun service directory structure at `/etc/s6/sv/greetd/`
/// within the install root. This is intended to be run during system installation
/// so the service is available for enablement without relying on a packaged
/// service definition.
///
/// Generated structure:
/// ```text
/// /etc/s6/sv/greetd/
/// ├── type              # "longrun"
/// ├── run               # Execution script
/// ├── finish            # Cleanup script
/// ├── notification-fd   # Readiness notification fd
/// ├── dependencies.d/
/// │   └── seatd         # Depends on seat manager
/// └── log/
///     ├── type           # "longrun"
///     └── run            # Logging pipeline script
/// ```
pub fn create_greetd_s6_service(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if config.desktop.environment == DesktopEnvironment::None {
        info!("Skipping greetd s6 service creation (no desktop environment selected)");
        return Ok(());
    }

    if config.system.init != InitSystem::S6 {
        info!("Skipping greetd s6 service creation (init system is {})", config.system.init);
        return Ok(());
    }

    info!("Creating s6 service files for greetd");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would create s6 service directory at /etc/s6/sv/greetd/");
        println!("  [dry-run] Would create run, finish, type, notification-fd");
        println!("  [dry-run] Would create dependencies.d/seatd");
        println!("  [dry-run] Would create log/run, log/type");
        return Ok(());
    }

    let sv_dir = format!("{}/etc/s6/sv/greetd", install_root);
    let deps_dir = format!("{}/dependencies.d", sv_dir);
    let log_dir = format!("{}/log", sv_dir);

    fs::create_dir_all(&deps_dir)?;
    fs::create_dir_all(&log_dir)?;

    // Service type: longrun (persistent daemon)
    fs::write(format!("{}/type", sv_dir), "longrun\n")?;

    // Run script: the main greetd execution
    let run_script = r#"#!/bin/execlineb -P
fdmove -c 2 1
exec greetd
"#;
    fs::write(format!("{}/run", sv_dir), run_script)?;

    // Finish script: cleanup on service stop
    let finish_script = r#"#!/bin/execlineb -P
foreground { s6-sleep 1 }
"#;
    fs::write(format!("{}/finish", sv_dir), finish_script)?;

    // Readiness notification: greetd writes to fd 3 when ready
    fs::write(format!("{}/notification-fd", sv_dir), "3\n")?;

    // Dependency on seatd (seat management must be running first)
    fs::write(format!("{}/seatd", deps_dir), "")?;

    // Logging pipeline
    fs::write(format!("{}/type", log_dir), "longrun\n")?;

    let log_run_script = r#"#!/bin/execlineb -P
s6-log -b -- n20 s1000000 /var/log/greetd/
"#;
    fs::write(format!("{}/run", log_dir), log_run_script)?;

    // Set executable permissions on run/finish scripts
    set_executable(&format!("{}/run", sv_dir))?;
    set_executable(&format!("{}/finish", sv_dir))?;
    set_executable(&format!("{}/run", log_dir))?;

    // Create log output directory
    let log_output_dir = format!("{}/var/log/greetd", install_root);
    fs::create_dir_all(&log_output_dir)?;

    info!("s6 service files for greetd created at /etc/s6/sv/greetd/");
    Ok(())
}

/// Set executable permission (0755) on a file
fn set_executable(path: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

/// Get the session command for a desktop environment
fn get_session_command(de: &DesktopEnvironment) -> &'static str {
    match de {
        DesktopEnvironment::Kde => "dbus-launch startplasma-wayland",
        DesktopEnvironment::Gnome => "dbus-launch gnome-session",
        DesktopEnvironment::Xfce => "dbus-launch startxfce4",
        DesktopEnvironment::None => "",
    }
}
