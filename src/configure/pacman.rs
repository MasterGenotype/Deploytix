//! Configure Arch Linux repository access on the target system.
//!
//! After `artix-archlinux-support` is installed via basestrap, the target
//! system has `/etc/pacman.d/mirrorlist-arch` and the `archlinux-keyring`
//! package, but the Arch repos are not yet enabled in `/etc/pacman.conf`.
//! This module appends the `[extra]` (and optionally `[multilib]`)
//! sections so the installed system can install packages from Arch repos
//! out of the box.

use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// Path to the Arch mirrorlist provided by `artix-archlinux-support`.
const MIRRORLIST_ARCH: &str = "/etc/pacman.d/mirrorlist-arch";

/// Append Arch Linux `[extra]` and `[multilib]` repositories to the
/// target system's `/etc/pacman.conf` and populate the Arch keyring.
pub fn configure_arch_repos(
    cmd: &CommandRunner,
    install_root: &str,
) -> Result<()> {
    let conf_path = format!("{}/etc/pacman.conf", install_root);
    let content = fs::read_to_string(&conf_path).unwrap_or_default();

    if content.lines().any(|l| l.trim() == "[extra]") {
        info!("Arch [extra] repo already present in target pacman.conf");
    } else {
        info!("Appending Arch [extra] and [multilib] repos to target pacman.conf");

        if cmd.is_dry_run() {
            println!("  [dry-run] Would append [extra] and [multilib] to {}", conf_path);
            println!("  [dry-run] Would run pacman-key --populate archlinux in chroot");
            return Ok(());
        }

        let mirrorlist_available = std::path::Path::new(&format!(
            "{}{}",
            install_root, MIRRORLIST_ARCH
        ))
        .exists();

        let mirror_entry = if mirrorlist_available {
            format!("Include = {}", MIRRORLIST_ARCH)
        } else {
            "Server = https://geo.mirror.pkgbuild.com/$repo/os/$arch".to_string()
        };

        let repos = format!(
            "\n\n\
             # Arch Linux repositories (added by deploytix installer)\n\
             [extra]\n\
             SigLevel = PackageRequired\n\
             {mirror}\n\
             \n\
             [multilib]\n\
             SigLevel = PackageRequired\n\
             {mirror}\n",
            mirror = mirror_entry,
        );

        let updated = format!("{}{}", content.trim_end(), repos);
        fs::write(&conf_path, updated)?;

        info!("Arch repos appended to target pacman.conf");
    }

    if !cmd.is_dry_run() {
        // Populate the Arch keyring so package signature verification works.
        let _ = cmd.run_in_chroot(install_root, "pacman-key --populate archlinux");
        info!("Arch Linux keyring populated on target");
    }

    Ok(())
}
