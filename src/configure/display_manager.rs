//! Display manager dispatch — configures whichever DM the config selects.
//!
//! `greetd` (the default) keeps the original deploytix behavior: auto-login
//! straight into the desktop session, handled by `configure::greetd`. The
//! conventional display managers (SDDM, GDM, LightDM) present their normal
//! login screen; their packages and init services are installed/enabled
//! through the generic service machinery in `configure::services`, so this
//! module only writes DM-specific configuration files. `None` leaves the
//! system on a TTY login (each desktop module writes ~/.xinitrc for startx).

use crate::config::{DeploymentConfig, DesktopEnvironment, DisplayManager};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// Configure the selected display manager (if a desktop environment is set).
pub fn configure_display_manager(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if config.desktop.environment == DesktopEnvironment::None {
        info!("Skipping display manager configuration (no desktop environment selected)");
        return Ok(());
    }

    match config.desktop.display_manager {
        DisplayManager::Greetd => {
            crate::configure::greetd::configure_greetd(cmd, config, install_root)
        }
        DisplayManager::Sddm => configure_sddm(cmd, config, install_root),
        DisplayManager::Gdm => {
            // GDM works out of the box; package + service enablement is
            // handled by configure::services.
            info!("GDM selected; no extra configuration required");
            Ok(())
        }
        DisplayManager::Lightdm => {
            // lightdm-gtk-greeter is the compiled-in default greeter on
            // Artix, so the stock /etc/lightdm/lightdm.conf works as-is.
            info!("LightDM selected; no extra configuration required");
            Ok(())
        }
        DisplayManager::None => {
            info!(
                "No display manager selected; system boots to TTY login \
                 (~/.xinitrc is set up for startx)"
            );
            Ok(())
        }
    }
}

/// Configure SDDM: sane UID range, plus the Breeze theme when KDE is the
/// selected desktop (the theme ships with plasma-workspace).
fn configure_sddm(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    info!("Configuring SDDM");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would write /etc/sddm.conf.d/deploytix.conf");
        return Ok(());
    }

    let sddm_conf_dir = format!("{}/etc/sddm.conf.d", install_root);
    fs::create_dir_all(&sddm_conf_dir)?;

    let mut sddm_conf = String::new();
    if config.desktop.environment == DesktopEnvironment::Kde {
        sddm_conf.push_str("[Theme]\nCurrent=breeze\n\n");
    }
    sddm_conf.push_str("[Users]\nMaximumUid=60000\nMinimumUid=1000\n");

    fs::write(format!("{}/deploytix.conf", sddm_conf_dir), sddm_conf)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> String {
        let dir =
            std::env::temp_dir().join(format!("deploytix-dm-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn config(dm: DisplayManager) -> DeploymentConfig {
        // sample(): KDE desktop, runit init, user "user"
        let mut cfg = DeploymentConfig::sample();
        cfg.desktop.display_manager = dm;
        cfg
    }

    #[test]
    fn greetd_writes_autologin_config() {
        let root = test_root("greetd");
        let cmd = CommandRunner::new(false);
        configure_display_manager(&cmd, &config(DisplayManager::Greetd), &root).unwrap();

        let conf = fs::read_to_string(format!("{}/etc/greetd/config.toml", root)).unwrap();
        assert!(conf.contains("user = \"user\""));
        assert!(conf.contains("startplasma-wayland"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sddm_writes_conf_with_breeze_theme_for_kde() {
        let root = test_root("sddm");
        let cmd = CommandRunner::new(false);
        configure_display_manager(&cmd, &config(DisplayManager::Sddm), &root).unwrap();

        // No greetd config in sddm mode
        assert!(!std::path::Path::new(&format!("{}/etc/greetd/config.toml", root)).exists());
        let conf = fs::read_to_string(format!("{}/etc/sddm.conf.d/deploytix.conf", root)).unwrap();
        assert!(conf.contains("Current=breeze"));
        assert!(conf.contains("MinimumUid=1000"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sddm_omits_breeze_theme_for_non_kde() {
        let root = test_root("sddm-xfce");
        let cmd = CommandRunner::new(false);
        let mut cfg = config(DisplayManager::Sddm);
        cfg.desktop.environment = DesktopEnvironment::Xfce;
        configure_display_manager(&cmd, &cfg, &root).unwrap();

        let conf = fs::read_to_string(format!("{}/etc/sddm.conf.d/deploytix.conf", root)).unwrap();
        assert!(!conf.contains("breeze"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn gdm_lightdm_and_none_write_no_config() {
        for dm in [
            DisplayManager::Gdm,
            DisplayManager::Lightdm,
            DisplayManager::None,
        ] {
            let root = test_root("noop");
            let cmd = CommandRunner::new(false);
            configure_display_manager(&cmd, &config(dm), &root).unwrap();
            // Nothing should have been written under the install root
            assert!(fs::read_dir(&root).unwrap().next().is_none());
            let _ = fs::remove_dir_all(&root);
        }
    }
}
