//! Paired snapshot sets for the transactional immutable model.
//!
//! A *snapshot set* captures the three OS subvolumes — root, `usr` and `etc` —
//! under a shared id so they roll back together. Sets live in a top-level
//! [`SETS_DIR`] directory on each btrfs filesystem:
//!
//! ```text
//! root fs (Crypt-Root):  @deploytix-sets/<id>/root   (snapshot of the source root)
//!                        @deploytix-sets/<id>/etc    (snapshot of the source etc)
//! usr  fs (Crypt-Usr):   @deploytix-sets/<id>/usr    (snapshot of the source usr)
//! ```
//!
//! The *source* is a [`SubvolSet`]: the install-time base `{@, @usr, @etc}` on a
//! never-updated system, otherwise the set the system is currently running. Each
//! update snapshots the running set, so updates compose.
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

/// The three OS subvolumes that together make up one bootable system state.
///
/// A new set is always snapshotted **from** one of these — normally the one the
/// system is currently running — so that successive updates compose instead of
/// each rebasing onto the install-time base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubvolSet {
    /// Root subvolume, on the root filesystem.
    pub root: String,
    /// `/usr` subvolume, on the usr filesystem (which may be the root one).
    pub usr: String,
    /// `/etc` subvolume, on the root filesystem.
    pub etc: String,
}

impl SubvolSet {
    /// The base subvolumes written at install time (`@`, `@usr`, `@etc`).
    ///
    /// Only the state a never-updated system boots; after the first update the
    /// running state is a snapshot set, not this.
    pub fn base() -> Self {
        Self {
            root: crate::immutable::ROOT_SUBVOL.to_string(),
            usr: crate::immutable::USR_SUBVOL.to_string(),
            etc: crate::immutable::ETC_SUBVOL.to_string(),
        }
    }

    /// The subvolumes belonging to snapshot set `id`.
    pub fn of_set(id: &str) -> Self {
        Self {
            root: set_root_subvol(id),
            usr: set_usr_subvol(id),
            etc: set_etc_subvol(id),
        }
    }

    /// The set for a root subvolume path, by the sibling convention: a set's
    /// root pairs with its own `usr`/`etc`, the base `@` with `@usr`/`@etc`.
    pub fn for_root(root_subvol: &str) -> Self {
        match crate::immutable::boot::pointer_set_id(root_subvol) {
            Some(id) => Self::of_set(&id),
            None => Self::base(),
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

/// Shell body that snapshots `source`'s root-fs subvolumes (root and etc) into
/// a set. `-r` yields read-only (archival) snapshots; omit for writable ones a
/// transactional update can `pacman` into.
fn root_fs_snapshot_body(source: &SubvolSet, readonly: bool) -> String {
    let ro = if readonly { " -r" } else { "" };
    format!(
        "mkdir -p \"$m/{SETS_DIR}/$id\"; \
         btrfs subvolume snapshot{ro} \"$m/{src_root}\" \"$m/{SETS_DIR}/$id/root\"; \
         btrfs subvolume snapshot{ro} \"$m/{src_etc}\" \"$m/{SETS_DIR}/$id/etc\"",
        ro = ro,
        src_root = source.root,
        src_etc = source.etc,
    )
}

/// Shell body that snapshots `source`'s `/usr` into a set on the usr filesystem.
fn usr_fs_snapshot_body(source: &SubvolSet, readonly: bool) -> String {
    let ro = if readonly { " -r" } else { "" };
    format!(
        "mkdir -p \"$m/{SETS_DIR}/$id\"; \
         btrfs subvolume snapshot{ro} \"$m/{src_usr}\" \"$m/{SETS_DIR}/$id/usr\"",
        ro = ro,
        src_usr = source.usr,
    )
}

/// Build the command that creates a full paired set from `source`.
///
/// `source` is normally the set the system is running (see
/// [`crate::immutable::boot::running_subvols`]), so each update builds on the
/// last one rather than on the install-time base.
///
/// Split into per-filesystem invocations (returned in run order). In
/// single-partition layouts a single command snapshots all three.
pub fn create_set_cmds(
    devices: &ImmutableDevices,
    id: &str,
    source: &SubvolSet,
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
///
/// Each filesystem's `<id>` directory goes too, and each command fails unless
/// it is gone afterwards. A subvolume already missing is not an error — the
/// delete is retried by the next prune — but one that survives is reported.
///
/// The root filesystem's directory matters most: [`list_sets`] reads it, so an
/// empty one left behind was taken for a set on every later update and offered
/// as a rollback target with nothing in it to boot.
pub fn delete_set_cmds(devices: &ImmutableDevices, id: &str) -> Vec<String> {
    delete_set_bodies(devices)
        .into_iter()
        .map(|(device, body)| with_fs_root(device, id, &body))
        .collect()
}

/// The shell bodies [`delete_set_cmds`] runs, each with the filesystem it runs on.
fn delete_set_bodies(devices: &ImmutableDevices) -> Vec<(&str, String)> {
    let del_root = format!(
        "btrfs subvolume delete \"$m/{SETS_DIR}/$id/root\" 2>/dev/null || true; \
         btrfs subvolume delete \"$m/{SETS_DIR}/$id/etc\" 2>/dev/null || true",
    );
    let del_usr = format!("btrfs subvolume delete \"$m/{SETS_DIR}/$id/usr\" 2>/dev/null || true");
    let remove_dir =
        format!("rmdir \"$m/{SETS_DIR}/$id\" 2>/dev/null || true; [ ! -e \"$m/{SETS_DIR}/$id\" ]");
    if devices.usr_on_root() {
        vec![(
            &devices.root_fs,
            format!("{del_root}; {del_usr}; {remove_dir}"),
        )]
    } else {
        vec![
            (&devices.root_fs, format!("{del_root}; {remove_dir}")),
            (&devices.usr_fs, format!("{del_usr}; {remove_dir}")),
        ]
    }
}

/// Create a paired snapshot set from `source` and write its pairing marker.
/// Returns the new set id.
///
/// The new set's marker names its *own* `usr`/`etc`, overwriting the one
/// inherited from `source`, so booting it mounts the matching trio.
pub fn create_set(
    cmd: &CommandRunner,
    devices: &ImmutableDevices,
    source: &SubvolSet,
    readonly: bool,
) -> Result<String> {
    let id = new_set_id();
    info!(
        "[immutable] Creating {} snapshot set {} from {}",
        if readonly { "read-only" } else { "writable" },
        id,
        source.root,
    );
    for c in create_set_cmds(devices, &id, source, readonly) {
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
///
/// A set is a directory holding a `root` snapshot. A directory without one —
/// left by an older delete, or by a create that failed part-way — is not a set:
/// nothing in it can boot, so it is neither pruned again nor offered for rollback.
pub fn list_sets(cmd: &CommandRunner, root_fs: &str) -> Result<Vec<String>> {
    if cmd.is_dry_run() {
        return Ok(Vec::new());
    }
    let body = list_sets_body();
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

/// Shell body printing the id of every set directory that holds a `root` snapshot.
fn list_sets_body() -> String {
    format!(
        "for d in \"$m/{SETS_DIR}\"/*/; do \
         [ -e \"${{d}}root\" ] && basename \"$d\"; \
         done; true"
    )
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
        let cmds = create_set_cmds(&multi(), "42", &SubvolSet::base(), false);
        assert_eq!(cmds.len(), 2, "separate usr fs needs its own snapshot cmd");
        assert!(cmds[0].contains("subvolume snapshot \"$m/@\" \"$m/@deploytix-sets/$id/root\""));
        assert!(cmds[0].contains("subvolume snapshot \"$m/@etc\""));
        assert!(cmds[1].contains("subvolume snapshot \"$m/@usr\""));
        assert!(cmds[1].contains("mount -t btrfs -o subvolid=5 /dev/mapper/Crypt-Usr"));
        cmds.iter().for_each(|c| assert_valid_shell(c));
    }

    #[test]
    fn single_partition_create_uses_one_command() {
        let cmds = create_set_cmds(&single(), "42", &SubvolSet::base(), true);
        assert_eq!(cmds.len(), 1, "shared fs snapshots all three together");
        assert!(cmds[0].contains("subvolume snapshot -r \"$m/@\""));
        assert!(cmds[0].contains("subvolume snapshot -r \"$m/@usr\""));
        assert_valid_shell(&cmds[0]);
    }

    /// The bug this guards: `create_set` used to snapshot the hardcoded `@`,
    /// so the second and every later update rebased onto the install-time base
    /// instead of building on the running set — silently discarding every
    /// earlier update.
    #[test]
    fn a_set_is_snapshotted_from_its_source_not_from_the_base() {
        let source = SubvolSet::of_set("100");
        let cmds = create_set_cmds(&multi(), "200", &source, false);

        assert!(
            cmds[0].contains(
                "subvolume snapshot \"$m/@deploytix-sets/100/root\" \"$m/@deploytix-sets/$id/root\""
            ),
            "root must come from the source set, got: {}",
            cmds[0]
        );
        assert!(cmds[0].contains("subvolume snapshot \"$m/@deploytix-sets/100/etc\""));
        assert!(cmds[1].contains("subvolume snapshot \"$m/@deploytix-sets/100/usr\""));
        // Nothing may reference the base subvolumes when the source is a set.
        for c in &cmds {
            assert!(!c.contains("\"$m/@\""), "base root leaked in: {c}");
            assert!(!c.contains("\"$m/@usr\""), "base usr leaked in: {c}");
            assert!(!c.contains("\"$m/@etc\""), "base etc leaked in: {c}");
            assert_valid_shell(c);
        }
    }

    #[test]
    fn subvol_sets_resolve_by_root_path() {
        assert_eq!(SubvolSet::for_root("@"), SubvolSet::base());
        assert_eq!(
            SubvolSet::for_root("@deploytix-sets/7/root"),
            SubvolSet::of_set("7")
        );
        let base = SubvolSet::base();
        assert_eq!(
            (base.root.as_str(), base.usr.as_str(), base.etc.as_str()),
            ("@", "@usr", "@etc")
        );
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

    /// Runs `body` as `with_fs_root` would, with `$m` a plain directory instead
    /// of a mounted filesystem and `btrfs subvolume delete` a stub that removes
    /// the directory it is given. Returns whether the body succeeded.
    fn run_body(m: &std::path::Path, id: &str, body: &str) -> bool {
        let bin = m.join(".bin");
        std::fs::create_dir_all(&bin).unwrap();
        let stub = bin.join("btrfs");
        std::fs::write(&stub, "#!/bin/sh\n[ -d \"$3\" ] || exit 1\nrm -rf \"$3\"\n").unwrap();
        std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("m={}; id={id}; {body}", m.display()))
            .env("PATH", path)
            .status()
            .unwrap()
            .success()
    }

    fn make_set(m: &std::path::Path, id: &str, parts: &[&str]) {
        for part in parts {
            std::fs::create_dir_all(m.join(SETS_DIR).join(id).join(part)).unwrap();
        }
    }

    fn temp_fs(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "deploytix-sets-{name}-{}-{}",
            std::process::id(),
            new_set_id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(SETS_DIR)).unwrap();
        dir
    }

    /// The bug this guards: with `/usr` on its own filesystem, deleting a set
    /// left its empty directory on the root filesystem. `list_sets` reads that
    /// one, so every later update "deleted" the same sets again and rollback
    /// offered them as targets with nothing in them to boot.
    #[test]
    fn delete_leaves_no_directory_on_either_filesystem() {
        let devices = multi();
        let (root_fs, usr_fs) = (temp_fs("root"), temp_fs("usr"));
        make_set(&root_fs, "42", &["root", "etc"]);
        make_set(&usr_fs, "42", &["usr"]);

        for (device, body) in delete_set_bodies(&devices) {
            let m = if device == devices.root_fs {
                &root_fs
            } else {
                &usr_fs
            };
            assert!(run_body(m, "42", &body), "delete failed on {device}");
        }

        assert!(!root_fs.join(SETS_DIR).join("42").exists());
        assert!(!usr_fs.join(SETS_DIR).join("42").exists());
        let _ = std::fs::remove_dir_all(root_fs);
        let _ = std::fs::remove_dir_all(usr_fs);
    }

    #[test]
    fn delete_on_one_filesystem_removes_the_whole_set() {
        let devices = single();
        let fs = temp_fs("single");
        make_set(&fs, "42", &["root", "etc", "usr"]);

        let bodies = delete_set_bodies(&devices);
        assert_eq!(bodies.len(), 1);
        assert!(run_body(&fs, "42", &bodies[0].1));

        assert!(!fs.join(SETS_DIR).join("42").exists());
        let _ = std::fs::remove_dir_all(fs);
    }

    #[test]
    fn deleting_an_already_empty_directory_succeeds() {
        let fs = temp_fs("empty");
        make_set(&fs, "42", &[]);

        let (_, body) = delete_set_bodies(&multi()).swap_remove(0);
        assert!(run_body(&fs, "42", &body));
        assert!(!fs.join(SETS_DIR).join("42").exists());
        let _ = std::fs::remove_dir_all(fs);
    }

    #[test]
    fn a_set_that_survives_its_delete_is_reported() {
        let fs = temp_fs("stuck");
        make_set(&fs, "42", &["root", "etc"]);
        // Something the delete does not remove keeps the directory alive.
        std::fs::write(fs.join(SETS_DIR).join("42").join("stray"), "").unwrap();

        let (_, body) = delete_set_bodies(&multi()).swap_remove(0);
        assert!(
            !run_body(&fs, "42", &body),
            "a surviving set must fail the delete"
        );
        let _ = std::fs::remove_dir_all(fs);
    }

    #[test]
    fn only_directories_holding_a_root_snapshot_are_sets() {
        let fs = temp_fs("list");
        make_set(&fs, "100", &["root", "etc"]);
        make_set(&fs, "200", &[]);
        make_set(&fs, "300", &["etc"]);
        make_set(&fs, "400", &["root", "etc"]);

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("m={}; {}", fs.display(), list_sets_body()))
            .output()
            .unwrap();
        assert!(out.status.success());
        let mut ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        ids.sort();
        assert_eq!(ids, ["100", "400"]);
        let _ = std::fs::remove_dir_all(fs);
    }

    #[test]
    fn listing_an_empty_sets_directory_succeeds() {
        let fs = temp_fs("none");
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("m={}; {}", fs.display(), list_sets_body()))
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        let _ = std::fs::remove_dir_all(fs);
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
