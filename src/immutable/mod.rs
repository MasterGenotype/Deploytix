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
//! This module owns the primitives shared across install, `deploytix update`,
//! `deploytix rollback` and `deploytix migrate-immutable`.
//!
//! ## Subvolume roles (root btrfs; `@usr` may live on a separate `Crypt-Usr`)
//! | Subvol | Mount | State | Snapshotted |
//! |--------|-------|-------|-------------|
//! | `@`    | `/`    | ro   | yes (paired) |
//! | `@usr` | `/usr` | ro   | yes (paired) |
//! | `@etc` | `/etc` | rw   | yes (paired) |
//! | `@var`, `@log`, `@home` | rw | no (persistent) |
//!
//! `/lib`, `/lib64`, `/bin`, `/sbin` are symlinks into `/usr`, so a read-only
//! `@usr` covers them for free.

pub mod boot;
pub mod etc;
pub mod lockdown;
pub mod rollback;
pub mod snapshot;
pub mod update;

/// The read-only OS root subvolume.
pub const ROOT_SUBVOL: &str = "@";
/// The read-only `/usr` subvolume.
pub const USR_SUBVOL: &str = "@usr";
/// The writable `/etc` subvolume (kept out of the read-only root).
pub const ETC_SUBVOL: &str = "@etc";

/// Pairing marker written inside each root subvolume/snapshot. It records the
/// `@usr` and `@etc` subvolume paths that belong with this root, so the
/// initramfs can mount the matching pair when booting any snapshot. Lives at the
/// root of `@` (readable even when the root is mounted read-only).
pub const PAIR_MARKER: &str = ".deploytix-pair";

/// Mount points that deploytix mounts read-only under the immutable model.
pub const READONLY_MOUNTPOINTS: &[&str] = &["/", "/usr"];

/// Whether `mount_point` is mounted read-only under the immutable model.
pub fn is_readonly_mount(mount_point: &str) -> bool {
    READONLY_MOUNTPOINTS.contains(&mount_point)
}

use crate::immutable::snapshot::ImmutableDevices;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::path::Path;

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
