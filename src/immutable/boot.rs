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

use crate::configure::bootloader::REINSTALL_GRUB_PATH;
use crate::immutable::snapshot::{self, ImmutableDevices};
use crate::immutable::{detect_devices, PAIR_MARKER};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::path::Path;
use tracing::info;

/// Where grub.cfg lives / is regenerated.
pub const GRUB_CFG: &str = "/boot/grub/grub.cfg";
/// Scratch mountpoint for the grub-regeneration chroot.
const GRUB_CHROOT: &str = "/run/deploytix-grub";

/// How `deploytix regen-grub` regenerates grub.cfg on the running system.
///
/// grub-btrfs's snapshot entries only reach the menu through a regeneration,
/// and the right way to regenerate depends on the layout: the immutable
/// watcher (`deploytix-grub-btrfsd`) and users call `deploytix regen-grub`
/// rather than having to know which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegenStrategy {
    /// Immutable btrfs root: the live `/` is an overlayfs, so mount the
    /// active snapshot set at a scratch chroot and run `grub-mkconfig` there
    /// ([`activate_target`] with the current pointer).
    ImmutableChroot,
    /// Encrypted / standalone layouts: the reinstall-grub pipeline the pacman
    /// hook uses (mkconfig, then grub-install or a standalone rebuild, then
    /// SecureBoot re-signing) — a bare mkconfig would leave an embedded
    /// standalone config stale.
    ReinstallScript,
    /// Plain `grub-mkconfig` against the live root.
    Mkconfig,
}

impl RegenStrategy {
    /// One-line description for logs.
    pub fn describe(self) -> &'static str {
        match self {
            RegenStrategy::ImmutableChroot => {
                "immutable root: grub-mkconfig in a chroot of the active snapshot set"
            }
            RegenStrategy::ReinstallScript => "reinstall-grub pipeline",
            RegenStrategy::Mkconfig => "grub-mkconfig",
        }
    }
}

/// Pick the regeneration strategy from what is present on the running system:
/// the live pairing marker signals an immutable btrfs root, the reinstall
/// script an encrypted/standalone layout.
pub fn detect_regen_strategy() -> RegenStrategy {
    if Path::new(&format!("/{PAIR_MARKER}")).exists() {
        RegenStrategy::ImmutableChroot
    } else if Path::new(REINSTALL_GRUB_PATH).exists() {
        RegenStrategy::ReinstallScript
    } else {
        RegenStrategy::Mkconfig
    }
}

/// Regenerate grub.cfg with `strategy`, leaving the boot pointer where it is.
pub fn regenerate_grub(cmd: &CommandRunner, strategy: RegenStrategy) -> Result<()> {
    match strategy {
        RegenStrategy::ImmutableChroot => {
            let devices = detect_devices();
            let pointer = current_boot_pointer(cmd)?;
            activate_target(cmd, &devices, &pointer)
        }
        RegenStrategy::ReinstallScript => cmd.run(REINSTALL_GRUB_PATH, &[]).map(|_| ()),
        RegenStrategy::Mkconfig => cmd.run("grub-mkconfig", &["-o", GRUB_CFG]).map(|_| ()),
    }
}

/// The set id embedded in a boot pointer, if it points at a snapshot set.
/// `@deploytix-sets/<id>/root` → `Some(<id>)`; `@` → `None`.
pub fn pointer_set_id(pointer: &str) -> Option<String> {
    let rest = pointer.strip_prefix("@deploytix-sets/")?;
    rest.strip_suffix("/root").map(str::to_string)
}

/// The paired `usr`/`etc` subvolumes for a given root subvolume, by convention:
/// a set uses its sibling `usr`/`etc`; the live `@` uses `@usr`/`@etc`.
fn paired_subvols(root_subvol: &str) -> (String, String) {
    match pointer_set_id(root_subvol) {
        Some(id) => (snapshot::set_usr_subvol(&id), snapshot::set_etc_subvol(&id)),
        None => (
            crate::immutable::USR_SUBVOL.to_string(),
            crate::immutable::ETC_SUBVOL.to_string(),
        ),
    }
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

    #[test]
    fn regenerate_dry_run_is_safe_for_every_strategy() {
        // Each strategy must issue its commands through the dry-run runner
        // without touching the host (the immutable one reads the pointer,
        // which dry-run reports as `@`).
        let cmd = CommandRunner::new(true);
        for strategy in [
            RegenStrategy::ImmutableChroot,
            RegenStrategy::ReinstallScript,
            RegenStrategy::Mkconfig,
        ] {
            regenerate_grub(&cmd, strategy).unwrap();
            assert!(!strategy.describe().is_empty());
        }
    }
}
