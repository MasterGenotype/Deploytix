//! Per-set kernel archives on the shared `/boot` partition.
//!
//! `/boot` is a separate, non-snapshotted partition, so the kernel and initramfs
//! it holds are shared by every snapshot set while `/usr/lib/modules/<ver>` lives
//! *inside* each set. Nothing else reconciles the two, which leaves two holes:
//!
//! - **A failed update leaves the new kernel behind.** The kernel package's
//!   install hook and `mkinitcpio` write through the rbind-mounted `/boot`
//!   during the pacman transaction. If a later step fails the set is discarded,
//!   but `/boot` keeps the new `vmlinuz`/`initramfs` — and the set that still
//!   boots has no matching `/usr/lib/modules`.
//! - **Rollback restores userspace but not the kernel.** Moving the boot pointer
//!   to an older set boots it against whatever kernel was installed last.
//!
//! So each set keeps its own copy of the kernel images it was built with under
//! [`SETS_SUBDIR`], and every boot-pointer move restores that copy over the
//! canonical `/boot/vmlinuz-*` / `/boot/initramfs-*` names. The invariant is:
//!
//! > the canonical kernel images always match the set the pointer selects.
//!
//! Only the canonical names are ever booted — GRUB's `10_linux` and grub-btrfs
//! both glob the *top level* of `/boot`, so the nested archive directories are
//! invisible to them and no menu entry has to be hand-written. `/boot/efi` and
//! `/boot/grub` are untouched for the same reason.

use crate::immutable::boot::pointer_set_id;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::path::Path;
use tracing::{info, warn};

/// The boot partition's mount point.
pub const BOOT_ROOT: &str = "/boot";

/// Directory under the boot partition holding the per-set archives.
pub const SETS_SUBDIR: &str = "deploytix";

/// Archive name for the pristine base install (`@`), which is not a set.
pub const BASE_NAME: &str = "base";

/// Archive name for a boot pointer: a set's id, or [`BASE_NAME`] for `@`.
///
/// Anything else (a snapper snapshot booted from the grub-btrfs menu) has no
/// archive of its own and keeps whatever kernel is canonical.
pub fn archive_name(root_subvol: &str) -> String {
    pointer_set_id(root_subvol).unwrap_or_else(|| BASE_NAME.to_string())
}

/// Path of one archive directory.
pub fn archive_dir(boot_root: &str, name: &str) -> String {
    format!("{boot_root}/{SETS_SUBDIR}/{name}")
}

/// Whether an archive exists for `name`.
pub fn has_archive(boot_root: &str, name: &str) -> bool {
    Path::new(&archive_dir(boot_root, name)).is_dir()
}

/// Shell that copies the canonical kernel images into `name`'s archive.
///
/// Built under a temporary name and swapped in, so an interrupted archive
/// leaves the previous one intact rather than a half-written directory that a
/// later restore would treat as good. `--reflink=auto` makes this free on a
/// btrfs `/boot` and a plain copy elsewhere.
pub fn archive_cmd(boot_root: &str, name: &str) -> String {
    format!(
        "set -e; b={boot_root}; d=$b/{SETS_SUBDIR}/{name}; \
         rm -rf \"$d.new\" \"$d.old\"; mkdir -p \"$d.new\"; n=0; \
         for f in \"$b\"/vmlinuz-* \"$b\"/initramfs-*; do \
             [ -f \"$f\" ] || continue; \
             cp -a --reflink=auto \"$f\" \"$d.new/$(basename \"$f\")\"; \
             n=$((n+1)); \
         done; \
         if [ \"$n\" -eq 0 ]; then rm -rf \"$d.new\"; \
             echo \"no kernel images in $b\" >&2; exit 1; fi; \
         sync; \
         if [ -d \"$d\" ]; then mv \"$d\" \"$d.old\"; fi; \
         mv \"$d.new\" \"$d\"; rm -rf \"$d.old\""
    )
}

/// Shell that restores `name`'s archived kernel images over the canonical ones.
///
/// Each file lands via a rename, so a kernel image is never seen half-written.
/// Canonical images the archive does not carry are removed: they belong to a
/// kernel this set never had, and GRUB would offer them an entry whose modules
/// are missing from `/usr/lib/modules`.
pub fn restore_cmd(boot_root: &str, name: &str) -> String {
    format!(
        "set -e; b={boot_root}; d=$b/{SETS_SUBDIR}/{name}; n=0; \
         for f in \"$d\"/vmlinuz-* \"$d\"/initramfs-*; do \
             [ -f \"$f\" ] || continue; \
             k=$(basename \"$f\"); \
             cp -a --reflink=auto \"$f\" \"$b/.dpx-$k.new\"; \
             mv -f \"$b/.dpx-$k.new\" \"$b/$k\"; \
             n=$((n+1)); \
         done; \
         if [ \"$n\" -eq 0 ]; then echo \"empty archive $d\" >&2; exit 1; fi; \
         for f in \"$b\"/vmlinuz-* \"$b\"/initramfs-*; do \
             [ -f \"$f\" ] || continue; \
             [ -f \"$d/$(basename \"$f\")\" ] || rm -f \"$f\"; \
         done; \
         sync"
    )
}

/// Shell that removes `name`'s archive (and any interrupted leftovers).
pub fn remove_cmd(boot_root: &str, name: &str) -> String {
    format!("d={boot_root}/{SETS_SUBDIR}/{name}; rm -rf \"$d\" \"$d.new\" \"$d.old\"; true")
}

/// Archive the canonical kernel images as `name`'s.
///
/// Called once a set is fully built, so the archive records exactly the kernel
/// that set's `/usr/lib/modules` matches.
pub fn archive(cmd: &CommandRunner, boot_root: &str, name: &str) -> Result<()> {
    info!("[immutable] Archiving kernel images for {}", name);
    cmd.run("sh", &["-c", &archive_cmd(boot_root, name)])?;
    Ok(())
}

/// Restore `name`'s archived kernel images over the canonical ones.
///
/// `Ok(false)` when there is no archive — a system installed before archives
/// existed, or a pointer that is not a set. The caller keeps going: moving the
/// pointer without the matching kernel is worse than not moving it only in that
/// it is silent, so it warns instead.
pub fn restore(cmd: &CommandRunner, boot_root: &str, name: &str) -> Result<bool> {
    if cmd.is_dry_run() {
        println!("  [dry-run] Would restore kernel images from {SETS_SUBDIR}/{name}");
        return Ok(true);
    }
    if !has_archive(boot_root, name) {
        warn!(
            "[immutable] No archived kernel for {} — booting it will use the kernel \
             currently in {} (modules may not match)",
            name, boot_root
        );
        return Ok(false);
    }
    info!("[immutable] Restoring kernel images for {}", name);
    cmd.run("sh", &["-c", &restore_cmd(boot_root, name)])?;
    Ok(true)
}

/// Remove archives that no longer belong to a live set, and any leftovers from
/// an interrupted [`archive`].
///
/// `/boot` is a small partition (2 GiB by default) and an archive costs a
/// kernel plus its two initramfs images, so a leaked one is worth sweeping.
/// Sets can vanish without passing through [`remove`]: a delete that failed
/// partway, a system that predates archives, manual surgery.
pub fn prune_orphans(cmd: &CommandRunner, boot_root: &str, live_sets: &[String]) {
    if cmd.is_dry_run() {
        return;
    }
    let dir = format!("{boot_root}/{SETS_SUBDIR}");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        // A `.new`/`.old` directory is always debris from an interrupted run,
        // even when the archive it belongs to is live — so match it first, and
        // never route it through `remove`, which would take the good one too.
        if name.ends_with(".new") || name.ends_with(".old") {
            let _ = cmd.run("sh", &["-c", &format!("rm -rf \"{dir}/{name}\"")]);
            continue;
        }
        if name == BASE_NAME || live_sets.contains(&name) {
            continue;
        }
        warn!("[immutable] Removing orphaned kernel archive {}", name);
        remove(cmd, boot_root, &name);
    }
}

/// Remove `name`'s archive, best-effort — it follows its set out of existence.
pub fn remove(cmd: &CommandRunner, boot_root: &str, name: &str) {
    let _ = cmd.run("sh", &["-c", &remove_cmd(boot_root, name)]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_sweep_keeps_live_sets_and_the_base() {
        let cmd = CommandRunner::new(false);
        let boot = fake_boot("orphans");
        write_kernel(&boot, "6.1");
        for name in [BASE_NAME, "1", "2", "3"] {
            archive(&cmd, &boot, name).unwrap();
        }
        // Debris from a run that died mid-archive, alongside a live archive.
        std::fs::create_dir_all(archive_dir(&boot, "2.new")).unwrap();
        std::fs::create_dir_all(archive_dir(&boot, "9.old")).unwrap();

        // Set 3 was pruned away; 1 and 2 are still live.
        prune_orphans(&cmd, &boot, &["1".to_string(), "2".to_string()]);

        assert!(has_archive(&boot, BASE_NAME), "the base is never pruned");
        assert!(has_archive(&boot, "1"));
        assert!(
            has_archive(&boot, "2"),
            "a live archive survives its own debris"
        );
        assert!(!has_archive(&boot, "3"));
        assert!(!has_archive(&boot, "2.new"));
        assert!(!has_archive(&boot, "9.old"));
        let _ = std::fs::remove_dir_all(&boot);
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

    /// A throwaway `/boot` with the canonical images a kernel install leaves.
    fn fake_boot(tag: &str) -> String {
        let dir =
            std::env::temp_dir().join(format!("deploytix-bootset-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // /boot/efi and /boot/grub must survive untouched.
        std::fs::create_dir_all(dir.join("efi/EFI/artix")).unwrap();
        std::fs::create_dir_all(dir.join("grub")).unwrap();
        std::fs::write(dir.join("grub/grub.cfg"), "menuentry {}\n").unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn write_kernel(boot: &str, version: &str) {
        std::fs::write(
            format!("{boot}/vmlinuz-linux-zen"),
            format!("kernel {version}"),
        )
        .unwrap();
        std::fs::write(
            format!("{boot}/initramfs-linux-zen.img"),
            format!("initramfs {version}"),
        )
        .unwrap();
        std::fs::write(
            format!("{boot}/initramfs-linux-zen-fallback.img"),
            format!("fallback {version}"),
        )
        .unwrap();
    }

    fn canonical_kernel(boot: &str) -> String {
        std::fs::read_to_string(format!("{boot}/vmlinuz-linux-zen")).unwrap()
    }

    #[test]
    fn archive_name_maps_pointers_to_archives() {
        assert_eq!(archive_name("@deploytix-sets/7/root"), "7");
        // The pristine base is not a set but still needs its own kernel kept.
        assert_eq!(archive_name("@"), BASE_NAME);
        // A snapper snapshot has no archive of its own; it reads as the base
        // name and `restore` reports whether one actually exists.
        assert_eq!(archive_name("@snapshots/1/snapshot"), BASE_NAME);
    }

    #[test]
    fn scripts_are_valid_shell_and_scoped_to_kernel_images() {
        for s in [
            archive_cmd("/boot", "7"),
            restore_cmd("/boot", "7"),
            remove_cmd("/boot", "7"),
        ] {
            assert_valid_shell(&s);
            // Only top-level kernel images are in scope — never efi/ or grub/.
            assert!(!s.contains("/efi"), "must not touch the ESP: {s}");
            assert!(!s.contains("grub"), "must not touch grub: {s}");
        }
    }

    #[test]
    fn a_rollback_gets_back_the_kernel_it_was_built_with() {
        let cmd = CommandRunner::new(false);
        let boot = fake_boot("rollback");

        // Set 1 is built and archived.
        write_kernel(&boot, "6.1");
        archive(&cmd, &boot, "1").unwrap();

        // An update installs 6.2 over the shared /boot and archives it as set 2.
        write_kernel(&boot, "6.2");
        archive(&cmd, &boot, "2").unwrap();
        assert_eq!(canonical_kernel(&boot), "kernel 6.2");

        // Rolling back to set 1 brings its own kernel back, not 6.2.
        assert!(restore(&cmd, &boot, "1").unwrap());
        assert_eq!(canonical_kernel(&boot), "kernel 6.1");
        assert_eq!(
            std::fs::read_to_string(format!("{boot}/initramfs-linux-zen.img")).unwrap(),
            "initramfs 6.1"
        );

        // ...and forward again.
        assert!(restore(&cmd, &boot, "2").unwrap());
        assert_eq!(canonical_kernel(&boot), "kernel 6.2");

        // The ESP and grub.cfg were never in scope.
        assert!(std::path::Path::new(&format!("{boot}/efi/EFI/artix")).is_dir());
        assert_eq!(
            std::fs::read_to_string(format!("{boot}/grub/grub.cfg")).unwrap(),
            "menuentry {}\n"
        );
        let _ = std::fs::remove_dir_all(&boot);
    }

    #[test]
    fn a_failed_update_cannot_leave_the_new_kernel_behind() {
        let cmd = CommandRunner::new(false);
        let boot = fake_boot("failed");

        // The running set's kernel, archived when it was built.
        write_kernel(&boot, "6.1");
        archive(&cmd, &boot, "1").unwrap();

        // An update's pacman hook writes 6.2 through the shared /boot, then a
        // later step fails: the set is discarded, so the pointer still selects
        // set 1 — whose /usr/lib/modules only has 6.1.
        write_kernel(&boot, "6.2");
        assert!(restore(&cmd, &boot, "1").unwrap());
        assert_eq!(canonical_kernel(&boot), "kernel 6.1");
        let _ = std::fs::remove_dir_all(&boot);
    }

    #[test]
    fn restoring_drops_kernels_the_set_never_had() {
        let cmd = CommandRunner::new(false);
        let boot = fake_boot("stale");

        write_kernel(&boot, "6.1");
        archive(&cmd, &boot, "1").unwrap();

        // A later set added a second kernel; rolling back must not leave an
        // entry whose modules are missing from the restored set.
        std::fs::write(format!("{boot}/vmlinuz-linux-lts"), "lts").unwrap();
        std::fs::write(format!("{boot}/initramfs-linux-lts.img"), "lts").unwrap();
        assert!(restore(&cmd, &boot, "1").unwrap());
        assert!(!std::path::Path::new(&format!("{boot}/vmlinuz-linux-lts")).exists());
        assert!(std::path::Path::new(&format!("{boot}/vmlinuz-linux-zen")).exists());
        let _ = std::fs::remove_dir_all(&boot);
    }

    #[test]
    fn a_missing_archive_warns_instead_of_failing() {
        let cmd = CommandRunner::new(false);
        let boot = fake_boot("missing");
        write_kernel(&boot, "6.1");

        // Systems installed before archives existed have none: moving the
        // pointer must still work, keeping the current kernel.
        assert!(!restore(&cmd, &boot, "99").unwrap());
        assert_eq!(canonical_kernel(&boot), "kernel 6.1");
        let _ = std::fs::remove_dir_all(&boot);
    }

    #[test]
    fn an_interrupted_archive_leaves_the_previous_one_usable() {
        let cmd = CommandRunner::new(false);
        let boot = fake_boot("interrupted");

        write_kernel(&boot, "6.1");
        archive(&cmd, &boot, "1").unwrap();

        // Simulate a run that died midway: a stray .new directory. The good
        // archive is still the one `restore` reads, and the next archive
        // clears the leftover.
        std::fs::create_dir_all(archive_dir(&boot, "1.new")).unwrap();
        std::fs::write(
            format!("{}/vmlinuz-linux-zen", archive_dir(&boot, "1.new")),
            "torn",
        )
        .unwrap();

        write_kernel(&boot, "6.2");
        assert!(restore(&cmd, &boot, "1").unwrap());
        assert_eq!(canonical_kernel(&boot), "kernel 6.1");

        archive(&cmd, &boot, "1").unwrap();
        assert!(!std::path::Path::new(&archive_dir(&boot, "1.new")).exists());
        let _ = std::fs::remove_dir_all(&boot);
    }

    #[test]
    fn archiving_an_empty_boot_fails_rather_than_recording_nothing() {
        let cmd = CommandRunner::new(false);
        let boot = fake_boot("empty");
        // An empty archive would silently "restore" nothing later.
        assert!(archive(&cmd, &boot, "1").is_err());
        assert!(!has_archive(&boot, "1"));
        let _ = std::fs::remove_dir_all(&boot);
    }

    #[test]
    fn remove_takes_the_archive_and_its_leftovers() {
        let cmd = CommandRunner::new(false);
        let boot = fake_boot("remove");
        write_kernel(&boot, "6.1");
        archive(&cmd, &boot, "1").unwrap();
        assert!(has_archive(&boot, "1"));
        remove(&cmd, &boot, "1");
        assert!(!has_archive(&boot, "1"));
        // Removing a set that has none is a no-op, not an error.
        remove(&cmd, &boot, "1");
        let _ = std::fs::remove_dir_all(&boot);
    }
}
