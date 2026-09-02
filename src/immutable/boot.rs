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
use crate::immutable::PAIR_MARKER;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::path::Path;
use tracing::info;

/// Where grub.cfg lives / is regenerated.
///
/// On a **standalone** GRUB install (SecureBoot sbctl + encryption) this file is
/// not what boots: it is only the source `grub-mkstandalone` embeds into the
/// signed EFI binary's memdisk. See [`BOOT_POINTER_FILE`].
pub const GRUB_CFG: &str = "/boot/grub/grub.cfg";

/// Durable record of the boot target deploytix last activated.
///
/// The pointer cannot be read back reliably from the boot configuration itself.
/// On a standalone install the configuration that boots lives inside the signed
/// EFI binary, and extracting it from that memdisk is not practical; reading
/// `/boot/grub/grub.cfg` instead reports whatever last wrote *that* file, which
/// on such a system may never have reached the binary at all. Either way the
/// answer would be a guess about someone else's write.
///
/// So the activation records its own decision, on the shared `/boot` where every
/// set can see it — exactly as the LVM A/B backend records its active slot in
/// `lvm_ab::STATE_FILE`. This file is the authority for "what did we point the
/// next boot at"; grub.cfg is the fallback for installs predating it.
pub const BOOT_POINTER_FILE: &str = "/boot/deploytix-boot.conf";
/// The running system's grub template — on an immutable system this is the
/// *running* set's `@etc` copy, not the target's.
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

/// Whether the running system is a transactional immutable btrfs install.
///
/// The live pairing marker at `/` is written by the installer and carried by
/// every snapshot set, so its presence is the signal. Everything in this module
/// applies only to such a system.
pub fn is_immutable_btrfs() -> bool {
    Path::new(&format!("/{PAIR_MARKER}")).exists()
}

/// The command that rebuilds the boot configuration, run **inside** the
/// regeneration chroot.
///
/// One rule, decided in the chroot rather than by inspecting the host: if the
/// system has the `reinstall-grub` pipeline, use it, because on a standalone
/// GRUB install (SecureBoot sbctl + encryption) grub.cfg is embedded in the
/// signed EFI binary and a bare `grub-mkconfig` writes a file nothing reads —
/// the menu, snapshot entries included, would never change. The pipeline runs
/// `grub-mkconfig` and then rebuilds and re-signs the binary, so it is the
/// correct superset on every layout that has it. Only a system without it
/// (unencrypted, plain grub-install) falls back to bare `grub-mkconfig`.
fn regenerate_grub_cmd() -> String {
    format!(
        "if [ -x {REINSTALL_GRUB_PATH} ]; then {REINSTALL_GRUB_PATH}; \
         else grub-mkconfig -o {GRUB_CFG}; fi"
    )
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
/// grub.cfg is only ever a *derived* file: whatever next runs `grub-mkconfig`
/// rebuilds it from the `/etc/default/grub` of whichever root that run sees. On
/// an immutable system two things do so from the live root, behind our back:
///
/// - the `95-grub-reinstall.hook` pacman hook, on any kernel or grub update;
/// - a **stock** `grub-btrfsd`, on every snapshot change. Its "is the submenu
///   already there?" check greps a literal `{grub_directory}/grub.cfg` (an
///   upstream typo in the packaged 4.13), which never exists — so it always
///   takes the full-`grub-mkconfig` branch.
///
/// Either one regenerates grub.cfg from the running set's template and reverts
/// the pointer to whatever *that* set names, silently undoing a staged update.
/// Writing the pointer here as well makes such a regeneration reproduce the
/// staged target instead of discarding it. `lvm_ab::activate_slot` has always
/// done this for the A/B backend; the btrfs backend did not.
pub(crate) fn sync_live_pointer_cmd(root_subvol: &str) -> String {
    set_pointer_sed(GRUB_DEFAULT, root_subvol)
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

/// Make `root_subvol` the default boot, completely.
///
/// This is the only place the boot configuration is written, and it does the
/// whole job — `update` and `rollback` both go through it and neither needs to
/// follow up with anything:
///
/// 1. Mount `root_subvol` and its paired `usr`/`etc` at a scratch chroot. `/` is
///    a real btrfs root there, so `grub-probe` succeeds and grub-btrfs's
///    generator produces the snapshot entries — neither works against the live
///    overlay root.
/// 2. Point that root's `/etc/default/grub` at itself.
/// 3. Rebuild the boot configuration from inside the chroot with
///    [`regenerate_grub_cmd`], which handles a standalone (signed, embedded)
///    GRUB as well as an on-disk grub.cfg.
/// 4. Record the target in [`BOOT_POINTER_FILE`], the authority for what was
///    activated — a standalone install's real configuration is inside the
///    signed binary and cannot be read back.
/// 5. Point the *running* system's `/etc/default/grub` at the same target, so a
///    later regeneration by anything else reproduces this pointer instead of
///    reverting it (see [`sync_live_pointer_cmd`]).
///
/// Takes effect on the next reboot.
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
        // Record the decision. Not best-effort: without it the pointer can only
        // be guessed from a derived file, which on a standalone install is not
        // the configuration that boots. A rollback reading a wrong "current"
        // moves to the wrong set, so failing here is better than succeeding
        // with an unreadable result.
        write_boot_pointer_record(cmd, root_subvol)?;
        // Keep the running system's template naming the same target, so a later
        // grub-mkconfig from the live root reproduces this pointer rather than
        // reverting it (see `sync_live_pointer_cmd`). Best-effort: the boot
        // configuration written above is what actually boots, and a missing or
        // read-only template must not fail an otherwise complete activation.
        let _ = cmd.run("sh", &["-c", &sync_live_pointer_cmd(root_subvol)]);
    }
    result
}

/// The boot target currently selected for the next boot.
///
/// Prefers [`BOOT_POINTER_FILE`], the record deploytix wrote when it last
/// activated a target, and falls back to the first `rootflags=subvol=` in the
/// generated grub.cfg for installs that predate the record. `@` when neither
/// yields anything.
pub fn current_boot_pointer(cmd: &CommandRunner) -> Result<String> {
    if cmd.is_dry_run() {
        return Ok("@".to_string());
    }
    // The record deploytix wrote when it last activated a target. Authoritative
    // on every layout, and the only correct source on a standalone install.
    if let Some(pointer) = read_boot_pointer_record() {
        return Ok(pointer);
    }
    // No record: an install predating it, or one never activated since. Fall
    // back to the generated config, which is accurate on a non-standalone
    // system and is the best available answer on a standalone one.
    let script = format!(
        "grep -o 'rootflags=subvol=[^ \"]*' {GRUB_CFG} | head -n1 | sed 's/rootflags=subvol=//'"
    );
    let out = cmd.run("sh", &["-c", &script])?;
    let ptr = out
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    Ok(if ptr.is_empty() { "@".to_string() } else { ptr })
}

/// The subvolume named by a [`BOOT_POINTER_FILE`] body (`subvol=<path>`).
/// Blank lines and `#` comments are ignored, matching the A/B state file.
pub fn parse_boot_pointer(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .find_map(|l| l.strip_prefix("subvol="))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Read [`BOOT_POINTER_FILE`], or `None` when it is absent or unparseable.
fn read_boot_pointer_record() -> Option<String> {
    std::fs::read_to_string(BOOT_POINTER_FILE)
        .ok()
        .and_then(|text| parse_boot_pointer(&text))
}

/// The body written to [`BOOT_POINTER_FILE`] for `root_subvol`.
fn boot_pointer_record(root_subvol: &str) -> String {
    format!(
        "# Written by deploytix. The boot target activated for the next boot.\n\
         # On a standalone GRUB install the booting config is embedded in the\n\
         # signed EFI binary, so this record — not /boot/grub/grub.cfg — is what\n\
         # deploytix trusts when reporting or moving the pointer.\n\
         subvol={root_subvol}\n"
    )
}

/// Record `root_subvol` as the activated boot target.
fn write_boot_pointer_record(cmd: &CommandRunner, root_subvol: &str) -> Result<()> {
    if cmd.is_dry_run() {
        println!("  [dry-run] Would record boot target {root_subvol} in {BOOT_POINTER_FILE}");
        return Ok(());
    }
    std::fs::write(BOOT_POINTER_FILE, boot_pointer_record(root_subvol))?;
    Ok(())
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

    /// The record is the only truthful source on a standalone install, where
    /// the booting config lives inside the signed EFI binary and grub.cfg is
    /// merely what it was built from.
    #[test]
    fn the_activated_target_round_trips_through_its_record() {
        for target in ["@", "@deploytix-sets/1788322987/root"] {
            let body = boot_pointer_record(target);
            assert_eq!(parse_boot_pointer(&body).as_deref(), Some(target));
        }
    }

    #[test]
    fn a_record_that_says_nothing_useful_yields_nothing() {
        // Comments and blank lines are skipped, so a record carrying only
        // commentary must fall through to the grub.cfg fallback rather than
        // returning an empty pointer.
        assert_eq!(parse_boot_pointer("# just a comment\n\n"), None);
        assert_eq!(parse_boot_pointer(""), None);
        assert_eq!(parse_boot_pointer("subvol=\n"), None);
        assert_eq!(parse_boot_pointer("slot=A\n"), None);
        // Leading/trailing whitespace is tolerated.
        assert_eq!(
            parse_boot_pointer("  subvol=@deploytix-sets/9/root  \n").as_deref(),
            Some("@deploytix-sets/9/root")
        );
    }

    #[test]
    fn recording_the_target_is_dry_run_safe() {
        let cmd = CommandRunner::new(true);
        write_boot_pointer_record(&cmd, "@deploytix-sets/9/root").unwrap();
        assert!(!Path::new(BOOT_POINTER_FILE).exists());
    }

    /// A staged pointer has to survive anything else regenerating grub.cfg from
    /// the live root — the pacman grub hook, or a stock grub-btrfsd, both of
    /// which rebuild it from the *running* system's template.
    #[test]
    fn the_pointer_is_written_to_the_running_template_too() {
        let target = "@deploytix-sets/9/root";
        let chroot_sed = set_pointer_sed(&format!("{GRUB_CHROOT}/etc/default/grub"), target);
        let live_sed = sync_live_pointer_cmd(target);

        // Two distinct files: the target set's template (reached through the
        // regeneration chroot) and the running system's own.
        assert!(chroot_sed.ends_with("/run/deploytix-grub/etc/default/grub"));
        assert!(live_sed.ends_with(" /etc/default/grub"));
        assert_ne!(chroot_sed, live_sed);
        // Both name the same target.
        assert!(live_sed.contains("rootflags=subvol=@deploytix-sets/9/root"));
        assert_valid_shell(&live_sed);
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

    /// A standalone GRUB install keeps grub.cfg inside the signed EFI binary,
    /// so a bare `grub-mkconfig` writes a file nothing reads and the menu never
    /// changes. The regeneration must prefer the reinstall pipeline, which
    /// rebuilds and re-signs it.
    #[test]
    fn regeneration_prefers_the_reinstall_pipeline_over_a_bare_mkconfig() {
        let c = regenerate_grub_cmd();
        assert!(c.contains("if [ -x /usr/local/bin/reinstall-grub ]"));
        // The fallback exists only for systems without the pipeline.
        let fallback = c.find("grub-mkconfig").unwrap();
        let guard = c.find("reinstall-grub").unwrap();
        assert!(guard < fallback, "the pipeline must be tried first: {c}");
        assert!(c.contains("grub-mkconfig -o /boot/grub/grub.cfg"));
        assert_valid_shell(&c);
    }
}
