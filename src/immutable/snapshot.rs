//! Paired snapshot sets for the transactional immutable model.
//!
//! A *snapshot set* captures the three OS subvolumes — `@` (root), `@usr` and
//! `@etc` — under a shared id so they roll back together. Sets live in a
//! top-level [`SETS_DIR`] directory on each btrfs filesystem:
//!
//! ```text
//! root fs (Crypt-Root):  @deploytix-sets/<id>/root   (snapshot of @)
//!                        @deploytix-sets/<id>/etc    (snapshot of @etc)
//! usr  fs (Crypt-Usr):   @deploytix-sets/<id>/usr    (snapshot of @usr)
//! ```
//!
//! (In single-partition layouts the usr fs *is* the root fs, so all three land
//! together.) Each set's root snapshot carries a [`crate::immutable::PAIR_MARKER`]
//! file naming its `usr`/`etc` subvolumes, so the initramfs mounts the matching
//! trio when booting that set — see the `mountcrypt` hook.
//!
//! These primitives are the shared foundation for `deploytix update`
//! (writable set + `pacman` + boot pointer) and `deploytix rollback` (repoint at
//! an older set).

use crate::immutable::PAIR_MARKER;
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

/// Top-level directory (inside the filesystem root subvolume) that holds
/// deploytix transactional snapshot sets on each btrfs filesystem.
pub const SETS_DIR: &str = "@deploytix-sets";

/// The btrfs filesystems backing the immutable subvolumes.
///
/// In multi-volume encrypted layouts `usr_fs` is a separate container
/// (`Crypt-Usr`); in single-partition layouts it equals `root_fs`.
#[derive(Debug, Clone)]
pub struct ImmutableDevices {
    /// btrfs carrying `@`, `@etc` and the sets container (e.g. `/dev/mapper/Crypt-Root`).
    pub root_fs: String,
    /// btrfs carrying `@usr` (e.g. `/dev/mapper/Crypt-Usr`; may equal `root_fs`).
    pub usr_fs: String,
}

impl ImmutableDevices {
    /// True when `@usr` shares the root filesystem (single-partition layout).
    pub fn usr_on_root(&self) -> bool {
        self.usr_fs == self.root_fs
    }
}

/// Subvolume path (relative to a filesystem root) of a set's root snapshot.
pub fn set_root_subvol(id: &str) -> String {
    format!("{SETS_DIR}/{id}/root")
}

/// Subvolume path (relative to a filesystem root) of a set's `/etc` snapshot.
pub fn set_etc_subvol(id: &str) -> String {
    format!("{SETS_DIR}/{id}/etc")
}

/// Subvolume path (relative to a filesystem root) of a set's `/usr` snapshot.
pub fn set_usr_subvol(id: &str) -> String {
    format!("{SETS_DIR}/{id}/usr")
}

/// Allocate a fresh, sortable set id (seconds since the Unix epoch).
pub fn new_set_id() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

/// Wrap `body` so it runs with `device`'s filesystem root (subvolid=5) mounted
/// at the shell variable `$m` (a private temp dir), always unmounted afterward.
/// `body` may reference `$m` and `$id`.
fn with_fs_root(device: &str, id: &str, body: &str) -> String {
    format!(
        "set -e; id={id}; m=$(mktemp -d /run/deploytix-set.XXXXXX); \
         mount -t btrfs -o subvolid=5 {device} \"$m\"; \
         rc=0; {{ {body}; }} || rc=$?; \
         umount \"$m\"; rmdir \"$m\" 2>/dev/null || true; exit $rc",
        id = id,
        device = device,
        body = body,
    )
}

/// Shell body that snapshots the live root-fs subvolumes (`@`, `@etc`) into a
/// set. `-r` yields read-only (archival) snapshots; omit for writable ones a
/// transactional update can `pacman` into.
fn root_fs_snapshot_body(readonly: bool) -> String {
    let ro = if readonly { " -r" } else { "" };
    format!(
        "mkdir -p \"$m/{SETS_DIR}/$id\"; \
         btrfs subvolume snapshot{ro} \"$m/@\" \"$m/{SETS_DIR}/$id/root\"; \
         btrfs subvolume snapshot{ro} \"$m/@etc\" \"$m/{SETS_DIR}/$id/etc\"",
        ro = ro,
    )
}

/// Shell body that snapshots the live `@usr` into a set on the usr filesystem.
fn usr_fs_snapshot_body(readonly: bool) -> String {
    let ro = if readonly { " -r" } else { "" };
    format!(
        "mkdir -p \"$m/{SETS_DIR}/$id\"; \
         btrfs subvolume snapshot{ro} \"$m/@usr\" \"$m/{SETS_DIR}/$id/usr\"",
        ro = ro,
    )
}

/// Build the command that creates a full paired set from the live subvolumes.
///
/// Split into per-filesystem invocations (returned in run order). In
/// single-partition layouts a single command snapshots all three.
pub fn create_set_cmds(devices: &ImmutableDevices, id: &str, readonly: bool) -> Vec<String> {
    if devices.usr_on_root() {
        let body = format!(
            "{}; {}",
            root_fs_snapshot_body(readonly),
            usr_fs_snapshot_body(readonly)
        );
        vec![with_fs_root(&devices.root_fs, id, &body)]
    } else {
        vec![
            with_fs_root(&devices.root_fs, id, &root_fs_snapshot_body(readonly)),
            with_fs_root(&devices.usr_fs, id, &usr_fs_snapshot_body(readonly)),
        ]
    }
}

/// Build the command that writes the pairing marker into a set's root snapshot,
/// naming the set's own `usr`/`etc` subvolumes.
pub fn write_pair_marker_cmd(root_fs: &str, id: &str) -> String {
    let body = format!(
        "printf 'usr=%s\\netc=%s\\n' \"{usr}\" \"{etc}\" > \"$m/{root}/{marker}\"",
        usr = set_usr_subvol(id),
        etc = set_etc_subvol(id),
        root = set_root_subvol(id),
        marker = PAIR_MARKER,
    );
    with_fs_root(root_fs, id, &body)
}

/// Build the command that deletes every subvolume of a set (both filesystems).
pub fn delete_set_cmds(devices: &ImmutableDevices, id: &str) -> Vec<String> {
    let del_root = format!(
        "btrfs subvolume delete \"$m/{SETS_DIR}/$id/root\" 2>/dev/null || true; \
         btrfs subvolume delete \"$m/{SETS_DIR}/$id/etc\" 2>/dev/null || true",
    );
    let del_usr = "btrfs subvolume delete \"$m/@deploytix-sets/$id/usr\" 2>/dev/null || true; \
                   rmdir \"$m/@deploytix-sets/$id\" 2>/dev/null || true"
        .to_string();
    if devices.usr_on_root() {
        let body = format!(
            "{}; {}; rmdir \"$m/{SETS_DIR}/$id\" 2>/dev/null || true",
            del_root, del_usr
        );
        vec![with_fs_root(&devices.root_fs, id, &body)]
    } else {
        vec![
            with_fs_root(&devices.root_fs, id, &del_root),
            with_fs_root(&devices.usr_fs, id, &del_usr),
        ]
    }
}

/// Create a paired snapshot set from the live subvolumes and write its pairing
/// marker. Returns the new set id.
pub fn create_set(
    cmd: &CommandRunner,
    devices: &ImmutableDevices,
    readonly: bool,
) -> Result<String> {
    let id = new_set_id();
    info!(
        "[immutable] Creating {} snapshot set {}",
        if readonly { "read-only" } else { "writable" },
        id
    );
    for c in create_set_cmds(devices, &id, readonly) {
        cmd.run("sh", &["-c", &c])?;
    }
    cmd.run("sh", &["-c", &write_pair_marker_cmd(&devices.root_fs, &id)])?;
    Ok(id)
}

/// Delete a paired snapshot set (all three subvolumes, both filesystems).
pub fn delete_set(cmd: &CommandRunner, devices: &ImmutableDevices, id: &str) -> Result<()> {
    info!("[immutable] Deleting snapshot set {}", id);
    for c in delete_set_cmds(devices, id) {
        cmd.run("sh", &["-c", &c])?;
    }
    Ok(())
}

/// List existing set ids on `root_fs`, sorted ascending (oldest first).
pub fn list_sets(cmd: &CommandRunner, root_fs: &str) -> Result<Vec<String>> {
    if cmd.is_dry_run() {
        return Ok(Vec::new());
    }
    let body = format!("ls -1 \"$m/{SETS_DIR}\" 2>/dev/null || true");
    let script = with_fs_root(root_fs, "-", &body);
    let out = cmd
        .run("sh", &["-c", &script])?
        .ok_or_else(|| DeploytixError::CommandFailed {
            command: "list_sets".into(),
            stderr: "produced no output".into(),
        })?;
    let mut ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    ids.sort();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multi() -> ImmutableDevices {
        ImmutableDevices {
            root_fs: "/dev/mapper/Crypt-Root".into(),
            usr_fs: "/dev/mapper/Crypt-Usr".into(),
        }
    }
    fn single() -> ImmutableDevices {
        ImmutableDevices {
            root_fs: "/dev/mapper/Crypt-Root".into(),
            usr_fs: "/dev/mapper/Crypt-Root".into(),
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
    fn subvol_paths_are_stable() {
        assert_eq!(set_root_subvol("42"), "@deploytix-sets/42/root");
        assert_eq!(set_etc_subvol("42"), "@deploytix-sets/42/etc");
        assert_eq!(set_usr_subvol("42"), "@deploytix-sets/42/usr");
    }

    #[test]
    fn multi_volume_create_uses_two_commands() {
        let cmds = create_set_cmds(&multi(), "42", false);
        assert_eq!(cmds.len(), 2, "separate usr fs needs its own snapshot cmd");
        assert!(cmds[0].contains("subvolume snapshot \"$m/@\" \"$m/@deploytix-sets/$id/root\""));
        assert!(cmds[0].contains("subvolume snapshot \"$m/@etc\""));
        assert!(cmds[1].contains("subvolume snapshot \"$m/@usr\""));
        assert!(cmds[1].contains("mount -t btrfs -o subvolid=5 /dev/mapper/Crypt-Usr"));
        cmds.iter().for_each(|c| assert_valid_shell(c));
    }

    #[test]
    fn single_partition_create_uses_one_command() {
        let cmds = create_set_cmds(&single(), "42", true);
        assert_eq!(cmds.len(), 1, "shared fs snapshots all three together");
        assert!(cmds[0].contains("subvolume snapshot -r \"$m/@\""));
        assert!(cmds[0].contains("subvolume snapshot -r \"$m/@usr\""));
        assert_valid_shell(&cmds[0]);
    }

    #[test]
    fn pair_marker_records_set_subvols() {
        let script = write_pair_marker_cmd("/dev/mapper/Crypt-Root", "42");
        assert!(script.contains("usr=%s"));
        assert!(script.contains("@deploytix-sets/42/usr"));
        assert!(script.contains("@deploytix-sets/42/etc"));
        assert!(script.contains(".deploytix-pair"));
        assert_valid_shell(&script);
    }

    #[test]
    fn delete_removes_all_subvols() {
        for devices in [multi(), single()] {
            for c in delete_set_cmds(&devices, "42") {
                assert!(c.contains("subvolume delete"));
                assert_valid_shell(&c);
            }
        }
    }
}
