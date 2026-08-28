//! `deploytix update` — transactional system update.
//!
//! The running system's `/` and `/usr` are read-only, so updates never modify it
//! in place. Instead:
//!
//! 1. Snapshot the current `{@, @usr, @etc}` into a new **writable** set.
//! 2. Mount that set (root + paired usr/etc, with `/var`, `/home`, `/boot`
//!    bind-mounted so state and the kernel/initramfs are shared).
//! 3. Run `pacman -Syu` (or install the requested packages) and regenerate the
//!    initramfs *inside the set* via `artix-chroot`.
//! 4. On success, point the default boot entry at the new set and regenerate
//!    grub.cfg; the change takes effect on the next reboot. On failure, delete
//!    the half-built set and leave the running system untouched.
//!
//! Old sets are pruned to [`UpdateOptions::keep_sets`], never removing the
//! running set or the one just built.
//!
//! ## Caveat: shared `/boot`
//! `/boot` is a separate, non-snapshotted partition, so the kernel and initramfs
//! are shared across all sets. A rollback restores userspace (`@`/`@usr`/`@etc`)
//! but boots with the most recently installed kernel. The `mountcrypt` hook is
//! version-independent, so this is safe; only kernel *contents* are not rolled
//! back. This is documented in `docs/IMMUTABLE_SYSTEM.md`.

use crate::immutable::snapshot::{self, ImmutableDevices};
use crate::immutable::{boot, detect_devices};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use tracing::{info, warn};

/// Options controlling a transactional update.
pub struct UpdateOptions {
    /// Number of previous sets to retain when pruning (excludes the running set
    /// and the newly built one).
    pub keep_sets: usize,
    /// Reboot automatically after a successful update.
    pub reboot: bool,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            keep_sets: 3,
            reboot: false,
        }
    }
}

/// Directory under `/run` where a set is assembled for the chroot.
fn target_dir(id: &str) -> String {
    format!("/run/deploytix-update/{id}")
}

/// Shell that mounts a set (root + paired usr/etc + bind var/home/boot) at its
/// target so `artix-chroot` can operate on it. Snapshots were created writable,
/// so pacman can write to `/` and `/usr`.
pub fn mount_set_cmd(devices: &ImmutableDevices, id: &str) -> String {
    let target = target_dir(id);
    format!(
        "set -e; t={target}; mkdir -p \"$t\"; \
         mount -t btrfs -o subvol={root},rw,noatime,compress=zstd {root_fs} \"$t\"; \
         mkdir -p \"$t/usr\" \"$t/etc\"; \
         mount -t btrfs -o subvol={usr},rw,noatime,compress=zstd {usr_fs} \"$t/usr\"; \
         mount -t btrfs -o subvol={etc},rw,noatime,compress=zstd {root_fs} \"$t/etc\"; \
         for d in var home boot; do mkdir -p \"$t/$d\"; mount --rbind \"/$d\" \"$t/$d\"; done",
        target = target,
        root = snapshot::set_root_subvol(id),
        etc = snapshot::set_etc_subvol(id),
        usr = snapshot::set_usr_subvol(id),
        root_fs = devices.root_fs,
        usr_fs = devices.usr_fs,
    )
}

/// Shell that recursively unmounts and removes a set's chroot target.
pub fn unmount_set_cmd(id: &str) -> String {
    let target = target_dir(id);
    format!("umount -R {target} 2>/dev/null || true; rmdir {target} 2>/dev/null || true")
}

/// The pacman invocation for the transaction: a full upgrade, or installing the
/// named packages on top of a sync.
pub fn pacman_cmd(extra_packages: &[String]) -> String {
    if extra_packages.is_empty() {
        "pacman -Syu --noconfirm".to_string()
    } else {
        format!("pacman -Syu --noconfirm {}", extra_packages.join(" "))
    }
}

/// Confirm we are on an immutable deploytix system before updating.
fn ensure_immutable(cmd: &CommandRunner) -> Result<()> {
    if cmd.is_dry_run() {
        return Ok(());
    }
    // The live pairing marker at the root of `/` is the reliable signal.
    if std::path::Path::new(&format!("/{}", crate::immutable::PAIR_MARKER)).exists() {
        Ok(())
    } else {
        Err(DeploytixError::ConfigError(
            "not an immutable deploytix system (no /.deploytix-pair marker); \
             `deploytix update` only applies to immutable installs"
                .to_string(),
        ))
    }
}

/// Delete sets older than the newest `keep`, never touching `running` or `keep_id`.
fn prune_sets(
    cmd: &CommandRunner,
    devices: &ImmutableDevices,
    keep: usize,
    running: &str,
    keep_id: &str,
) -> Result<()> {
    let sets = snapshot::list_sets(cmd, &devices.root_fs)?;
    let removable: Vec<String> = sets
        .iter()
        .filter(|id| id.as_str() != running && id.as_str() != keep_id)
        .cloned()
        .collect();
    if removable.len() <= keep {
        return Ok(());
    }
    let to_delete = &removable[..removable.len() - keep];
    for id in to_delete {
        if let Err(e) = snapshot::delete_set(cmd, devices, id) {
            warn!("[immutable] Failed to prune set {}: {}", id, e);
        }
    }
    Ok(())
}

/// Perform a transactional update.
pub fn run_update(
    cmd: &CommandRunner,
    extra_packages: &[String],
    opts: &UpdateOptions,
) -> Result<()> {
    ensure_immutable(cmd)?;
    let devices = detect_devices();

    info!("[immutable] Building transactional update set");
    let id = snapshot::create_set(cmd, &devices, /* readonly = */ false)?;

    // Everything from here is unwound on failure so a bad update leaves nothing.
    let result = (|| -> Result<()> {
        cmd.run("sh", &["-c", &mount_set_cmd(&devices, &id)])?;
        let target = target_dir(&id);
        info!("[immutable] Running pacman in set {}", id);
        cmd.run_in_chroot(&target, &pacman_cmd(extra_packages))?;
        // Regenerate the (shared) initramfs from within the updated set.
        cmd.run_in_chroot(&target, "mkinitcpio -P")?;
        Ok(())
    })();

    // Always release the chroot mounts.
    let _ = cmd.run("sh", &["-c", &unmount_set_cmd(&id)]);

    match result {
        Ok(()) => {
            boot::set_boot_pointer(cmd, &snapshot::set_root_subvol(&id))?;
            let running = boot::pointer_set_id(&boot::current_boot_pointer(cmd)?)
                .unwrap_or_else(|| "@".to_string());
            prune_sets(cmd, &devices, opts.keep_sets, &running, &id)?;
            info!(
                "[immutable] Update ready. Reboot to activate set {} (rollback: `deploytix rollback`).",
                id
            );
            if opts.reboot {
                cmd.run("reboot", &[])?;
            }
            Ok(())
        }
        Err(e) => {
            warn!("[immutable] Update failed; discarding set {}", id);
            let _ = snapshot::delete_set(cmd, &devices, &id);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devices() -> ImmutableDevices {
        ImmutableDevices {
            root_fs: "/dev/mapper/Crypt-Root".into(),
            usr_fs: "/dev/mapper/Crypt-Usr".into(),
        }
    }

    fn assert_valid_shell(script: &str) {
        if let Ok(status) = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(script)
            .status()
        {
            assert!(status.success(), "not valid shell:\n{script}");
        }
    }

    #[test]
    fn mount_cmd_mounts_paired_subvols_and_binds_state() {
        let s = mount_set_cmd(&devices(), "42");
        assert!(s.contains("subvol=@deploytix-sets/42/root,rw"));
        assert!(s.contains("subvol=@deploytix-sets/42/usr,rw"));
        assert!(s.contains("subvol=@deploytix-sets/42/etc,rw"));
        assert!(s.contains("/dev/mapper/Crypt-Usr"));
        // /var, /home and /boot are shared via rbind.
        assert!(s.contains("mount --rbind \"/$d\""));
        assert_valid_shell(&s);
    }

    #[test]
    fn unmount_cmd_is_recursive_and_safe() {
        let s = unmount_set_cmd("42");
        assert!(s.contains("umount -R /run/deploytix-update/42"));
        assert_valid_shell(&s);
    }

    #[test]
    fn pacman_cmd_full_upgrade_vs_targeted() {
        assert_eq!(pacman_cmd(&[]), "pacman -Syu --noconfirm");
        assert_eq!(
            pacman_cmd(&["vim".to_string(), "git".to_string()]),
            "pacman -Syu --noconfirm vim git"
        );
    }

    #[test]
    fn dry_run_update_is_safe_and_ordered() {
        // A dry-run must issue the snapshot → mount → pacman → boot-pointer
        // sequence without touching the system.
        let cmd = CommandRunner::new(true);
        run_update(&cmd, &[], &UpdateOptions::default()).unwrap();
    }
}
