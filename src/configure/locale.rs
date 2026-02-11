//! Locale and timezone configuration

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::io::Write;
use tracing::info;

/// Configure locale, timezone, and keymap
pub fn configure_locale(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    info!("Configuring locale, timezone, keymap, and hostname");

    // Set timezone
    set_timezone(cmd, &config.system.timezone, install_root)?;

    // Configure locale
    set_locale(cmd, &config.system.locale, install_root)?;

    // Set keymap
    set_keymap(cmd, &config.system.keymap, install_root)?;

    // Set hostname
    set_hostname(cmd, &config.system.hostname, install_root)?;

    Ok(())
}

/// Set system timezone
fn set_timezone(cmd: &CommandRunner, timezone: &str, install_root: &str) -> Result<()> {
    info!("Setting timezone to {}", timezone);

    let zoneinfo_path = format!("/usr/share/zoneinfo/{}", timezone);
    let localtime_path = format!("{}/etc/localtime", install_root);

    if cmd.is_dry_run() {
        println!("  [dry-run] ln -sf {} {}", zoneinfo_path, localtime_path);
        return Ok(());
    }

    // Remove existing localtime if it exists
    let _ = fs::remove_file(&localtime_path);

    // Create symlink
    std::os::unix::fs::symlink(&zoneinfo_path, &localtime_path)?;

    // Set hardware clock
    cmd.run_in_chroot(install_root, "hwclock --systohc")?;

    Ok(())
}

/// Configure locale
fn set_locale(cmd: &CommandRunner, locale: &str, install_root: &str) -> Result<()> {
    info!("Setting locale to {}", locale);

    let locale_gen_path = format!("{}/etc/locale.gen", install_root);
    let locale_conf_path = format!("{}/etc/locale.conf", install_root);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure locale {} in {}", locale, install_root);
        return Ok(());
    }

    // Enable locale in locale.gen
    let locale_entry = format!("{} UTF-8", locale);
    let locale_gen_content = fs::read_to_string(&locale_gen_path).unwrap_or_default();

    if !locale_gen_content.contains(&locale_entry) {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&locale_gen_path)?;
        writeln!(file, "{}", locale_entry)?;
    }

    // Create locale.conf
    let locale_conf_content = format!("LANG={}\n", locale);
    fs::write(&locale_conf_path, locale_conf_content)?;

    // Generate locales
    cmd.run_in_chroot(install_root, "locale-gen")?;

    Ok(())
}

/// Set keyboard layout
fn set_keymap(cmd: &CommandRunner, keymap: &str, install_root: &str) -> Result<()> {
    info!("Setting keymap to {}", keymap);

    let vconsole_path = format!("{}/etc/vconsole.conf", install_root);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would set keymap {} in {}", keymap, vconsole_path);
        return Ok(());
    }

    let content = format!("KEYMAP={}\n", keymap);
    fs::write(&vconsole_path, content)?;

    Ok(())
}

/// Set hostname
fn set_hostname(cmd: &CommandRunner, hostname: &str, install_root: &str) -> Result<()> {
    info!("Setting hostname to {}", hostname);

    let hostname_path = format!("{}/etc/hostname", install_root);
    let hosts_path = format!("{}/etc/hosts", install_root);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would set hostname to {}", hostname);
        return Ok(());
    }

    // Write hostname
    fs::write(&hostname_path, format!("{}\n", hostname))?;

    // Update hosts file
    let hosts_content = format!(
        "127.0.0.1\tlocalhost\n::1\t\tlocalhost\n127.0.1.1\t{}.localdomain\t{}\n",
        hostname, hostname
    );
    fs::write(&hosts_path, hosts_content)?;

    Ok(())
}
