//! Boot pointer for the transactional immutable model.
//!
//! The "current" system is selected by the `rootflags=subvol=<path>` value in the
//! default GRUB entry. The initramfs `mountcrypt` hook reads that subvol from the
//! kernel cmdline, mounts it read-only as `/`, and consults its `.deploytix-pair`
//! marker for the matching `/usr` and `/etc`.
//!
//! ## Why grub is regenerated in a chroot
//! On a booted immutable system `/` is an **overlayfs**, and `grub-probe` (which
//! `grub-mkconfig` calls) fails with *"failed to get canonical path of
//! `overlay'"* and aborts — producing an empty grub.cfg. So we never run
//! `grub-mkconfig` against the live `/`. Instead [`activate_target`] mounts the
//! target subvolume set at a scratch chroot (a **real** btrfs root, where
//! `grub-probe` works), points that root's `/etc/default/grub` at itself, and
//! runs `grub-mkconfig` there — writing the shared `/boot/grub/grub.cfg`.

use crate::immutable::bootset;
use crate::immutable::snapshot::{self, ImmutableDevices};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use tracing::info;

/// Where grub.cfg lives / is regenerated.
pub const GRUB_CFG: &str = "/boot/grub/grub.cfg";
/// Scratch mountpoint for the grub-regeneration chroot.
const GRUB_CHROOT: &str = "/run/deploytix-grub";

/// The set id embedded in a boot pointer, if it points at a snapshot set.
/// `@deploytix-sets/<id>/root` → `Some(<id>)`; `@` → `None`.
pub fn pointer_set_id(pointer: &str) -> Option<String> {
    let rest = pointer.strip_prefix("@deploytix-sets/")?;
    rest.strip_suffix("/root").map(str::to_string)
}

/// The paired `usr`/`etc` subvolumes for a given root subvolume, by convention:
/// a set uses its sibling `usr`/`etc`; the live `@` uses `@usr`/`@etc`.
fn paired_subvols(root_subvol: &str) -> (String, String) {
    let trio = snapshot::SourceSubvols::for_root(root_subvol);
    (trio.usr, trio.etc)
}

/// The root subvolume the running system actually booted from, read from
/// `/proc/cmdline`.
///
/// [`current_boot_pointer`] reads grub.cfg, which is what will boot *next*: the
/// two differ exactly while an update or rollback is staged but not yet booted,
/// and telling them apart is what keeps a second `deploytix update` in the same
/// session from starting a rival set (and keeps pruning off the set the live
/// system is running from).
///
/// Mirrors `resolve_root_subvol` in the mountcrypt hook: last `rootflags=subvol=`
/// wins, surrounding double quotes stripped (grub-btrfs emits them).
pub fn running_root_subvol() -> String {
    std::fs::read_to_string("/proc/cmdline")
        .ok()
        .and_then(|c| parse_root_subvol(&c))
        .unwrap_or_else(|| crate::immutable::ROOT_SUBVOL.to_string())
}

/// Extract the effective `rootflags=subvol=` value from a kernel cmdline.
fn parse_root_subvol(cmdline: &str) -> Option<String> {
    let mut found = None;
    for arg in cmdline.split_whitespace() {
        let Some(flags) = arg.strip_prefix("rootflags=") else {
            continue;
        };
        for flag in flags.split(',') {
            if let Some(value) = flag.strip_prefix("subvol=") {
                let value = value.trim_matches('"');
                if !value.is_empty() {
                    found = Some(value.to_string());
                }
            }
        }
    }
    found
}

/// `sed` that rewrites the `rootflags=subvol=` pointer in `file`. A `|` delimiter
/// avoids escaping the slashes in set subvolume paths.
fn set_pointer_sed(file: &str, root_subvol: &str) -> String {
    format!("sed -i 's|rootflags=subvol=[^ \"]*|rootflags=subvol={root_subvol}|' {file}")
}

/// Shell that mounts the target subvolume set at [`GRUB_CHROOT`] as a real btrfs
/// root (root/usr read-only, etc read-write for the sed), plus `/.snapshots`,
/// `/boot` and `/var`, so `grub-mkconfig` can run there.
fn mount_grub_chroot_cmd(devices: &ImmutableDevices, root: &str, usr: &str, etc: &str) -> String {
    format!(
        "set -e; t={t}; mkdir -p \"$t\"; \
         mount -t btrfs -o subvol={root},ro,noatime,compress=zstd {rfs} \"$t\"; \
         mkdir -p \"$t/usr\" \"$t/etc\" \"$t/.snapshots\"; \
         mount -t btrfs -o subvol={usr},ro,noatime,compress=zstd {ufs} \"$t/usr\"; \
         mount -t btrfs -o subvol={etc},rw,noatime,compress=zstd {rfs} \"$t/etc\"; \
         mount -t btrfs -o subvol=@snapshots,ro,noatime,compress=zstd {rfs} \"$t/.snapshots\" 2>/dev/null || true; \
         for d in boot var; do mkdir -p \"$t/$d\"; mount --rbind \"/$d\" \"$t/$d\"; done",
        t = GRUB_CHROOT,
        root = root,
        usr = usr,
        etc = etc,
        rfs = devices.root_fs,
        ufs = devices.usr_fs,
    )
}

/// Shell that recursively unmounts and removes the grub chroot.
fn unmount_grub_chroot_cmd() -> String {
    format!("umount -R {GRUB_CHROOT} 2>/dev/null || true; rmdir {GRUB_CHROOT} 2>/dev/null || true")
}

/// Make `root_subvol` the default boot: mount it (and its paired usr/etc) at a
/// scratch chroot, point that root's `/etc/default/grub` at itself, and
/// regenerate `/boot/grub/grub.cfg` from inside the chroot — where `/` is a real
/// btrfs root, so `grub-probe` succeeds. Takes effect on the next reboot.
pub fn activate_target(
    cmd: &CommandRunner,
    devices: &ImmutableDevices,
    root_subvol: &str,
) -> Result<()> {
    let (usr, etc) = paired_subvols(root_subvol);

    // Put this set's own kernel images back under the canonical names before
    // regenerating grub, so the entry `10_linux` writes and the modules in the
    // set's /usr/lib/modules are the same kernel. Every pointer move goes
    // through here, so that holds for updates and rollbacks alike.
    bootset::restore(cmd, bootset::BOOT_ROOT, &bootset::archive_name(root_subvol))?;

    info!(
        "[immutable] Regenerating grub.cfg with default boot = {} (via chroot)",
        root_subvol
    );
    cmd.run(
        "sh",
        &[
            "-c",
            &mount_grub_chroot_cmd(devices, root_subvol, &usr, &etc),
        ],
    )?;
    let result = (|| -> Result<()> {
        cmd.run(
            "sh",
            &[
                "-c",
                &set_pointer_sed(&format!("{GRUB_CHROOT}/etc/default/grub"), root_subvol),
            ],
        )?;
        cmd.run_in_chroot(GRUB_CHROOT, &format!("grub-mkconfig -o {GRUB_CFG}"))?;
        Ok(())
    })();
    let _ = cmd.run("sh", &["-c", &unmount_grub_chroot_cmd()]);
    result
}

/// Read the current `rootflags=subvol=` pointer from the generated grub.cfg (the
/// first menuentry's — i.e. the default). `@` when none is found.
pub fn current_boot_pointer(cmd: &CommandRunner) -> Result<String> {
    if cmd.is_dry_run() {
        return Ok("@".to_string());
    }
    let script = format!(
        "grep -o 'rootflags=subvol=[^ \"]*' {GRUB_CFG} | head -n1 | sed 's/rootflags=subvol=//'"
    );
    let out = cmd.run("sh", &["-c", &script])?;
    let ptr = out
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    Ok(if ptr.is_empty() { "@".to_string() } else { ptr })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_root_subvol_takes_the_last_rootflags_like_the_hook() {
        // Plain boot of the base install.
        assert_eq!(
            parse_root_subvol("root=UUID=x rootflags=subvol=@ rw"),
            Some("@".into())
        );

        // A grub-btrfs snapshot entry appends its own rootflags after the
        // cmdline's; the kernel (and the hook) take the last one.
        assert_eq!(
            parse_root_subvol(
                "rootflags=subvol=@ quiet rootflags=subvol=\"@deploytix-sets/7/root\""
            ),
            Some("@deploytix-sets/7/root".into())
        );

        // Other rootflags survive alongside subvol=.
        assert_eq!(
            parse_root_subvol("rootflags=compress=zstd,subvol=@deploytix-sets/9/root,noatime"),
            Some("@deploytix-sets/9/root".into())
        );

        // Nothing to find → caller falls back to @.
        assert_eq!(parse_root_subvol("root=UUID=x rw quiet"), None);
        assert_eq!(parse_root_subvol("rootflags=subvol="), None);
    }

    fn devices() -> ImmutableDevices {
        ImmutableDevices {
            root_fs: "/dev/mapper/Crypt-Root".into(),
            usr_fs: "/dev/mapper/Crypt-Usr".into(),
        }
    }

    fn assert_valid_shell(s: &str) {
        if let Ok(status) = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(s)
            .status()
        {
            assert!(status.success(), "not valid shell:\n{s}");
        }
    }

    #[test]
    fn pointer_set_id_roundtrips() {
        assert_eq!(
            pointer_set_id("@deploytix-sets/123/root"),
            Some("123".to_string())
        );
        assert_eq!(pointer_set_id("@"), None);
        assert_eq!(pointer_set_id("@deploytix-sets/123/usr"), None);
    }

    #[test]
    fn paired_subvols_by_convention() {
        assert_eq!(
            paired_subvols("@deploytix-sets/9/root"),
            (
                "@deploytix-sets/9/usr".into(),
                "@deploytix-sets/9/etc".into()
            )
        );
        assert_eq!(paired_subvols("@"), ("@usr".into(), "@etc".into()));
    }

    #[test]
    fn grub_chroot_mounts_real_root_and_sed_uses_pipe() {
        let m = mount_grub_chroot_cmd(
            &devices(),
            "@deploytix-sets/9/root",
            "@deploytix-sets/9/usr",
            "@deploytix-sets/9/etc",
        );
        assert!(m.contains("subvol=@deploytix-sets/9/root,ro"));
        assert!(m.contains("subvol=@deploytix-sets/9/etc,rw"));
        assert!(m.contains("mount --rbind \"/$d\""));
        assert_valid_shell(&m);
        let s = set_pointer_sed(
            "/run/deploytix-grub/etc/default/grub",
            "@deploytix-sets/9/root",
        );
        assert!(s.contains("s|rootflags=subvol=[^ \"]*|rootflags=subvol=@deploytix-sets/9/root|"));
        assert_valid_shell(&s);
        assert_valid_shell(&unmount_grub_chroot_cmd());
    }

    #[test]
    fn activate_dry_run_is_safe() {
        let cmd = CommandRunner::new(true);
        activate_target(&cmd, &devices(), "@").unwrap();
    }
}
