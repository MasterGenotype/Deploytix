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
use tracing::{info, warn};

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

/// The subvolume trio a new set is snapshotted **from**.
///
/// Updates must build on whatever is current, not on the pristine base: `@` is
/// never written to, so snapshotting it every time would silently drop every
/// package installed by earlier updates. The source is normally the trio the
/// next boot is pointed at — the running system, or a set staged by an earlier
/// `deploytix update`/`rollback` this session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSubvols {
    /// Subvolume to snapshot as the set's `/`.
    pub root: String,
    /// Subvolume to snapshot as the set's `/usr`.
    pub usr: String,
    /// Subvolume to snapshot as the set's `/etc`.
    pub etc: String,
}

impl SourceSubvols {
    /// The pristine base install: `@`, `@usr`, `@etc`.
    pub fn live() -> Self {
        Self {
            root: crate::immutable::ROOT_SUBVOL.to_string(),
            usr: crate::immutable::USR_SUBVOL.to_string(),
            etc: crate::immutable::ETC_SUBVOL.to_string(),
        }
    }

    /// The trio belonging to `root_subvol`, by the set convention: a set uses
    /// its sibling `usr`/`etc`, anything else (the base `@`, or a snapper
    /// snapshot booted from the grub-btrfs menu) pairs with the live
    /// `@usr`/`@etc` its marker names.
    pub fn for_root(root_subvol: &str) -> Self {
        match crate::immutable::boot::pointer_set_id(root_subvol) {
            Some(id) => Self {
                root: set_root_subvol(&id),
                usr: set_usr_subvol(&id),
                etc: set_etc_subvol(&id),
            },
            None => Self {
                root: root_subvol.to_string(),
                ..Self::live()
            },
        }
    }
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
fn root_fs_snapshot_body(source: &SourceSubvols, readonly: bool) -> String {
    let ro = if readonly { " -r" } else { "" };
    format!(
        "mkdir -p \"$m/{SETS_DIR}/$id\"; \
         btrfs subvolume snapshot{ro} \"$m/{root}\" \"$m/{SETS_DIR}/$id/root\"; \
         btrfs subvolume snapshot{ro} \"$m/{etc}\" \"$m/{SETS_DIR}/$id/etc\"",
        ro = ro,
        root = source.root,
        etc = source.etc,
    )
}

/// Shell body that snapshots the live `@usr` into a set on the usr filesystem.
fn usr_fs_snapshot_body(source: &SourceSubvols, readonly: bool) -> String {
    let ro = if readonly { " -r" } else { "" };
    format!(
        "mkdir -p \"$m/{SETS_DIR}/$id\"; \
         btrfs subvolume snapshot{ro} \"$m/{usr}\" \"$m/{SETS_DIR}/$id/usr\"",
        ro = ro,
        usr = source.usr,
    )
}

/// Build the command that creates a full paired set from the live subvolumes.
///
/// Split into per-filesystem invocations (returned in run order). In
/// single-partition layouts a single command snapshots all three.
pub fn create_set_cmds(
    devices: &ImmutableDevices,
    source: &SourceSubvols,
    id: &str,
    readonly: bool,
) -> Vec<String> {
    if devices.usr_on_root() {
        let body = format!(
            "{}; {}",
            root_fs_snapshot_body(source, readonly),
            usr_fs_snapshot_body(source, readonly)
        );
        vec![with_fs_root(&devices.root_fs, id, &body)]
    } else {
        vec![
            with_fs_root(
                &devices.root_fs,
                id,
                &root_fs_snapshot_body(source, readonly),
            ),
            with_fs_root(&devices.usr_fs, id, &usr_fs_snapshot_body(source, readonly)),
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
    source: &SourceSubvols,
    readonly: bool,
) -> Result<String> {
    let id = new_set_id();
    info!(
        "[immutable] Creating {} snapshot set {} from {}",
        if readonly { "read-only" } else { "writable" },
        id,
        source.root
    );
    // A set spans two filesystems in multi-volume layouts, so a failure can
    // leave a half-built set behind — which grub-btrfs would then list as a
    // bootable entry. Unwind it rather than leaking it.
    let build = (|| -> Result<()> {
        for c in create_set_cmds(devices, source, &id, readonly) {
            cmd.run("sh", &["-c", &c])?;
        }
        cmd.run("sh", &["-c", &write_pair_marker_cmd(&devices.root_fs, &id)])?;
        Ok(())
    })();
    if let Err(e) = build {
        warn!(
            "[immutable] Set {} failed to build; removing its partial snapshots",
            id
        );
        let _ = delete_set(cmd, devices, &id);
        return Err(e);
    }
    Ok(id)
}

/// Whether a set's root snapshot is writable.
///
/// Sets built by `deploytix update` are writable — pacman has to write into
/// them — while archival sets (the install baseline) are read-only. That is what
/// separates "an update is already staged, keep filling it" from "the next boot
/// is pointed at an old set, snapshot a new one from it".
///
/// Unknown answers report `false`: snapshotting a fresh set from the staged one
/// preserves its contents, so the conservative branch is never destructive.
pub fn set_is_writable(cmd: &CommandRunner, root_fs: &str, id: &str) -> bool {
    if cmd.is_dry_run() {
        return false;
    }
    // A query that fails — a set already pruned away, a busy device — answers
    // "no" rather than failing the update, since the fresh-set branch is safe.
    match cmd.run("sh", &["-c", &set_ro_property_cmd(root_fs, id)]) {
        Ok(Some(out)) => String::from_utf8_lossy(&out.stdout).contains("ro=false"),
        _ => false,
    }
}

/// Shell that reports a set root snapshot's `ro` property (`ro=true|false`).
fn set_ro_property_cmd(root_fs: &str, id: &str) -> String {
    let body = format!("btrfs property get -ts \"$m/{SETS_DIR}/$id/root\" ro");
    with_fs_root(root_fs, id, &body)
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
    fn a_set_snapshots_the_source_trio_not_the_pristine_base() {
        // The base install: @ / @usr / @etc.
        let live = create_set_cmds(&single(), &SourceSubvols::live(), "42", false);
        assert!(live[0].contains("subvolume snapshot \"$m/@\" \"$m/@deploytix-sets/$id/root\""));

        // An update chained onto an earlier set snapshots that set, so the
        // packages it installed carry forward instead of being dropped.
        let prev = SourceSubvols::for_root("@deploytix-sets/7/root");
        let chained = create_set_cmds(&multi(), &prev, "42", false);
        assert!(chained[0].contains("snapshot \"$m/@deploytix-sets/7/root\""));
        assert!(chained[0].contains("snapshot \"$m/@deploytix-sets/7/etc\""));
        assert!(chained[1].contains("snapshot \"$m/@deploytix-sets/7/usr\""));
        chained.iter().for_each(|c| assert_valid_shell(c));
    }

    #[test]
    fn source_trio_follows_the_set_convention() {
        assert_eq!(SourceSubvols::for_root("@"), SourceSubvols::live());
        assert_eq!(
            SourceSubvols::for_root("@deploytix-sets/9/root"),
            SourceSubvols {
                root: "@deploytix-sets/9/root".into(),
                usr: "@deploytix-sets/9/usr".into(),
                etc: "@deploytix-sets/9/etc".into(),
            }
        );
        // A snapper snapshot booted from the grub-btrfs menu keeps its own
        // root and pairs with the live @usr/@etc, as its marker names.
        assert_eq!(
            SourceSubvols::for_root("@snapshots/1/snapshot"),
            SourceSubvols {
                root: "@snapshots/1/snapshot".into(),
                usr: "@usr".into(),
                etc: "@etc".into(),
            }
        );
    }

    #[test]
    fn writability_query_reads_the_set_root_at_the_fs_root() {
        let script = set_ro_property_cmd("/dev/mapper/Crypt-Root", "42");
        assert!(script.contains("mount -t btrfs -o subvolid=5 /dev/mapper/Crypt-Root"));
        assert!(script.contains("btrfs property get -ts \"$m/@deploytix-sets/$id/root\" ro"));
        // The scratch mount is always released, whatever the query returns.
        assert!(script.contains("umount \"$m\""));
        assert_valid_shell(&script);
    }

    #[test]
    fn subvol_paths_are_stable() {
        assert_eq!(set_root_subvol("42"), "@deploytix-sets/42/root");
        assert_eq!(set_etc_subvol("42"), "@deploytix-sets/42/etc");
        assert_eq!(set_usr_subvol("42"), "@deploytix-sets/42/usr");
    }

    #[test]
    fn multi_volume_create_uses_two_commands() {
        let cmds = create_set_cmds(&multi(), &SourceSubvols::live(), "42", false);
        assert_eq!(cmds.len(), 2, "separate usr fs needs its own snapshot cmd");
        assert!(cmds[0].contains("subvolume snapshot \"$m/@\" \"$m/@deploytix-sets/$id/root\""));
        assert!(cmds[0].contains("subvolume snapshot \"$m/@etc\""));
        assert!(cmds[1].contains("subvolume snapshot \"$m/@usr\""));
        assert!(cmds[1].contains("mount -t btrfs -o subvolid=5 /dev/mapper/Crypt-Usr"));
        cmds.iter().for_each(|c| assert_valid_shell(c));
    }

    #[test]
    fn single_partition_create_uses_one_command() {
        let cmds = create_set_cmds(&single(), &SourceSubvols::live(), "42", true);
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
