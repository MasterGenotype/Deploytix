//! The pacman database, moved inside the OS image so it rolls back with it.
//!
//! # The problem
//!
//! Both immutable backends share their `/var` across every system state: the
//! btrfs backend snapshots `{@, @usr, @etc}` and leaves `@var` alone, and the
//! LVM A/B backend keeps `/var` on its own LV outside both verity-sealed root
//! slots. `/var/lib/pacman` therefore lived in exactly one place while the
//! files it describes lived in several.
//!
//! Every transaction wrote that one database, so it always described the newest
//! state — and a rollback restored the files without restoring it:
//!
//! - roll an **update** back and the database reports versions the files on
//!   disk no longer match;
//! - roll a **removal** back and the files come back while the database still
//!   says the package is gone;
//! - either way `pacman -Qkk` reports missing files for packages the database
//!   claims are installed, and the next transaction plans against a state the
//!   system is not in.
//!
//! # The fix
//!
//! The database is stored at [`DB_DIR`] — inside `/usr`, which *is* part of the
//! image on both backends (`@usr` on btrfs, the root LV on LVM A/B) — and
//! bind-mounted back onto [`DB_MOUNT`], where pacman and everything else looks
//! for it. This is the same move openSUSE MicroOS and Fedora Silverblue made
//! with the RPM database (`/usr/lib/sysimage/rpm`), for the same reason.
//!
//! Nothing needs to know about it. `DBPath` stays at its default, so `pacman`,
//! `pacman -Q`, `yay`, `pactree`, `pacdiff` and every `--dbpath` deploytix
//! passes elsewhere keep working unchanged; the bind mount is what makes the
//! standard path resolve to the running image's own database.
//!
//! Consequences, all of them intended:
//!
//! - a rollback returns the database to that set's/slot's version, because it
//!   never left the subvolume being rolled back;
//! - the live database is read-only, because the `/usr` under it is. Writing it
//!   needs a transaction, which is the rule the rest of the model already
//!   enforces;
//! - a failed transaction discards the set with the database inside it, so
//!   there is nothing to unwind by hand (see [`crate::immutable::remove`]).
//!
//! # Migration
//!
//! Existing installs are healed by the next transaction: [`ensure_in_target`]
//! seeds the new set/slot's [`DB_DIR`] from the shared database and adds the
//! fstab entry, **inside the set**. The running system's `/var/lib/pacman` is
//! not touched, so nothing changes underneath it and there is no window where
//! the machine has no database. Rolling back to a set from before the migration
//! still works: it has no fstab entry, so it uses the shared database as it
//! always did.

use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::os::unix::fs::MetadataExt;
use tracing::{info, warn};

/// Where the database really lives: inside `/usr`, so it is part of the
/// snapshot set (btrfs) or the verity-sealed root slot (LVM A/B).
pub const DB_DIR: &str = "/usr/lib/sysimage/pacman";

/// Where pacman looks for it. Kept as the stock path so no tool needs
/// reconfiguring — the bind mount below is the whole mechanism.
pub const DB_MOUNT: &str = "/var/lib/pacman";

/// The fstab entry that makes [`DB_DIR`] appear at [`DB_MOUNT`] on every boot.
///
/// `nofail` so that booting a snapshot set created before this existed — which
/// has no [`DB_DIR`] — is a missing bind rather than a failed `mount -a`. Such
/// a set falls back to the shared database, exactly as it did when it was made.
pub fn fstab_entry() -> String {
    format!(
        "\n# pacman database. It lives inside /usr (the snapshotted / sealed image)\n\
         # so that it rolls back with the files it describes, and is bind-mounted\n\
         # back onto the stock path. See src/immutable/pacman_db.rs.\n\
         {DB_DIR}  {DB_MOUNT}  none  bind,nofail  0  0\n"
    )
}

/// Whether `root`'s fstab already carries the bind entry.
fn fstab_has_entry(root: &str) -> bool {
    std::fs::read_to_string(format!("{root}/etc/fstab"))
        .map(|c| {
            c.lines().any(|l| {
                let l = l.trim_start();
                !l.starts_with('#') && l.split_whitespace().next() == Some(DB_DIR)
            })
        })
        .unwrap_or(false)
}

/// Append the bind entry to `<root>/etc/fstab`, unless it is already there.
/// `root` is `""` for the live system, or a mounted set/slot.
///
/// Best-effort like [`crate::immutable::etc::repair_fstab`]: a system whose
/// fstab cannot be written still has a working database at [`DB_MOUNT`], it
/// just does not gain the per-image one until this succeeds.
pub fn add_fstab_entry(cmd: &CommandRunner, root: &str) {
    if cmd.is_dry_run() {
        println!("  [dry-run] Would add the pacman-db bind to {root}/etc/fstab");
        return;
    }
    if fstab_has_entry(root) {
        return;
    }
    let path = format!("{root}/etc/fstab");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if let Err(e) = std::fs::write(&path, format!("{existing}{}", fstab_entry())) {
        warn!("[immutable] Could not add the pacman-db bind to {path}: {e}");
    } else {
        info!("[immutable] Added the pacman-db bind mount to {path}");
    }
}

/// Shell that binds `<root>`'s [`DB_DIR`] over its [`DB_MOUNT`].
///
/// Both directories are created first: at install time neither exists yet, and
/// inside an update chroot [`DB_MOUNT`] belongs to the rbound shared `/var` and
/// so still shows the *live* database until this covers it.
///
/// A `db.lck` is cleared on the way in. pacman's lock now lives *inside* the
/// image, so one left behind by an interrupted transaction would be snapshotted
/// into every set descended from it and make each one refuse to install
/// anything ("unable to lock database"). Nothing else can hold it here: the
/// caller holds the deploytix transaction lock and the set was snapshotted a
/// moment ago, so any lock file present is a fossil.
pub fn bind_cmd(root: &str) -> String {
    format!(
        "set -e; mkdir -p \"{root}{DB_DIR}\" \"{root}{DB_MOUNT}\"; \
         rm -f \"{root}{DB_DIR}/db.lck\"; \
         mount --bind \"{root}{DB_DIR}\" \"{root}{DB_MOUNT}\""
    )
}

/// Shell that seeds `<root>`'s [`DB_DIR`] from the database currently visible
/// at its [`DB_MOUNT`], for a system installed before this module existed.
///
/// Runs before [`bind_cmd`], so [`DB_MOUNT`] is still the shared `/var` copy —
/// which is precisely the database the new set should start from. The copy is
/// `--reflink=auto`, so on btrfs it is instant and free until one side changes;
/// elsewhere it falls back to a normal copy rather than failing.
///
/// Copy, never move: the running system keeps its database exactly where it is.
pub fn seed_cmd(root: &str) -> String {
    format!(
        "set -e; mkdir -p \"{root}{DB_DIR}\"; \
         cp -a --reflink=auto \"{root}{DB_MOUNT}/.\" \"{root}{DB_DIR}/\""
    )
}

/// Whether the bind is actually in place under `<root>` — i.e. whether writing
/// [`DB_MOUNT`] there writes the image's own database rather than the shared
/// `/var` one.
///
/// Asks the precise question by inode rather than inferring it from
/// [`is_migrated`], which only says the directory has been seeded: a seed that
/// succeeded and a bind that then failed would otherwise look like a per-image
/// database while pacman wrote straight through to the shared `/var`.
pub fn target_uses_own_db(root: &str) -> bool {
    let same = |a: &str, b: &str| match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    };
    same(&format!("{root}{DB_DIR}"), &format!("{root}{DB_MOUNT}"))
}

/// Whether `<root>` already keeps its database inside the image.
///
/// Tests for `local/`, the directory that holds the installed-package entries:
/// [`DB_DIR`] itself can exist and be empty (a partial migration, or a set
/// snapshotted mid-transaction), and treating that as migrated would bind an
/// empty database over a good one.
pub fn is_migrated(root: &str) -> bool {
    std::path::Path::new(&format!("{root}{DB_DIR}/local")).is_dir()
}

/// Put the per-image database in place under `install_root`, for a fresh
/// install. Call **before** basestrap, once `/usr` and `/var` are mounted, so
/// the base system's database is written into [`DB_DIR`] from the start and
/// every later chroot transaction of the install writes there too.
pub fn create_and_bind(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("[immutable] Placing the pacman database inside the image ({DB_DIR})");
    if cmd.is_dry_run() {
        println!("  [dry-run] Would bind {install_root}{DB_DIR} -> {install_root}{DB_MOUNT}");
        return Ok(());
    }
    cmd.run("sh", &["-c", &bind_cmd(install_root)])?;
    Ok(())
}

/// Make `target` — a snapshot set or A/B slot mounted for a transaction — use
/// its own database, migrating an older install on the way.
///
/// Returns whether the target ended up with a per-image database. `false` means
/// the transaction runs against the shared `/var/lib/pacman`, exactly as it did
/// before this module existed, so a failure here degrades rather than breaks.
///
/// Ordering matters: seed while [`DB_MOUNT`] still shows the shared database,
/// then bind, then record the entry in the set's own fstab.
pub fn ensure_in_target(cmd: &CommandRunner, target: &str) -> bool {
    if cmd.is_dry_run() {
        println!("  [dry-run] Would give {target} its own pacman database at {DB_DIR}");
        return true;
    }

    if !is_migrated(target) {
        info!(
            "[immutable] Moving the pacman database into the image for {} \
             (the shared /var copy is left untouched)",
            target
        );
        if let Err(e) = cmd.run("sh", &["-c", &seed_cmd(target)]) {
            warn!(
                "[immutable] Could not seed the per-image pacman database in {}: {}. \
                 This transaction will use the shared /var/lib/pacman, which does not \
                 roll back with the system.",
                target, e
            );
            return false;
        }
    }

    if let Err(e) = cmd.run("sh", &["-c", &bind_cmd(target)]) {
        warn!(
            "[immutable] Could not bind the per-image pacman database in {}: {}. \
             This transaction will use the shared /var/lib/pacman.",
            target, e
        );
        return false;
    }

    add_fstab_entry(cmd, target);
    // ...and to the live fstab as well. On the LVM A/B backend `/etc` is one
    // overlay shared by both slots, so the entry has to be written there to
    // take effect at all; on btrfs the set already inherited a copy and this
    // only annotates the running set, whose own `/usr` has no DB_DIR — which
    // `nofail` turns into a skipped mount rather than a failed `mount -a`.
    add_fstab_entry(cmd, "");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The whole fix in one line: the database has to sit inside the part of
    /// the tree that gets snapshotted, and appear at the path pacman uses.
    #[test]
    fn the_database_lives_in_usr_and_surfaces_at_the_stock_path() {
        assert!(
            DB_DIR.starts_with("/usr/"),
            "{DB_DIR} is not inside the image, so it would not roll back"
        );
        assert_eq!(DB_MOUNT, "/var/lib/pacman", "pacman's default DBPath");
    }

    #[test]
    fn the_fstab_entry_is_a_bind_that_cannot_block_a_boot() {
        let e = fstab_entry();
        let line = e
            .lines()
            .find(|l| l.starts_with(DB_DIR))
            .expect("no entry line");
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields[0], DB_DIR);
        assert_eq!(fields[1], DB_MOUNT);
        assert_eq!(fields[3], "bind,nofail");
        // A set made before this existed has no DB_DIR; `nofail` keeps that a
        // skipped mount rather than a failed `mount -a`.
        assert!(fields[3].contains("nofail"));
    }

    #[test]
    fn bind_and_seed_are_valid_shell_and_target_the_right_paths() {
        let bind = bind_cmd("/run/deploytix-update/42");
        assert!(bind.contains("mount --bind \"/run/deploytix-update/42/usr/lib/sysimage/pacman\""));
        assert!(bind.contains("\"/run/deploytix-update/42/var/lib/pacman\""));
        assert_valid_shell(&bind);

        let seed = seed_cmd("/run/deploytix-update/42");
        assert_valid_shell(&seed);
    }

    /// The migration must never take the database away from the running
    /// system: it is still using it, and would be left with none until reboot.
    #[test]
    fn seeding_copies_and_never_moves() {
        let seed = seed_cmd("/mnt");
        assert!(seed.contains("cp -a --reflink=auto"), "{seed}");
        assert!(!seed.contains("mv "), "seeding must not move: {seed}");
    }

    /// An empty DB_DIR is not a migrated system. Binding it over a good
    /// database would leave pacman believing nothing is installed.
    #[test]
    fn migration_is_detected_by_the_local_db_not_the_directory() {
        let dir = std::env::temp_dir().join(format!("deploytix-pacdb-{}", std::process::id()));
        let root = dir.to_string_lossy().to_string();
        let _ = std::fs::remove_dir_all(&dir);

        std::fs::create_dir_all(format!("{root}{DB_DIR}")).unwrap();
        assert!(!is_migrated(&root), "an empty {DB_DIR} is not migrated");

        std::fs::create_dir_all(format!("{root}{DB_DIR}/local")).unwrap();
        assert!(is_migrated(&root));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// pacman's lock now lives inside the image, so a fossil `db.lck` would be
    /// inherited by every set snapshotted from this one and make each of them
    /// refuse to install anything.
    #[test]
    fn binding_clears_a_fossil_lock_file() {
        let bind = bind_cmd("/run/deploytix-update/42");
        assert!(
            bind.contains("rm -f \"/run/deploytix-update/42/usr/lib/sysimage/pacman/db.lck\""),
            "{bind}"
        );
        assert_valid_shell(&bind);
    }

    /// A seeded directory is not a working bind. Reporting one as the other is
    /// what would let `deploytix remove` skip its safety copy while pacman
    /// wrote straight through to the shared /var.
    #[test]
    fn using_the_own_db_is_decided_by_the_mount_not_the_directory() {
        let dir = std::env::temp_dir().join(format!("deploytix-pacdb-m{}", std::process::id()));
        let root = dir.to_string_lossy().to_string();
        let _ = std::fs::remove_dir_all(&dir);

        std::fs::create_dir_all(format!("{root}{DB_DIR}/local")).unwrap();
        std::fs::create_dir_all(format!("{root}{DB_MOUNT}")).unwrap();
        assert!(is_migrated(&root), "seeded");
        assert!(
            !target_uses_own_db(&root),
            "two distinct directories are not a bind"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_is_safe() {
        let cmd = CommandRunner::new(true);
        create_and_bind(&cmd, "/mnt").unwrap();
        assert!(ensure_in_target(&cmd, "/run/deploytix-update/42"));
    }
}
