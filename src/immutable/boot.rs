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
use crate::immutable::snapshot::{self, ImmutableDevices, SubvolSet};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use tracing::info;

/// Where grub.cfg lives / is regenerated.
pub const GRUB_CFG: &str = "/boot/grub/grub.cfg";
/// The running system's grub template — under the immutable model this is the
/// *running* set's `@etc` copy, not the target's. Both are written on every
/// activation; see [`sync_live_pointer_cmd`].
pub const GRUB_DEFAULT: &str = "/etc/default/grub";
/// Scratch mountpoint for the grub-regeneration chroot.
const GRUB_CHROOT: &str = "/run/deploytix-grub";
/// Kernel cmdline of the running system.
const PROC_CMDLINE: &str = "/proc/cmdline";

/// The root subvolume named by a kernel cmdline's `rootflags=subvol=`.
///
/// Mirrors `resolve_root_subvol()` in the generated mountcrypt hook — the code
/// that actually performed the mount — so this reports what the system really
/// booted: the last occurrence wins (kernel behaviour for repeated parameters)
/// and surrounding double quotes are stripped (grub-btrfs emits them).
/// `None` when the cmdline names no subvolume.
pub fn parse_root_subvol(cmdline: &str) -> Option<String> {
    let mut found = None;
    for token in cmdline.split_whitespace() {
        let Some(flags) = token.strip_prefix("rootflags=") else {
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

/// The root subvolume the running system booted from; the base `@` when the
/// cmdline says nothing (an unreadable `/proc/cmdline`, or a layout that does
/// not pass `rootflags`).
///
/// This is deliberately **not** [`current_boot_pointer`]: that reads grub.cfg,
/// which names what boots *next*. After an update stages a new set the two
/// differ, and confusing them is how the running system ends up unprotected.
pub fn running_root_subvol() -> String {
    std::fs::read_to_string(PROC_CMDLINE)
        .ok()
        .and_then(|cmdline| parse_root_subvol(&cmdline))
        .unwrap_or_else(|| crate::immutable::ROOT_SUBVOL.to_string())
}

/// The snapshot set id the running system booted from; `@` for the base install.
pub fn running_set_id() -> String {
    let root = running_root_subvol();
    pointer_set_id(&root).unwrap_or_else(|| crate::immutable::ROOT_SUBVOL.to_string())
}

/// The `{root, usr, etc}` subvolumes the running system booted from — the
/// source a new snapshot set must be built from so updates compose.
pub fn running_subvols() -> SubvolSet {
    SubvolSet::for_root(&running_root_subvol())
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

/// `sed` that points the **running** system's grub template at `root_subvol`.
///
/// The boot config is a *derived* file: whatever next runs `grub-mkconfig`
/// rebuilds it from the `/etc/default/grub` of whichever root that run sees.
/// Two things do so from the live root outside deploytix — the
/// `95-grub-reinstall.hook` pacman hook on a kernel update, and `grub-btrfsd`
/// on a snapshot change. Both would otherwise rebuild from the *running* set's
/// template and revert a staged pointer to whatever is booted now.
///
/// Writing it here also makes the template an accurate record of the activated
/// target, which is what [`current_boot_pointer`] reads — the only source that
/// is correct on a standalone install, where the config that actually boots is
/// inside the signed EFI binary and cannot be read back.
fn sync_live_pointer_cmd(root_subvol: &str) -> String {
    set_pointer_sed(GRUB_DEFAULT, root_subvol)
}

/// The command that rebuilds the boot configuration, run **inside** the
/// regeneration chroot.
///
/// One rule, decided in the chroot rather than by inspecting the host: if the
/// system has the `reinstall-grub` pipeline, use it. On a standalone GRUB
/// install (SecureBoot sbctl + encryption) grub.cfg is embedded in the signed
/// EFI binary, so a bare `grub-mkconfig` writes a file nothing reads and the
/// menu — snapshot entries included — never changes. The pipeline runs
/// `grub-mkconfig` and then rebuilds and re-signs the binary, making it the
/// correct superset on every layout that has it. Only a system without it
/// (unencrypted, plain grub-install) falls back to a bare `grub-mkconfig`.
fn regenerate_grub_cmd() -> String {
    format!(
        "if [ -x {REINSTALL_GRUB_PATH} ]; then {REINSTALL_GRUB_PATH}; \
         else grub-mkconfig -o {GRUB_CFG}; fi"
    )
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
        cmd.run_in_chroot(GRUB_CHROOT, &regenerate_grub_cmd())?;
        Ok(())
    })();
    let _ = cmd.run("sh", &["-c", &unmount_grub_chroot_cmd()]);
    if result.is_ok() {
        // Point the running system's template at the same target, so a later
        // regeneration from the live root reproduces this pointer instead of
        // reverting it, and so the pointer can be read back. Best-effort: the
        // boot config written above is what actually boots, and a missing or
        // read-only template must not fail an otherwise complete activation.
        let _ = cmd.run("sh", &["-c", &sync_live_pointer_cmd(root_subvol)]);
    }
    result
}

/// The boot target currently selected for the next boot.
///
/// Read from the **running system's** `/etc/default/grub`, which every
/// activation writes. That is the only source that is correct on a standalone
/// install: there the config GRUB reads is embedded in the signed EFI binary,
/// and `/boot/grub/grub.cfg` is merely the source it was built from — the two
/// diverge whenever something writes grub.cfg without rebuilding the binary.
/// Falls back to grub.cfg for installs predating the template sync. `@` when
/// neither yields anything.
pub fn current_boot_pointer(cmd: &CommandRunner) -> Result<String> {
    if cmd.is_dry_run() {
        return Ok("@".to_string());
    }
    let from_template = format!(
        "grep -o 'rootflags=subvol=[^ \"]*' {GRUB_DEFAULT} 2>/dev/null | head -n1 \
         | sed 's/rootflags=subvol=//'"
    );
    if let Some(out) = cmd.run("sh", &["-c", &from_template])? {
        let ptr = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !ptr.is_empty() {
            return Ok(ptr);
        }
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
    fn root_subvol_is_parsed_the_way_the_initramfs_parses_it() {
        // The plain case: what a staged set's default entry carries.
        assert_eq!(
            parse_root_subvol(
                "BOOT_IMAGE=/vmlinuz-linux root=/dev/mapper/Crypt-Root \
                 rootflags=subvol=@deploytix-sets/1700000000/root rw quiet"
            ),
            Some("@deploytix-sets/1700000000/root".to_string())
        );
        // grub-btrfs quotes its value, and appends its rootflags after the one
        // from GRUB_CMDLINE_LINUX_DEFAULT — last occurrence wins, matching the
        // kernel and mountcrypt's resolve_root_subvol().
        assert_eq!(
            parse_root_subvol(
                "rootflags=subvol=@ rootflags=subvolid=5,subvol=\"@snapshots/3/snapshot\""
            ),
            Some("@snapshots/3/snapshot".to_string())
        );
        // The base install.
        assert_eq!(parse_root_subvol("rootflags=subvol=@ rw"), Some("@".into()));
        // Nothing to find: a non-subvolume layout, or an unreadable cmdline.
        assert_eq!(parse_root_subvol("root=UUID=1234 rw"), None);
        assert_eq!(parse_root_subvol("rootflags=ro,noatime"), None);
        assert_eq!(parse_root_subvol("rootflags=subvol="), None);
    }

    #[test]
    fn running_subvols_pair_with_the_running_root() {
        // Whatever /proc/cmdline says on the test host, the mapping from a root
        // subvolume to its trio must hold in both directions.
        assert_eq!(SubvolSet::for_root("@"), SubvolSet::base());
        assert_eq!(
            SubvolSet::for_root("@deploytix-sets/42/root"),
            SubvolSet::of_set("42")
        );
        // running_subvols() must agree with running_root_subvol() on this host.
        assert_eq!(
            running_subvols(),
            SubvolSet::for_root(&running_root_subvol())
        );
    }
}
