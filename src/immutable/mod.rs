//! Transactional immutable root support.
//!
//! deploytix's immutable mode brings openSUSE MicroOS/Aeon-style semantics to
//! Artix: `/` and `/usr` are mounted read-only on every boot, `/etc` lives on a
//! dedicated writable `@etc` subvolume, and the three are snapshotted as an
//! atomic set (`{@, @usr, @etc}`) that rolls back together. Package updates are
//! performed transactionally by [`update`] against a fresh writable snapshot set
//! that only takes effect on reboot; direct `pacman -Syu` on the live system is
//! prevented by the read-only `/usr` mount (with a friendly interactive nudge
//! from [`lockdown`] toward `deploytix update`).
//!
//! This module owns the primitives shared across install, `deploytix update`
//! and `deploytix rollback`.
//!
//! ## Subvolume roles (root btrfs; `@usr` may live on a separate `Crypt-Usr`)
//! | Subvol | Mount | State | Snapshotted |
//! |--------|-------|-------|-------------|
//! | `@`    | `/`    | ro   | yes (paired) |
//! | `@usr` | `/usr` | ro   | yes (paired) |
//! | `@etc` | `/etc` | rw   | yes (paired) |
//! | `@var`, `@log`, `@home` | rw | no (persistent) |
//! | `@tmp` | `/tmp` | rw | no (disk scratch; boot-wiped) |
//!
//! `/lib`, `/lib64`, `/bin`, `/sbin` are symlinks into `/usr`, so a read-only
//! `@usr` covers them for free.

pub mod boot;
pub mod etc;
pub mod history;
pub mod lockdown;
pub mod lvm_ab;
pub mod remove;
pub mod rollback;
pub mod snapshot;
pub mod tmp;
pub mod update;

/// The read-only OS root subvolume.
pub const ROOT_SUBVOL: &str = "@";
/// The read-only `/usr` subvolume.
pub const USR_SUBVOL: &str = "@usr";
/// The writable `/etc` subvolume (kept out of the read-only root).
pub const ETC_SUBVOL: &str = "@etc";
/// Disk-backed `/tmp` subvolume on the root btrfs (not snapshotted; boot-wiped).
pub const TMP_SUBVOL: &str = "@tmp";

/// Pairing marker written inside each root subvolume/snapshot. It records the
/// `@usr` and `@etc` subvolume paths that belong with this root, so the
/// initramfs can mount the matching pair when booting any snapshot. Lives at the
/// root of `@` (readable even when the root is mounted read-only).
pub const PAIR_MARKER: &str = ".deploytix-pair";

/// Mount points the `mountcrypt` initramfs hook mounts itself, from the booted
/// root's `.deploytix-pair` marker, before `switch_root`.
///
/// These must **not** appear in `/etc/fstab`. fstab can only name the
/// install-time base subvolumes (`@`, `@usr`, `@etc`), and every snapshot set
/// inherits a copy of that file on its `@etc`; when the init system runs
/// `mount -a` while booted on a set, libmount compares btrfs *subvolumes* and
/// so does not consider `subvol=@usr` to be the already-mounted
/// `subvol=@deploytix-sets/<id>/usr` — it mounts the base subvolume on top,
/// silently shadowing the set. The system still boots (the base `@usr` is a
/// complete `/usr`), it just is not the one that was updated.
///
/// The LVM A/B backend has always omitted them for the same reason; see the
/// header comment in `generate_fstab_lvm_ab`.
pub const INITRAMFS_OWNED_MOUNTPOINTS: &[&str] = &["/", "/usr", "/etc"];

/// Directories created on the writable `@var` to back the read-only root's
/// writable paths, each paired with the mount point it is bind-mounted to.
///
/// `/` is mounted read-only and is deliberately *not* covered by an overlayfs —
/// that keeps it a real btrfs mount, which is what `grub-probe`, grub-btrfs's
/// generator and `findmnt -no FSROOT /` need in order to work against the
/// running system. The directories that live inside `/` and still have to be
/// written therefore get explicit homes on `@var`.
///
/// Bind mounts, not symlinks: the `filesystem` package owns all three as
/// directories, and replacing them with symlinks makes every update of that
/// package conflict.
pub const WRITABLE_BIND_PATHS: &[(&str, &str)] = &[
    ("/var/roothome", "/root"),
    ("/var/opt", "/opt"),
    ("/var/srv", "/srv"),
];

/// Create the `@var` directories that [`WRITABLE_BIND_PATHS`] binds from.
///
/// Called with the target's `/var` already mounted, before basestrap, so the
/// bind sources exist the first time fstab is processed. `/root` is created
/// 0700 like the directory it stands in for; the others take the default.
pub fn create_writable_path_sources(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("[immutable] Creating writable-path sources on @var");
    if cmd.is_dry_run() {
        for (source, target) in WRITABLE_BIND_PATHS {
            println!("  [dry-run] Would create {source} (bind source for {target})");
        }
        return Ok(());
    }
    for (source, _) in WRITABLE_BIND_PATHS {
        let path = format!("{install_root}{source}");
        std::fs::create_dir_all(&path)?;
        if *source == "/var/roothome" {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// Bind [`WRITABLE_BIND_PATHS`] into the install root, the way the booted
/// system will have them.
///
/// The installer must reproduce the booted mount topology, not just the
/// filesystems. On a booted immutable system `/opt` is a bind mount of
/// `/var/opt`; inside the install chroot it is an ordinary directory in `@`.
/// A package installing to `/opt` during the install therefore writes into the
/// root subvolume, and on the next boot the bind mount covers it with the empty
/// `/var/opt`. The files are installed, paid for, and unreachable — which is
/// what an empty `/opt` and a launcher in `/usr/bin` pointing into it look like.
///
/// [`crate::immutable::update::mount_set_cmd`] already does this for the update
/// chroot for exactly the same reason.
pub fn mount_writable_path_binds(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("[immutable] Binding writable paths into the install root");
    if cmd.is_dry_run() {
        for (source, target) in WRITABLE_BIND_PATHS {
            println!("  [dry-run] Would bind {install_root}{source} -> {install_root}{target}");
        }
        return Ok(());
    }
    for (source, target) in WRITABLE_BIND_PATHS {
        let src = format!("{install_root}{source}");
        let dest = format!("{install_root}{target}");
        std::fs::create_dir_all(&src)?;
        std::fs::create_dir_all(&dest)?;
        cmd.run("mount", &["--bind", &src, &dest])?;
        info!("  Bound {} -> {}", src, dest);
    }
    Ok(())
}

/// Whether the initramfs mounts `mount_point` itself, making an fstab entry for
/// it wrong. See [`INITRAMFS_OWNED_MOUNTPOINTS`].
pub fn initramfs_owned_mount(mount_point: &str) -> bool {
    INITRAMFS_OWNED_MOUNTPOINTS.contains(&mount_point)
}

use crate::immutable::snapshot::ImmutableDevices;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tracing::info;

/// btrfs holding `@`, `@etc` and the snapshot sets on a deploytix system.
pub const ROOT_FS_DEVICE: &str = "/dev/mapper/Crypt-Root";
/// btrfs holding `@usr` in multi-volume encrypted layouts.
pub const USR_FS_DEVICE: &str = "/dev/mapper/Crypt-Usr";

/// Write the *live* pairing marker (`usr=@usr`, `etc=@etc`) into the root
/// mounted at `root`. Called at install/migration time so the default `@` boot
/// mounts the live `@usr`/`@etc`; snapshot sets get their own marker from
/// [`snapshot::write_pair_marker_cmd`].
pub fn write_live_pair_marker(cmd: &CommandRunner, root: &str) -> Result<()> {
    let path = format!("{root}/{PAIR_MARKER}");
    if cmd.is_dry_run() {
        println!("  [dry-run] Would write live pairing marker {path}");
        return Ok(());
    }
    std::fs::write(&path, "usr=@usr\netc=@etc\n")?;
    Ok(())
}

/// Detect the immutable subvolume filesystems on the running/installed system.
///
/// `@usr` lives on its own `Crypt-Usr` container in multi-volume layouts and on
/// the root filesystem otherwise; we pick based on which mapper device exists.
pub fn detect_devices() -> ImmutableDevices {
    let usr_fs = if Path::new(USR_FS_DEVICE).exists() {
        USR_FS_DEVICE.to_string()
    } else {
        ROOT_FS_DEVICE.to_string()
    };
    ImmutableDevices {
        root_fs: ROOT_FS_DEVICE.to_string(),
        usr_fs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The installer must reproduce the booted mount topology, not just the
    /// filesystems. On a booted immutable system `/opt` is a bind mount of
    /// `/var/opt`; inside the install chroot it is a plain directory in `@`. A
    /// package installing to `/opt` during the install therefore writes into
    /// `@`, and the boot-time bind mount then covers it with the empty
    /// `/var/opt` — installed, and unreachable. warp-terminal and
    /// zen-browser-bin both install to `/opt` and both failed exactly that way.
    #[test]
    fn writable_paths_are_bound_during_the_install() {
        let cmd = CommandRunner::new(true);
        assert!(mount_writable_path_binds(&cmd, "/mnt").is_ok());

        let targets: Vec<&str> = WRITABLE_BIND_PATHS.iter().map(|(_, t)| *t).collect();
        assert!(targets.contains(&"/opt"), "/opt is the one that bites");
        assert!(targets.contains(&"/root"));
        assert!(targets.contains(&"/srv"));
    }

    /// Every source has to be under /var, or it would not persist across
    /// snapshot sets with the rest of the writable state.
    #[test]
    fn writable_bind_sources_live_on_var() {
        for (source, target) in WRITABLE_BIND_PATHS {
            assert!(source.starts_with("/var/"), "{source} is not on @var");
            assert!(
                !target.starts_with("/var/"),
                "{target} should be the mount point"
            );
        }
    }
}
