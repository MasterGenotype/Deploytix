//! Locale and timezone configuration

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
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

/// Charset field for a locale.gen entry.
///
/// `en_US.UTF-8` → `UTF-8`; bare names without a dot default to UTF-8.
fn locale_charset(locale: &str) -> &str {
    locale
        .rsplit_once('.')
        .map(|(_, cs)| cs)
        .filter(|cs| !cs.is_empty())
        .unwrap_or("UTF-8")
}

/// True when an uncommented locale.gen body selects `locale`.
///
/// Bodies look like `en_US.UTF-8 UTF-8` (optional trailing whitespace already
/// stripped by the caller). Match on the locale name only so a commented
/// `#en_US.UTF-8 UTF-8` is treated as the line to enable, not as "already on".
fn locale_gen_line_matches(uncommented_body: &str, locale: &str) -> bool {
    uncommented_body
        .split_whitespace()
        .next()
        .is_some_and(|name| name == locale)
}

/// Enable `locale` inside a locale.gen file's text.
///
/// The Artix/Arch stock file ships every locale commented (`#en_US.UTF-8 UTF-8`).
/// A naive `contains("en_US.UTF-8 UTF-8")` matches that commented line and skips
/// enabling it, so `locale-gen` writes nothing under `/usr/lib/locale`. On an
/// immutable root that archive cannot be rebuilt later (`/usr` is RO), so the
/// enable step must uncomment (or append) at install time, then run locale-gen
/// while the tree is still writable.
fn enable_locale_in_gen(content: &str, locale: &str) -> String {
    let charset = locale_charset(locale);
    let entry = format!("{} {}", locale, charset);
    let mut found = false;

    let mut lines: Vec<String> = content
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            let body = match trimmed.strip_prefix('#') {
                Some(rest) => rest.trim(),
                None => trimmed,
            };
            if locale_gen_line_matches(body, locale) {
                found = true;
                entry.clone()
            } else {
                line.to_string()
            }
        })
        .collect();

    if !found {
        lines.push(entry);
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Configure locale
fn set_locale(cmd: &CommandRunner, locale: &str, install_root: &str) -> Result<()> {
    info!("Setting locale to {}", locale);

    let locale_gen_path = format!("{}/etc/locale.gen", install_root);
    let locale_conf_path = format!("{}/etc/locale.conf", install_root);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would uncomment {} in locale.gen, write locale.conf, run locale-gen",
            locale
        );
        return Ok(());
    }

    // 1. Uncomment (or append) the chosen locale in locale.gen
    let existing = fs::read_to_string(&locale_gen_path).unwrap_or_default();
    let updated = enable_locale_in_gen(&existing, locale);
    if let Some(parent) = std::path::Path::new(&locale_gen_path).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&locale_gen_path, updated)?;

    // 2. Create locale.conf (LANG is enough; glibc picks up the rest)
    let locale_conf_content = format!("LANG={}\n", locale);
    fs::write(&locale_conf_path, locale_conf_content)?;

    // 3. Generate locales into /usr/lib/locale while the install root is RW
    //    (immutable installs mount /usr read-only after first boot).
    cmd.run_in_chroot(install_root, "/bin/locale-gen")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charset_from_locale_name() {
        assert_eq!(locale_charset("en_US.UTF-8"), "UTF-8");
        assert_eq!(locale_charset("C.UTF-8"), "UTF-8");
        assert_eq!(locale_charset("en_US"), "UTF-8");
    }

    #[test]
    fn uncomment_stock_commented_en_us() {
        // Stock glibc locale.gen ships the line commented, often with trailing
        // spaces. contains("en_US.UTF-8 UTF-8") would wrongly treat this as on.
        let stock = "\
#aa_DJ.UTF-8 UTF-8  \n\
#en_US.UTF-8 UTF-8  \n\
#en_US ISO-8859-1  \n\
#fr_FR.UTF-8 UTF-8  \n";
        let out = enable_locale_in_gen(stock, "en_US.UTF-8");
        assert!(
            out.lines().any(|l| l.trim() == "en_US.UTF-8 UTF-8"),
            "en_US.UTF-8 must be uncommented:\n{out}"
        );
        assert!(
            !out.lines().any(|l| {
                let t = l.trim();
                t.starts_with('#') && t.contains("en_US.UTF-8")
            }),
            "commented en_US.UTF-8 must not remain:\n{out}"
        );
        // Other locales stay commented; the ISO-8859-1 en_US variant is a
        // different locale name and must stay alone.
        assert!(out.contains("#fr_FR.UTF-8 UTF-8"));
        assert!(out.contains("#en_US ISO-8859-1"));
        // No duplicate append
        assert_eq!(
            out.lines()
                .filter(|l| l.split_whitespace().next() == Some("en_US.UTF-8"))
                .count(),
            1
        );
    }

    #[test]
    fn already_enabled_is_idempotent() {
        let on = "en_US.UTF-8 UTF-8\n#fr_FR.UTF-8 UTF-8\n";
        let out = enable_locale_in_gen(on, "en_US.UTF-8");
        assert_eq!(
            out.lines()
                .filter(|l| l.split_whitespace().next() == Some("en_US.UTF-8"))
                .count(),
            1
        );
        assert!(out.contains("en_US.UTF-8 UTF-8"));
    }

    #[test]
    fn missing_locale_is_appended() {
        let sparse = "#fr_FR.UTF-8 UTF-8\n";
        let out = enable_locale_in_gen(sparse, "en_US.UTF-8");
        assert!(out.contains("#fr_FR.UTF-8 UTF-8"));
        assert!(
            out.lines().any(|l| l.trim() == "en_US.UTF-8 UTF-8"),
            "missing locale must be appended:\n{out}"
        );
    }

    #[test]
    fn empty_file_gets_entry() {
        let out = enable_locale_in_gen("", "en_US.UTF-8");
        assert_eq!(out, "en_US.UTF-8 UTF-8\n");
    }

    #[test]
    fn set_locale_writes_gen_and_conf_before_locale_gen() {
        // Exercise the file side without requiring a real chroot locale-gen:
        // dry-run skips I/O, so drive enable + conf write the same way set_locale does.
        let mut root = std::env::temp_dir();
        root.push(format!("deploytix_locale_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("etc")).unwrap();

        let gen_path = root.join("etc/locale.gen");
        fs::write(&gen_path, "#en_US.UTF-8 UTF-8  \n#de_DE.UTF-8 UTF-8\n").unwrap();

        let updated = enable_locale_in_gen(&fs::read_to_string(&gen_path).unwrap(), "en_US.UTF-8");
        fs::write(&gen_path, updated).unwrap();
        fs::write(root.join("etc/locale.conf"), "LANG=en_US.UTF-8\n").unwrap();

        let gen = fs::read_to_string(&gen_path).unwrap();
        assert!(gen.lines().any(|l| l.trim() == "en_US.UTF-8 UTF-8"));
        assert!(!gen.contains("#en_US.UTF-8"));
        let conf = fs::read_to_string(root.join("etc/locale.conf")).unwrap();
        assert_eq!(conf, "LANG=en_US.UTF-8\n");

        let _ = fs::remove_dir_all(&root);
    }
}

/// Set keyboard layout
fn set_keymap(cmd: &CommandRunner, keymap: &str, install_root: &str) -> Result<()> {
    info!("Setting keymap to {}", keymap);

    let vconsole_path = format!("{}/etc/vconsole.conf", install_root);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would set keymap {} in {}",
            keymap, vconsole_path
        );
        return Ok(());
    }

    let content = format!("KEYMAP={}\n", keymap);
    fs::write(&vconsole_path, content)?;

    Ok(())
}

/// Create a dinit service that loads the console keymap from
/// `/etc/vconsole.conf` at boot.
///
/// Unlike runit/openrc/s6, dinit does not ship a built-in service
/// for keymap loading, so we provide one.
pub fn create_dinit_keymap_service(install_root: &str, keymap: &str) -> Result<()> {
    info!("Creating dinit keymap service for '{}'", keymap);

    // Script that loads the keymap
    let script_dir = format!("{}/usr/local/bin", install_root);
    fs::create_dir_all(&script_dir)?;

    let script = format!("#!/bin/sh\nloadkeys {}\n", keymap);
    let script_path = format!("{}/loadkeys-boot", script_dir);
    fs::write(&script_path, script)?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    // Dinit service file
    let dinit_dir = format!("{}/etc/dinit.d", install_root);
    fs::create_dir_all(&dinit_dir)?;

    let service = "type = scripted\ncommand = /usr/local/bin/loadkeys-boot\n";
    let service_path = format!("{}/loadkeys", dinit_dir);
    fs::write(&service_path, service)?;

    // Enable the service
    let boot_d = format!("{}/etc/dinit.d/boot.d", install_root);
    fs::create_dir_all(&boot_d)?;
    std::os::unix::fs::symlink("/etc/dinit.d/loadkeys", format!("{}/loadkeys", boot_d))?;

    info!("Created and enabled dinit loadkeys service");
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
