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
///
/// `body` runs on the left of `||`, where `set -e` does not apply: its status
/// is its last command's. A body whose every step must succeed chains them
/// with `&&`.
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
        "mkdir -p \"$m/{SETS_DIR}/$id\" && \
         btrfs subvolume snapshot{ro} \"$m/{src_root}\" \"$m/{SETS_DIR}/$id/root\" && \
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
        "mkdir -p \"$m/{SETS_DIR}/$id\" && \
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
    create_set_bodies(devices, source, readonly)
        .into_iter()
        .map(|(device, body)| with_fs_root(device, id, &body))
        .collect()
}

/// The shell bodies [`create_set_cmds`] runs, each with the filesystem it runs on.
fn create_set_bodies<'a>(
    devices: &'a ImmutableDevices,
    source: &SubvolSet,
    readonly: bool,
) -> Vec<(&'a str, String)> {
    if devices.usr_on_root() {
        let body = format!(
            "{} && {}",
            root_fs_snapshot_body(source, readonly),
            usr_fs_snapshot_body(source, readonly)
        );
        vec![(&devices.root_fs, body)]
    } else {
        vec![
            (&devices.root_fs, root_fs_snapshot_body(source, readonly)),
            (&devices.usr_fs, usr_fs_snapshot_body(source, readonly)),
        ]
    }
}

/// Build the command that writes the pairing marker into a set's root snapshot,
/// naming the set's own `usr`/`etc` subvolumes.
pub fn write_pair_marker_cmd(root_fs: &str, id: &str) -> String {
    with_fs_root(root_fs, id, &pair_marker_body(id))
}

/// Shell body writing set `id`'s pairing marker into its root snapshot.
fn pair_marker_body(id: &str) -> String {
    format!(
        "printf 'usr=%s\\netc=%s\\n' \"{usr}\" \"{etc}\" > \"$m/{root}/{marker}\"",
        usr = set_usr_subvol(id),
        etc = set_etc_subvol(id),
        root = set_root_subvol(id),
        marker = PAIR_MARKER,
    )
}

/// Build the command that deletes every subvolume of a set (both filesystems).
///
/// Each filesystem's `<id>` directory goes too, and each command fails unless
/// it is gone afterwards, saying what is left. A subvolume already missing is
/// not an error; one that survives is, and [`list_leftovers`] finds its
/// directory again so the next prune retries the delete.
///
/// The root snapshot goes first: it holds the set's pairing marker, so once it
/// is gone [`list_sets`] no longer offers the set, however much of it remains.
pub fn delete_set_cmds(devices: &ImmutableDevices, id: &str) -> Vec<String> {
    delete_set_bodies(devices)
        .into_iter()
        .map(|(device, body)| with_fs_root(device, id, &body))
        .collect()
}

/// The shell bodies [`delete_set_cmds`] runs, each with the filesystem it runs on.
fn delete_set_bodies(devices: &ImmutableDevices) -> Vec<(&str, String)> {
    if devices.usr_on_root() {
        vec![(
            &devices.root_fs,
            delete_body(&devices.root_fs, &["root", "etc", "usr"]),
        )]
    } else {
        vec![
            (
                &devices.root_fs,
                delete_body(&devices.root_fs, &["root", "etc"]),
            ),
            (&devices.usr_fs, delete_body(&devices.usr_fs, &["usr"])),
        ]
    }
}

/// Shell body deleting `subvols` of set `$id` on `device`, then its directory.
///
/// stderr is kept: when something survives, btrfs's and rmdir's reasons are
/// what the error carries, followed by what is still in the directory.
fn delete_body(device: &str, subvols: &[&str]) -> String {
    let deletes: String = subvols
        .iter()
        .map(|s| format!("[ ! -e \"$d/{s}\" ] || btrfs subvolume delete \"$d/{s}\" || true; "))
        .collect();
    format!(
        "d=\"$m/{SETS_DIR}/$id\"; {deletes}\
         [ ! -e \"$d\" ] || rmdir \"$d\" || true; \
         [ ! -e \"$d\" ] || {{ echo \"set $id not removed from {device}; \
         {SETS_DIR}/$id still holds: $(ls -A \"$d\" | tr '\\n' ' ')\" >&2; false; }}"
    )
}

/// Create a paired snapshot set from `source` and write its pairing marker.
/// Returns the new set id.
///
/// The new set's marker names its *own* `usr`/`etc`, overwriting the one
/// inherited from `source`, so booting it mounts the matching trio. The marker
/// is written last, which is what makes it the sign of a complete set
/// ([`list_sets`]). If any step fails, what was made is deleted again before
/// the error is returned: no transaction exists yet to discard it.
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
    let created = (|| -> Result<()> {
        for c in create_set_cmds(devices, &id, source, readonly) {
            cmd.run("sh", &["-c", &c])?;
        }
        cmd.run("sh", &["-c", &write_pair_marker_cmd(&devices.root_fs, &id)])?;
        Ok(())
    })();
    if let Err(e) = created {
        warn!(
            "[immutable] Creating set {} failed; removing what it made",
            id
        );
        if let Err(del) = delete_set(cmd, devices, &id) {
            // Pruning finds it again through `list_leftovers`.
            warn!("[immutable] Could not remove partial set {}: {}", id, del);
        }
        return Err(e);
    }
    Ok(id)
}

/// Delete a paired snapshot set (all three subvolumes, both filesystems).
///
/// Every filesystem's part is attempted even when an earlier one fails, and the
/// first error is returned: stopping at the root filesystem left the set's
/// `usr` behind with nothing pointing at it.
pub fn delete_set(cmd: &CommandRunner, devices: &ImmutableDevices, id: &str) -> Result<()> {
    info!("[immutable] Deleting snapshot set {}", id);
    let mut first_err = None;
    for c in delete_set_cmds(devices, id) {
        if let Err(e) = cmd.run("sh", &["-c", &c]) {
            first_err.get_or_insert(e);
        }
    }
    first_err.map_or(Ok(()), Err)
}

/// List complete set ids on `root_fs`, sorted ascending (oldest first).
///
/// A set is complete when its root snapshot's pairing marker names the set's
/// own `usr`. [`create_set`] writes that marker last and [`delete_set`] removes
/// the root first, so a set created or deleted only part-way is not listed:
/// booting it would mount another set's `usr` and `etc`, or none. Such sets are
/// [`list_leftovers`], for pruning; this list is what rollback offers.
pub fn list_sets(cmd: &CommandRunner, root_fs: &str) -> Result<Vec<String>> {
    if cmd.is_dry_run() {
        return Ok(Vec::new());
    }
    list_ids(cmd, root_fs, &list_sets_body())
}

/// Ids of set directories, on either filesystem, that are not complete sets:
/// what a create or delete that failed part-way left behind, and the empty
/// directories older versions left on every delete. Nothing in one can boot,
/// so pruning deletes them however many sets it keeps, and lists them again
/// until they are gone.
pub fn list_leftovers(cmd: &CommandRunner, devices: &ImmutableDevices) -> Result<Vec<String>> {
    if cmd.is_dry_run() {
        return Ok(Vec::new());
    }
    let complete = list_sets(cmd, &devices.root_fs)?;
    let mut dirs = list_ids(cmd, &devices.root_fs, &list_dirs_body())?;
    if !devices.usr_on_root() {
        dirs.extend(list_ids(cmd, &devices.usr_fs, &list_dirs_body())?);
    }
    dirs.sort();
    dirs.dedup();
    dirs.retain(|id| !complete.contains(id));
    Ok(dirs)
}

/// Run a listing `body` on `device` and return the ids it prints, sorted.
fn list_ids(cmd: &CommandRunner, device: &str, body: &str) -> Result<Vec<String>> {
    let script = with_fs_root(device, "-", body);
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

/// Shell body printing the id of every set whose root carries its own marker.
fn list_sets_body() -> String {
    format!(
        "for d in \"$m/{SETS_DIR}\"/*/; do \
         s=$(basename \"$d\"); \
         grep -qxF \"usr={SETS_DIR}/$s/usr\" \"${{d}}root/{PAIR_MARKER}\" 2>/dev/null \
         && echo \"$s\"; \
         done; true"
    )
}

/// Shell body printing the id of every set directory, complete or not.
fn list_dirs_body() -> String {
    format!(
        "for d in \"$m/{SETS_DIR}\"/*/; do \
         [ -d \"$d\" ] && basename \"$d\"; \
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

    /// A stand-in for `btrfs subvolume {snapshot,delete}` on plain directories.
    /// A subvolume holding a `.busy` file refuses to be deleted, as a mounted
    /// one does.
    const BTRFS_STUB: &str = r#"#!/bin/sh
[ "$1" = subvolume ] || exit 2
case "$2" in
delete)
    [ -d "$3" ] || { echo "ERROR: Could not statfs: $3" >&2; exit 1; }
    [ ! -e "$3/.busy" ] || { echo "ERROR: Could not destroy $3: Device or resource busy" >&2; exit 1; }
    rm -rf "$3" ;;
snapshot)
    shift 2; [ "$1" = -r ] && shift
    [ -d "$1" ] || { echo "ERROR: Could not statfs: $1" >&2; exit 1; }
    mkdir "$2" ;;
*) exit 2 ;;
esac
"#;

    /// Runs `body` as `with_fs_root` would, with `$m` a plain directory instead
    /// of a mounted filesystem and `btrfs` the stub above.
    fn run_body_output(m: &std::path::Path, id: &str, body: &str) -> std::process::Output {
        let bin = m.join(".bin");
        std::fs::create_dir_all(&bin).unwrap();
        let stub = bin.join("btrfs");
        std::fs::write(&stub, BTRFS_STUB).unwrap();
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
            .output()
            .unwrap()
    }

    /// [`run_body_output`], reduced to whether the body succeeded.
    fn run_body(m: &std::path::Path, id: &str, body: &str) -> bool {
        run_body_output(m, id, body).status.success()
    }

    /// The ids a listing `body` prints for the filesystem at `m`, sorted.
    fn listed(m: &std::path::Path, body: &str) -> Vec<String> {
        let out = run_body_output(m, "-", body);
        assert!(out.status.success(), "listing failed");
        let mut ids: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        ids.sort();
        ids
    }

    fn make_set(m: &std::path::Path, id: &str, parts: &[&str]) {
        let dir = m.join(SETS_DIR).join(id);
        std::fs::create_dir_all(&dir).unwrap();
        for part in parts {
            std::fs::create_dir_all(dir.join(part)).unwrap();
        }
    }

    /// A set as [`create_set`] leaves it: its parts plus its own marker.
    fn make_complete_set(m: &std::path::Path, id: &str, parts: &[&str]) {
        make_set(m, id, parts);
        assert!(
            run_body(m, id, &pair_marker_body(id)),
            "marker write failed"
        );
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
        let out = run_body_output(&fs, "42", &body);
        assert!(out.status.success());
        assert!(
            out.stderr.is_empty(),
            "missing subvolumes are not worth a word: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!fs.join(SETS_DIR).join("42").exists());
        let _ = std::fs::remove_dir_all(fs);
    }

    #[test]
    fn deleting_a_set_that_is_already_gone_succeeds() {
        let fs = temp_fs("gone");
        let (_, body) = delete_set_bodies(&multi()).swap_remove(0);
        assert!(run_body(&fs, "42", &body));
        let _ = std::fs::remove_dir_all(fs);
    }

    #[test]
    fn a_set_that_survives_its_delete_is_reported_with_what_is_left() {
        let fs = temp_fs("stuck");
        make_set(&fs, "42", &["root", "etc"]);
        // Something the delete does not remove keeps the directory alive.
        std::fs::write(fs.join(SETS_DIR).join("42").join("stray"), "").unwrap();

        let (_, body) = delete_set_bodies(&multi()).swap_remove(0);
        let out = run_body_output(&fs, "42", &body);
        assert!(
            !out.status.success(),
            "a surviving set must fail the delete"
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("set 42 not removed from /dev/mapper/Crypt-Root")
                && stderr.contains("stray"),
            "the error must say what survived, got: {stderr}"
        );
        let _ = std::fs::remove_dir_all(fs);
    }

    /// The bug this guards: the root snapshot is deleted first, and the
    /// listing used to require one, so a set whose `etc` survived its delete
    /// was never listed — and never deleted — again.
    #[test]
    fn a_half_deleted_set_is_a_leftover_and_its_delete_can_be_retried() {
        let fs = temp_fs("half");
        make_complete_set(&fs, "42", &["root", "etc"]);
        std::fs::write(fs.join(SETS_DIR).join("42").join("etc").join(".busy"), "").unwrap();

        let (_, body) = delete_set_bodies(&multi()).swap_remove(0);
        let out = run_body_output(&fs, "42", &body);
        assert!(!out.status.success());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("busy"),
            "btrfs's reason must reach the error: {stderr}"
        );
        assert!(!fs.join(SETS_DIR).join("42").join("root").exists());

        assert!(
            listed(&fs, &list_sets_body()).is_empty(),
            "not a rollback target"
        );
        assert_eq!(
            listed(&fs, &list_dirs_body()),
            ["42"],
            "still found for pruning"
        );

        std::fs::remove_file(fs.join(SETS_DIR).join("42").join("etc").join(".busy")).unwrap();
        assert!(
            run_body(&fs, "42", &body),
            "the retry must finish the delete"
        );
        assert!(listed(&fs, &list_dirs_body()).is_empty());
        let _ = std::fs::remove_dir_all(fs);
    }

    #[test]
    fn only_sets_whose_root_carries_their_own_marker_are_listed() {
        let fs = temp_fs("list");
        make_complete_set(&fs, "100", &["root", "etc"]);
        make_set(&fs, "200", &[]);
        make_set(&fs, "300", &["etc"]);
        make_complete_set(&fs, "400", &["root", "etc"]);
        // A create that failed after the root snapshot: the root still carries
        // the marker it inherited from its source, naming the source's usr.
        make_set(&fs, "500", &["root"]);
        std::fs::copy(
            fs.join(SETS_DIR).join("400/root").join(PAIR_MARKER),
            fs.join(SETS_DIR).join("500/root").join(PAIR_MARKER),
        )
        .unwrap();

        assert_eq!(listed(&fs, &list_sets_body()), ["100", "400"]);
        assert_eq!(
            listed(&fs, &list_dirs_body()),
            ["100", "200", "300", "400", "500"]
        );
        let _ = std::fs::remove_dir_all(fs);
    }

    #[test]
    fn listing_an_empty_sets_directory_succeeds() {
        let fs = temp_fs("none");
        assert!(listed(&fs, &list_sets_body()).is_empty());
        assert!(listed(&fs, &list_dirs_body()).is_empty());
        let _ = std::fs::remove_dir_all(fs);
    }

    #[test]
    fn listing_a_filesystem_with_no_sets_directory_succeeds() {
        let fs = temp_fs("bare");
        std::fs::remove_dir(fs.join(SETS_DIR)).unwrap();
        assert!(listed(&fs, &list_sets_body()).is_empty());
        assert!(listed(&fs, &list_dirs_body()).is_empty());
        let _ = std::fs::remove_dir_all(fs);
    }

    #[test]
    fn a_created_set_is_listed() {
        let fs = temp_fs("create");
        for base in ["@", "@etc", "@usr"] {
            std::fs::create_dir_all(fs.join(base)).unwrap();
        }
        let devices = single();
        let bodies = create_set_bodies(&devices, &SubvolSet::base(), false);
        assert_eq!(bodies.len(), 1);
        assert!(run_body(&fs, "42", &bodies[0].1), "create failed");
        assert!(
            run_body(&fs, "42", &pair_marker_body("42")),
            "marker failed"
        );

        assert_eq!(listed(&fs, &list_sets_body()), ["42"]);
        let _ = std::fs::remove_dir_all(fs);
    }

    /// The bug this guards: the body runs where `set -e` is off, so a failed
    /// root or etc snapshot was masked by the next step succeeding, and the
    /// set was then finished, marked and booted without it.
    #[test]
    fn a_snapshot_that_fails_part_way_fails_the_create() {
        let fs = temp_fs("partial");
        // No `@etc` to snapshot; `@` and `@usr` would both succeed.
        for base in ["@", "@usr"] {
            std::fs::create_dir_all(fs.join(base)).unwrap();
        }
        let devices = single();
        let (_, body) = create_set_bodies(&devices, &SubvolSet::base(), false).swap_remove(0);
        assert!(
            !run_body(&fs, "42", &body),
            "a missing etc must fail the create"
        );
        assert!(
            !fs.join(SETS_DIR).join("42").join("usr").exists(),
            "nothing after the failed step may run"
        );
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
