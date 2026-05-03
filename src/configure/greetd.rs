//! greetd display manager configuration

use crate::config::{DeploymentConfig, DesktopEnvironment, InitSystem};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
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

    info!(
        "Configuring greetd for user '{}' with session '{}'",
        config.user.name,
        get_session_command(&config.desktop.environment)
    );

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure /etc/greetd/config.toml");
        println!("    user: {}", config.user.name);
        println!(
            "    session: {}",
            get_session_command(&config.desktop.environment)
        );
        if config.system.init == InitSystem::S6 {
            println!("  [dry-run] Would write s6 service /etc/s6/sv/greetd-srv/");
        }
        return Ok(());
    }

    let username = &config.user.name;

    // Determine session command based on desktop environment
    let session_cmd = get_session_command(&config.desktop.environment);

    let config_content =
        if config.packages.install_session_switching && config.packages.install_gaming {
            // Session switching mode: greetd auto-logins the user into
            // deploytix-session-manager, which handles the gamescope ↔ desktop
            // loop internally via a sentinel file.
            format!(
                r#"[terminal]
vt = 1

[default_session]
command = "deploytix-session-manager"
user = "{user}"
"#,
                user = username,
            )
        } else {
            // Standard mode: desktop session as default
            format!(
                r#"[terminal]
vt = 1

[default_session]
command = "{session}"
user = "{user}"
"#,
                session = session_cmd,
                user = username,
            )
        };

    let greetd_dir = format!("{}/etc/greetd", install_root);
    fs::create_dir_all(&greetd_dir)?;
    fs::write(format!("{}/config.toml", greetd_dir), config_content)?;

    info!(
        "greetd config written to /etc/greetd/config.toml for user '{}'",
        username
    );

    // For S6 there is no official greetd-s6 package; write the service
    // directory ourselves so enable_s6_service() can find and enable it.
    if config.system.init == InitSystem::S6 {
        write_greetd_s6_service(install_root)?;
    }

    Ok(())
}

/// Write the greetd s6 service directory at `/etc/s6/sv/greetd-srv/`.
///
/// Artix s6 packages follow the `{name}-srv` convention for service
/// directories (e.g. `seatd-srv`, `NetworkManager-srv`).  There is no
/// official `greetd-s6` package, so we create the directory manually.
/// `enable_s6_service("greetd")` will then find it and touch the bundle
/// entry in `/etc/s6/adminsv/default/contents.d/greetd-srv`.
///
/// Structure created:
/// ```
/// /etc/s6/sv/greetd-srv/
///   type        — "longrun" (required by s6-rc)
///   run         — exec /usr/bin/greetd
///   log/
///     run       — svlogd → /var/log/greetd
/// ```
fn write_greetd_s6_service(install_root: &str) -> Result<()> {
    let sv_dir = format!("{}/etc/s6/sv/greetd-srv", install_root);
    let log_dir = format!("{}/log", sv_dir);

    fs::create_dir_all(&log_dir)?;

    // s6-rc requires a `type` file declaring the service class.
    fs::write(format!("{}/type", sv_dir), "longrun\n")?;

    // Main run script — redirect stderr into the s6 log pipeline, then
    // exec greetd (s6-supervise replaces this process, so no wrapper needed).
    let run = "#!/bin/sh\nexec 2>&1\nexec /usr/bin/greetd\n";
    let run_path = format!("{}/run", sv_dir);
    fs::write(&run_path, run)?;
    fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

    // Log run script — svlogd writes timestamped lines to /var/log/greetd.
    let log_run = "#!/bin/sh\n\
                   [ -d /var/log/greetd ] || install -dm 755 /var/log/greetd\n\
                   exec svlogd -tt /var/log/greetd\n";
    let log_run_path = format!("{}/run", log_dir);
    fs::write(&log_run_path, log_run)?;
    fs::set_permissions(&log_run_path, fs::Permissions::from_mode(0o755))?;

    info!("Written s6 service directory: /etc/s6/sv/greetd-srv/");
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
