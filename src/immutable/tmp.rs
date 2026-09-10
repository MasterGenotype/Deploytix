//! Disk-backed `/tmp` for the immutable (read-only root) model.
//!
//! `/` is a plain read-only btrfs mount with no overlay, so `/tmp` cannot be a
//! directory inside `@`. A half-RAM tmpfs is the wrong default for builds that
//! need multi-gigabyte scratch (linux-tkg, etc.). Instead install-time creates
//! a top-level `@tmp` subvolume on the root btrfs and mounts it at `/tmp`.
//!
//! See `docs/TMP_DISK_BACKED.md`.

use crate::immutable::TMP_SUBVOL;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::os::unix::fs::PermissionsExt;
use tracing::info;

/// Mount options for the writable `@tmp` subvolume.
const TMP_MOUNT_OPTS: &str = "subvol=@tmp,rw,noatime,compress=zstd";

/// Relative path of the boot-wipe tmpfiles drop-in under the install root.
pub const TMPFILES_REL: &str = "etc/tmpfiles.d/deploytix-tmp.conf";

/// Contents of [`TMPFILES_REL`]. `D!` wipes `/tmp` each boot so disk-backed
/// scratch stays ephemeral without consuming RAM.
const TMPFILES_CONTENTS: &str = "\
# Deploytix: disk-backed /tmp (btrfs @tmp). Wipe contents each boot so behavior
# stays close to classic volatile /tmp without a RAM backend. Mode 1777 sticky.
# See docs/TMP_DISK_BACKED.md
D! /tmp 1777 root root 0
";

/// Shell that creates top-level `@tmp` on `root_fs_device` (via `subvolid=5`),
/// idempotently, and sets mode 1777.
pub fn create_tmp_subvolume_cmd(root_fs_device: &str) -> String {
    format!(
        "m=/run/deploytix-tmp-setup && mkdir -p $m && \
         mount -t btrfs -o subvolid=5 {dev} $m && \
         if test ! -e $m/@tmp; then btrfs subvolume create $m/@tmp; fi && \
         chmod 1777 $m/@tmp && \
         ret=$?; umount $m; rmdir $m 2>/dev/null; exit $ret",
        dev = root_fs_device
    )
}

/// Create `@tmp` on `root_fs_device` and mount it at `<install_root>/tmp`.
///
/// Call after the root subvolume is mounted at `install_root`, alongside
/// `@etc` setup, so the install chroot already has a writable disk `/tmp`.
pub fn create_and_mount_tmp(
    cmd: &CommandRunner,
    root_fs_device: &str,
    install_root: &str,
) -> Result<()> {
    info!(
        "[immutable] Creating and mounting disk-backed {} subvolume for /tmp",
        TMP_SUBVOL
    );

    cmd.run("sh", &["-c", &create_tmp_subvolume_cmd(root_fs_device)])?;

    let tmp_mount = format!("{}/tmp", install_root);
    if !cmd.is_dry_run() {
        std::fs::create_dir_all(&tmp_mount)?;
    }
    cmd.run(
        "mount",
        &[
            "-t",
            "btrfs",
            "-o",
            TMP_MOUNT_OPTS,
            root_fs_device,
            &tmp_mount,
        ],
    )?;
    if !cmd.is_dry_run() {
        std::fs::set_permissions(&tmp_mount, std::fs::Permissions::from_mode(0o1777))?;
    }

    info!("[immutable] Mounted {} at {}", TMP_SUBVOL, tmp_mount);
    Ok(())
}

/// Install the tmpfiles drop-in that boot-wipes `/tmp` on the target.
pub fn install_tmpfiles(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    let path = format!("{install_root}/{TMPFILES_REL}");
    info!("[immutable] Installing {TMPFILES_REL} (boot-wipe disk /tmp)");
    if cmd.is_dry_run() {
        println!("  [dry-run] Would write /{TMPFILES_REL}");
        return Ok(());
    }
    if let Some(parent) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, TMPFILES_CONTENTS)?;
    Ok(())
}

/// fstab line for disk-backed `/tmp` on the root btrfs UUID.
pub fn tmp_fstab_entry(root_fs_uuid: &str) -> String {
    format!("UUID={root_fs_uuid}  /tmp  btrfs  subvol=@tmp,rw,noatime,compress=zstd  0  0\n")
}

// ── LVM A/B backend ─────────────────────────────────────────────────────────
//
// The A/B backend has no btrfs to put a `@tmp` subvolume on: `/` is an ext4/xfs
// image sealed with dm-verity and mounted read-only, and the `verity-ab` hook
// overlays only `/etc`. That left `/tmp` a read-only directory inside the sealed
// image — writable by nothing, which breaks far more than deploytix: makepkg,
// pacman's own scratch files, and the GUIs' single-instance locks all need it.
//
// It gets the same treatment as `/root`, `/opt` and `/srv` on the btrfs backend:
// a real directory on the shared, writable `/var`, bind-mounted into place. Disk
// backed rather than a half-RAM tmpfs, for the reason in `docs/TMP_DISK_BACKED.md`,
// and boot-wiped by the same tmpfiles drop-in as the btrfs backend.

/// Backing directory on the shared `/var` for the A/B backend's `/tmp`.
pub const AB_TMP_SOURCE: &str = "/var/tmp/deploytix-tmp";

/// fstab line binding [`AB_TMP_SOURCE`] onto `/tmp`, for the A/B backend.
pub fn ab_tmp_fstab_entry() -> String {
    format!(
        "# /tmp. The verity root is read-only and has no writable `@tmp` subvolume\n\
         # to offer, so /tmp is a directory on the shared /var bound into place.\n\
         # Disk-backed, not a half-RAM tmpfs; boot-wiped by tmpfiles.d.\n\
         {AB_TMP_SOURCE}  /tmp  none  bind  0  0\n"
    )
}

/// Create [`AB_TMP_SOURCE`] under `install_root` (mode 1777) and bind it over
/// `<install_root>/tmp`, the way the booted system will have it.
///
/// Call once `/var` is mounted and before basestrap, so the rest of the install
/// has a writable `/tmp` inside the target exactly as the booted system does.
pub fn create_and_bind_ab_tmp(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("[immutable] Creating the A/B backend's writable /tmp on the shared /var");
    if cmd.is_dry_run() {
        println!("  [dry-run] Would bind {install_root}{AB_TMP_SOURCE} -> {install_root}/tmp");
        return Ok(());
    }
    let source = format!("{install_root}{AB_TMP_SOURCE}");
    let mount = format!("{install_root}/tmp");
    std::fs::create_dir_all(&source)?;
    std::fs::create_dir_all(&mount)?;
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o1777))?;
    cmd.run("mount", &["--bind", &source, &mount])?;
    std::fs::set_permissions(&mount, std::fs::Permissions::from_mode(0o1777))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmp_subvolume_created_idempotently_at_fs_root() {
        let cmd = create_tmp_subvolume_cmd("/dev/mapper/Crypt-Root");
        assert!(cmd.contains("mount -t btrfs -o subvolid=5 /dev/mapper/Crypt-Root $m"));
        assert!(cmd.contains("@tmp"));
        assert!(cmd.contains("chmod 1777"));
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
    fn create_and_mount_tmp_is_dry_run_safe() {
        let cmd = CommandRunner::new(true);
        create_and_mount_tmp(&cmd, "/dev/mapper/Crypt-Root", "/mnt/target").unwrap();
    }

    #[test]
    fn install_tmpfiles_is_dry_run_safe() {
        let cmd = CommandRunner::new(true);
        install_tmpfiles(&cmd, "/mnt/target").unwrap();
    }

    #[test]
    fn tmp_fstab_entry_is_disk_backed_not_tmpfs() {
        let line = tmp_fstab_entry("9f72ea22-39ab-4a60-8ce0-38a8219c376a");
        assert!(line.contains("subvol=@tmp"));
        assert!(line.contains("/tmp"));
        assert!(!line.contains("tmpfs"));
    }

    /// A read-only /tmp is not a working system: makepkg, pacman's scratch
    /// files and the GUI locks all need to write there, and the sealed A/B root
    /// offers no `@tmp` subvolume to mount instead.
    #[test]
    fn the_ab_backend_gets_a_writable_disk_backed_tmp() {
        let entry = ab_tmp_fstab_entry();
        let line = entry
            .lines()
            .find(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
            .expect("no entry line");
        assert_eq!(
            line.split_whitespace().collect::<Vec<_>>(),
            vec![AB_TMP_SOURCE, "/tmp", "none", "bind", "0", "0"]
        );
        assert!(
            AB_TMP_SOURCE.starts_with("/var/"),
            "the bind source must be on the shared, writable /var"
        );
    }

    #[test]
    fn create_and_bind_ab_tmp_is_dry_run_safe() {
        let cmd = CommandRunner::new(true);
        create_and_bind_ab_tmp(&cmd, "/mnt/target").unwrap();
    }

    #[test]
    fn tmpfiles_contents_boot_wipe_tmp() {
        assert!(TMPFILES_CONTENTS.contains("D! /tmp 1777"));
        assert!(TMPFILES_CONTENTS.contains("docs/TMP_DISK_BACKED.md"));
    }
}
