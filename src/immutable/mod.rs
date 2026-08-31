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
//!
//! `/lib`, `/lib64`, `/bin`, `/sbin` are symlinks into `/usr`, so a read-only
//! `@usr` covers them for free.

pub mod boot;
pub mod bootset;
pub mod etc;
pub mod history;
pub mod lockdown;
pub mod lvm_ab;
pub mod rollback;
pub mod snapshot;
pub mod update;

/// The read-only OS root subvolume.
pub const ROOT_SUBVOL: &str = "@";
/// The read-only `/usr` subvolume.
pub const USR_SUBVOL: &str = "@usr";
/// The writable `/etc` subvolume (kept out of the read-only root).
pub const ETC_SUBVOL: &str = "@etc";
/// Where `@etc` is mounted — the probe for the root filesystem on a booted
/// immutable system, whose `/` is an overlay and names no block device.
pub const ETC_MOUNTPOINT: &str = "/etc";

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

/// Detect the immutable subvolume filesystems on the running system.
///
/// Resolved from the live mount table rather than assumed, because the
/// [`ROOT_FS_DEVICE`]/[`USR_FS_DEVICE`] mapper names only exist on an encrypted
/// layout whose names were not disambiguated. An unencrypted immutable install
/// has no `/dev/mapper/Crypt-*` at all, and `resolve_mapper_name()` can hand a
/// second deploytix system `Crypt-Root-1`; in both cases the constants name a
/// device that is missing or, worse, someone else's.
///
/// `/etc` is the probe rather than `/`: on a booted immutable system `/` is an
/// overlayfs whose source reads as `overlay`, while `@etc` is a plain subvolume
/// mount of the root btrfs. `/usr` answers for the usr filesystem, which is a
/// separate container only in multi-volume encrypted layouts.
pub fn detect_devices() -> ImmutableDevices {
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    detect_devices_from(&mounts)
}

/// [`detect_devices`] against a given mount table, so the resolution is testable.
fn detect_devices_from(mounts: &str) -> ImmutableDevices {
    let root_fs = mount_source(mounts, ETC_MOUNTPOINT)
        .or_else(|| mount_source(mounts, "/"))
        .unwrap_or_else(|| ROOT_FS_DEVICE.to_string());

    let usr_fs = mount_source(mounts, "/usr").unwrap_or_else(|| {
        // No separate /usr mount: it is a subvolume of the root filesystem —
        // unless the legacy container happens to be present.
        if Path::new(USR_FS_DEVICE).exists() {
            USR_FS_DEVICE.to_string()
        } else {
            root_fs.clone()
        }
    });

    ImmutableDevices { root_fs, usr_fs }
}

/// Backing device of `mount_point` in a `/proc/self/mounts` table.
///
/// The last entry wins, matching the kernel: a later mount over the same point
/// shadows the earlier one. Pseudo-filesystems (`overlay`, `tmpfs`) are skipped
/// — only a real block device can carry a btrfs subvolume.
fn mount_source(mounts: &str, mount_point: &str) -> Option<String> {
    mounts.lines().rev().find_map(|line| {
        let mut fields = line.split_whitespace();
        let source = fields.next()?;
        let target = fields.next()?;
        (target == mount_point && source.starts_with("/dev/")).then(|| source.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_resolve_from_the_live_mount_table() {
        // Encrypted multi-volume: /etc and /usr are separate containers, and /
        // is an overlay that names no device.
        let m = "overlay / overlay ro,lowerdir=/x 0 0\n\
                 /dev/mapper/Crypt-Usr /usr btrfs ro,subvol=/@usr 0 0\n\
                 /dev/mapper/Crypt-Root /etc btrfs rw,subvol=/@etc 0 0\n";
        let d = detect_devices_from(m);
        assert_eq!(d.root_fs, "/dev/mapper/Crypt-Root");
        assert_eq!(d.usr_fs, "/dev/mapper/Crypt-Usr");
        assert!(!d.usr_on_root());
    }

    #[test]
    fn unencrypted_immutable_resolves_a_real_partition() {
        // No /dev/mapper/Crypt-* exists at all here; the hardcoded constants
        // would have named a device that is simply not there.
        let m = "/dev/nvme0n1p3 / btrfs ro,subvol=/@ 0 0\n\
                 /dev/nvme0n1p3 /etc btrfs rw,subvol=/@etc 0 0\n\
                 /dev/nvme0n1p2 /boot ext4 rw 0 0\n";
        let d = detect_devices_from(m);
        assert_eq!(d.root_fs, "/dev/nvme0n1p3");
        // @usr shares the root filesystem when it is not its own container.
        assert_eq!(d.usr_fs, "/dev/nvme0n1p3");
        assert!(d.usr_on_root());
    }

    #[test]
    fn a_disambiguated_container_name_is_followed() {
        // resolve_mapper_name() hands a second deploytix system Crypt-Root-1;
        // the constant would have pointed at the *other* system's container.
        let m = "/dev/mapper/Crypt-Root-1 /etc btrfs rw,subvol=/@etc 0 0\n";
        assert_eq!(detect_devices_from(m).root_fs, "/dev/mapper/Crypt-Root-1");
    }

    #[test]
    fn a_later_mount_over_the_same_point_wins() {
        let m = "/dev/sda1 /etc btrfs rw 0 0\n/dev/sdb2 /etc btrfs rw 0 0\n";
        assert_eq!(detect_devices_from(m).root_fs, "/dev/sdb2");
    }

    #[test]
    fn an_unreadable_mount_table_falls_back_to_the_constants() {
        let d = detect_devices_from("");
        assert_eq!(d.root_fs, ROOT_FS_DEVICE);
    }
}
