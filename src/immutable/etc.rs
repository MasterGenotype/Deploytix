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
}
