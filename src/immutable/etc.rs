//! Creation and mounting of the writable `@etc` subvolume.
//!
//! Under the immutable model the OS root (`@`) is read-only, so `/etc` cannot
//! live inside it. Instead `/etc` is a dedicated `@etc` subvolume on the root
//! btrfs filesystem, mounted **before** basestrap so that the base system's
//! `/etc` is written straight into it. `@etc` is later snapshotted together with
//! `@` and `@usr` so configuration rolls back with the system.

use crate::immutable::ETC_SUBVOL;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use tracing::info;

/// Mount options for the writable `@etc` subvolume (rw, unlike `@`/`@usr`).
const ETC_MOUNT_OPTS: &str = "subvol=@etc,rw,noatime,compress=zstd";

/// Shell command that creates the top-level `@etc` subvolume on
/// `root_fs_device` (mounted by `subvolid=5`, the filesystem root), idempotently.
///
/// Kept as a standalone command (like `@snapshots`/`@overlay`) so it can run in
/// the chroot at install time or directly during migration.
pub fn create_etc_subvolume_cmd(root_fs_device: &str) -> String {
    // Use a dedicated mountpoint under /run (tmpfs, always available) rather
    // than /mnt: this command runs on the host during install and on the live
    // system during migration, where /mnt may already be in use.
    format!(
        "m=/run/deploytix-etc-setup && mkdir -p $m && \
         mount -t btrfs -o subvolid=5 {dev} $m && \
         test -e $m/@etc || btrfs subvolume create $m/@etc; \
         ret=$?; umount $m; rmdir $m 2>/dev/null; exit $ret",
        dev = root_fs_device
    )
}

/// Create `@etc` on `root_fs_device` and mount it at `<install_root>/etc`.
///
/// Call this after the root subvolume (`@`) is mounted at `install_root` but
/// **before** basestrap, so the freshly installed `/etc` lands in `@etc`.
/// `root_fs_device` is the block device carrying the root btrfs (e.g.
/// `/dev/mapper/Crypt-Root`, or the ROOT partition for single-partition btrfs).
pub fn create_and_mount_etc(
    cmd: &CommandRunner,
    root_fs_device: &str,
    install_root: &str,
) -> Result<()> {
    info!(
        "[immutable] Creating and mounting writable {} subvolume for /etc",
        ETC_SUBVOL
    );

    // 1. Create the subvolume at the filesystem root (idempotent).
    cmd.run("sh", &["-c", &create_etc_subvolume_cmd(root_fs_device)])?;

    // 2. Mount it over <install_root>/etc (the empty /etc dir inside @).
    let etc_mount = format!("{}/etc", install_root);
    if !cmd.is_dry_run() {
        std::fs::create_dir_all(&etc_mount)?;
    }
    cmd.run(
        "mount",
        &[
            "-t",
            "btrfs",
            "-o",
            ETC_MOUNT_OPTS,
            root_fs_device,
            &etc_mount,
        ],
    )?;

    info!("[immutable] Mounted {} at {}", ETC_SUBVOL, etc_mount);
    Ok(())
}

/// Marker prefixed to fstab lines this module disables, so the repair is
/// idempotent and the original line stays readable.
const DISABLED_PREFIX: &str = "# deploytix (initramfs-mounted, see .deploytix-pair): ";

/// Comment out any fstab entry for a mount point the initramfs owns
/// (see [`crate::immutable::INITRAMFS_OWNED_MOUNTPOINTS`]).
///
/// Installs made before this was fixed have `/`, `/usr` and `/etc` lines naming
/// the base `@`/`@usr`/`@etc` in their `@etc` — and every snapshot set
/// inherited a copy. Booting a set then lets `mount -a` mount the base
/// subvolumes over the ones the initramfs mounted, hiding everything
/// `deploytix update` installed. Disabling the lines is enough: the initramfs
/// has already mounted all three by the time fstab is processed.
///
/// Returns `None` when the file already needs no change, so callers can skip
/// the write (and the log line) on an already-correct system.
pub fn sanitize_fstab(contents: &str) -> Option<String> {
    let mut changed = false;
    let mut out = String::with_capacity(contents.len());
    for line in contents.lines() {
        let trimmed = line.trim_start();
        // Blank lines, comments and our own disabled entries pass through, which
        // is what makes repeated repairs a no-op.
        let owned = !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && line
                .split_whitespace()
                .nth(1)
                .is_some_and(crate::immutable::initramfs_owned_mount);
        if owned {
            changed = true;
            out.push_str(DISABLED_PREFIX);
        }
        out.push_str(line);
        out.push('\n');
    }
    changed.then_some(out)
}

/// Apply [`sanitize_fstab`] to `<root>/etc/fstab` in place. `root` is `""` for
/// the live system, or a chroot target for a staged snapshot set.
///
/// Best-effort by design: a missing or unreadable fstab is not a reason to fail
/// an update, so this reports what it did rather than propagating I/O errors.
/// Returns whether the file was rewritten.
pub fn repair_fstab(cmd: &CommandRunner, root: &str) -> bool {
    let path = format!("{root}/etc/fstab");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Some(fixed) = sanitize_fstab(&contents) else {
        return false;
    };
    if cmd.is_dry_run() {
        println!("  [dry-run] Would disable initramfs-owned fstab entries in {path}");
        return false;
    }
    match std::fs::write(&path, fixed) {
        Ok(()) => {
            info!(
                "[immutable] Disabled initramfs-owned /, /usr and /etc entries in {} \
                 (they would shadow a booted snapshot set)",
                path
            );
            true
        }
        Err(e) => {
            tracing::warn!("[immutable] Could not repair {}: {}", path, e);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etc_subvolume_created_idempotently_at_fs_root() {
        let cmd = create_etc_subvolume_cmd("/dev/mapper/Crypt-Root");
        assert!(cmd.contains("mount -t btrfs -o subvolid=5 /dev/mapper/Crypt-Root $m"));
        assert!(cmd.contains("test -e $m/@etc || btrfs subvolume create $m/@etc"));
        assert!(cmd.contains("umount $m"));

        let status = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(&cmd)
            .status();
        if let Ok(status) = status {
            assert!(status.success(), "generated command is not valid shell");
        }
    }

    #[test]
    fn create_and_mount_etc_is_dry_run_safe() {
        // Dry-run must not touch the filesystem or error out.
        let cmd = CommandRunner::new(true);
        create_and_mount_etc(&cmd, "/dev/mapper/Crypt-Root", "/mnt/target").unwrap();
    }

    /// The regression: a legacy fstab naming @/@usr/@etc lets `mount -a` mount
    /// the base subvolumes over a booted snapshot set, so an update's packages
    /// vanish even though pacman installed them.
    #[test]
    fn sanitize_disables_only_the_initramfs_owned_entries() {
        let legacy = "\
# /etc/fstab
UUID=aaa  /  btrfs  subvol=@,defaults,noatime,compress=zstd,ro  0  0
UUID=bbb  /usr  btrfs  subvol=@usr,defaults,noatime,compress=zstd,ro  0  0
UUID=aaa  /etc  btrfs  subvol=@etc,rw,noatime,compress=zstd  0  0
UUID=ccc  /var  btrfs  subvol=@var,defaults,noatime,compress=zstd  0  0
UUID=ddd  /home  btrfs  subvol=@home,defaults,noatime,compress=zstd  0  0
UUID=eee  /boot  btrfs  subvol=@boot,defaults,noatime,compress=zstd  0  0
UUID=aaa  /tmp  btrfs  subvol=@tmp,rw,noatime,compress=zstd  0  0
/var/opt  /opt  none  bind  0  0
";
        let fixed = sanitize_fstab(legacy).expect("legacy fstab must be rewritten");
        for owned in ["subvol=@,", "subvol=@usr,", "subvol=@etc,"] {
            let line = fixed
                .lines()
                .find(|l| l.contains(owned))
                .unwrap_or_else(|| panic!("{owned} line disappeared"));
            assert!(line.starts_with(DISABLED_PREFIX), "still active: {line}");
        }
        // Everything else — including the writable-path binds and /tmp — stays.
        for kept in ["/var ", "/home ", "/boot ", "/tmp ", "/opt "] {
            let line = fixed
                .lines()
                .find(|l| l.contains(kept))
                .unwrap_or_else(|| panic!("{kept} line disappeared"));
            assert!(
                !line.starts_with(DISABLED_PREFIX),
                "wrongly disabled: {line}"
            );
        }
    }

    #[test]
    fn sanitize_is_idempotent_and_skips_correct_files() {
        let good = "UUID=ccc  /var  btrfs  subvol=@var,defaults  0  0\n";
        assert!(sanitize_fstab(good).is_none());
        let legacy = "UUID=bbb  /usr  btrfs  subvol=@usr,ro  0  0\n";
        let once = sanitize_fstab(legacy).unwrap();
        assert!(
            sanitize_fstab(&once).is_none(),
            "second pass must be a no-op"
        );
    }

    #[test]
    fn repair_is_dry_run_safe_and_tolerates_a_missing_fstab() {
        let cmd = CommandRunner::new(true);
        assert!(!repair_fstab(&cmd, "/nonexistent/deploytix-test-root"));
    }
}
