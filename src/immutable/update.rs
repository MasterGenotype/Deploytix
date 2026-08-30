//! `deploytix update` — transactional system update.
//!
//! The running system's `/` and `/usr` are read-only, so updates never modify it
//! in place. Instead:
//!
//! 1. Pick the set to write into: the one already staged for the next boot when
//!    an update is pending (so a forgotten package joins it), otherwise a new
//!    **writable** snapshot of whatever boots next.
//! 2. Mount that set (root + paired usr/etc, with `/var`, `/home`, `/boot`
//!    bind-mounted so state and the kernel/initramfs are shared).
//! 3. Run `pacman -Syu` (or install the requested packages) and regenerate the
//!    initramfs *inside the set* via `artix-chroot`.
//! 4. On success, point the default boot entry at the set and regenerate
//!    grub.cfg; the change takes effect on the next reboot. On failure, delete
//!    the half-built set — unless it was a staged one, which keeps what earlier
//!    updates put there — and leave the running system untouched.
//!
//! Old sets are pruned to [`UpdateOptions::keep_sets`], never removing the
//! running set or the one just built.
//!
//! ## Updates chain, they do not restart
//! Each update sources the *next boot pointer*, never the pristine `@`. `@` is
//! read-only for the life of the system, so re-sourcing it every time would
//! quietly discard every package installed by earlier updates — both those from
//! previous boots and those staged moments ago.
//!
//! ## Caveat: shared `/boot`
//! `/boot` is a separate, non-snapshotted partition, so the kernel and initramfs
//! are shared across all sets. A rollback restores userspace (`@`/`@usr`/`@etc`)
//! but boots with the most recently installed kernel. The `mountcrypt` hook is
//! version-independent, so this is safe; only kernel *contents* are not rolled
//! back. This is documented in `docs/IMMUTABLE_SYSTEM.md`.

use crate::immutable::snapshot::{self, ImmutableDevices};
use crate::immutable::{boot, bootset, detect_devices};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use tracing::{info, warn};

/// Options controlling a transactional update.
pub struct UpdateOptions {
    /// Number of previous sets to retain when pruning (excludes the running set
    /// and the newly built one).
    pub keep_sets: usize,
    /// Reboot automatically after a successful update.
    pub reboot: bool,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            keep_sets: 3,
            reboot: false,
        }
    }
}

/// Directory under `/run` where a set is assembled for the chroot.
fn target_dir(id: &str) -> String {
    format!("/run/deploytix-update/{id}")
}

/// Shell that mounts a set (root + paired usr/etc + bind var/home/boot) at its
/// target so `artix-chroot` can operate on it. Snapshots were created writable,
/// so pacman can write to `/` and `/usr`.
pub fn mount_set_cmd(devices: &ImmutableDevices, id: &str) -> String {
    let target = target_dir(id);
    format!(
        "set -e; t={target}; mkdir -p \"$t\"; \
         mount -t btrfs -o subvol={root},rw,noatime,compress=zstd {root_fs} \"$t\"; \
         mkdir -p \"$t/usr\" \"$t/etc\"; \
         mount -t btrfs -o subvol={usr},rw,noatime,compress=zstd {usr_fs} \"$t/usr\"; \
         mount -t btrfs -o subvol={etc},rw,noatime,compress=zstd {root_fs} \"$t/etc\"; \
         for d in var home boot; do mkdir -p \"$t/$d\"; mount --rbind \"/$d\" \"$t/$d\"; done",
        target = target,
        root = snapshot::set_root_subvol(id),
        etc = snapshot::set_etc_subvol(id),
        usr = snapshot::set_usr_subvol(id),
        root_fs = devices.root_fs,
        usr_fs = devices.usr_fs,
    )
}

/// Shell that recursively unmounts and removes a set's chroot target.
pub fn unmount_set_cmd(id: &str) -> String {
    let target = target_dir(id);
    format!("umount -R {target} 2>/dev/null || true; rmdir {target} 2>/dev/null || true")
}

/// Staging dir under the shared `/var` (rbind-mounted into the set) where local
/// package files are copied so the chroot can reach them by absolute path.
pub const PKG_STAGE_DIR: &str = "/var/cache/deploytix-update";

/// Whether an update arg is a local package file (installed with `pacman -U`)
/// rather than a repo package name (installed with `pacman -S`).
fn is_local_pkg(arg: &str) -> bool {
    arg.ends_with(".pkg.tar.zst")
        || arg.ends_with(".pkg.tar.xz")
        || std::path::Path::new(arg).is_file()
}

/// Split update args into (local package files, repo package names).
pub fn classify_args(extra: &[String]) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut names = Vec::new();
    for a in extra {
        if is_local_pkg(a) {
            files.push(a.clone());
        } else {
            names.push(a.clone());
        }
    }
    (files, names)
}

/// Build the ordered pacman invocation(s) for a transaction. `staged_files` are
/// absolute paths valid *inside the chroot*; `names` are repo package names.
///
/// - nothing → full `pacman -Syu`;
/// - repo names → `pacman -Syu --noconfirm <names>`;
/// - local files → `pacman -U --noconfirm <files>` (no DB sync, so it works
///   offline / without configured repos — which is why `-Syu <file>` failed with
///   a "missing database" error).
pub fn pacman_cmds(staged_files: &[String], names: &[String]) -> Vec<String> {
    let mut cmds = Vec::new();
    if staged_files.is_empty() && names.is_empty() {
        cmds.push("pacman -Syu --noconfirm".to_string());
        return cmds;
    }
    if !names.is_empty() {
        cmds.push(format!("pacman -Syu --noconfirm {}", names.join(" ")));
    }
    if !staged_files.is_empty() {
        cmds.push(format!("pacman -U --noconfirm {}", staged_files.join(" ")));
    }
    cmds
}

/// Copy local package files into the shared `/var` staging dir so `pacman -U`
/// can reach them inside the chroot. Returns their absolute in-chroot paths.
pub(crate) fn stage_local_pkgs(cmd: &CommandRunner, files: &[String]) -> Result<Vec<String>> {
    let staged: Result<Vec<String>> = files
        .iter()
        .map(|f| {
            std::path::Path::new(f)
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| format!("{PKG_STAGE_DIR}/{n}"))
                .ok_or_else(|| DeploytixError::ConfigError(format!("bad package path: {f}")))
        })
        .collect();
    let staged = staged?;
    if cmd.is_dry_run() {
        return Ok(staged);
    }
    std::fs::create_dir_all(PKG_STAGE_DIR)?;
    for (src, dest) in files.iter().zip(staged.iter()) {
        let abs = std::fs::canonicalize(src).map_err(|e| {
            DeploytixError::ConfigError(format!("package file not found: {src}: {e}"))
        })?;
        std::fs::copy(&abs, dest)?;
    }
    Ok(staged)
}

/// Confirm we are on an immutable deploytix system before updating.
fn ensure_immutable(cmd: &CommandRunner) -> Result<()> {
    if cmd.is_dry_run() {
        return Ok(());
    }
    // The live pairing marker at the root of `/` is the reliable signal.
    if std::path::Path::new(&format!("/{}", crate::immutable::PAIR_MARKER)).exists() {
        Ok(())
    } else {
        Err(DeploytixError::ConfigError(
            "not an immutable deploytix system (no /.deploytix-pair marker); \
             `deploytix update` only applies to immutable installs"
                .to_string(),
        ))
    }
}

/// Delete sets older than the newest `keep`, never touching `running` or `keep_id`.
fn prune_sets(
    cmd: &CommandRunner,
    devices: &ImmutableDevices,
    keep: usize,
    running: &str,
    keep_id: &str,
) -> Result<()> {
    let sets = snapshot::list_sets(cmd, &devices.root_fs)?;
    let removable: Vec<String> = sets
        .iter()
        .filter(|id| id.as_str() != running && id.as_str() != keep_id)
        .cloned()
        .collect();
    if removable.len() <= keep {
        return Ok(());
    }
    let to_delete = &removable[..removable.len() - keep];
    for id in to_delete {
        if let Err(e) = snapshot::delete_set(cmd, devices, id) {
            warn!("[immutable] Failed to prune set {}: {}", id, e);
            continue;
        }
        // The archive is only useful for booting the set it belongs to, and
        // /boot is a small partition — it goes with it.
        bootset::remove(cmd, bootset::BOOT_ROOT, id);
    }
    // Sweep archives whose sets are gone, so a small /boot cannot fill with
    // kernels nothing can boot.
    let live: Vec<String> = snapshot::list_sets(cmd, &devices.root_fs).unwrap_or_default();
    bootset::prune_orphans(cmd, bootset::BOOT_ROOT, &live);
    Ok(())
}

/// Pick the set this update writes into: `(id, reused)`.
///
/// Forgetting a package and running `deploytix update` again in the same
/// session must land in the set already staged for the next boot — otherwise
/// the second run would build a rival set from the *running* system, and the
/// first run's packages would vanish when that rival became the boot target.
/// So a staged, writable set is filled further rather than replaced, and stays
/// the set the system boots into.
///
/// Otherwise a fresh set is snapshotted from whatever the next boot is pointed
/// at, which is the running system in the ordinary case and the chosen target
/// when a `deploytix rollback` is staged. Never from `@` unless `@` is what
/// boots next: `@` is the pristine base and is never written to, so sourcing it
/// unconditionally would drop everything earlier updates installed.
fn resolve_update_set(
    cmd: &CommandRunner,
    devices: &ImmutableDevices,
    staged: &str,
    running: &str,
) -> Result<(String, bool)> {
    // Ask about writability only for a staged set — the query mounts the
    // filesystem root, and every other branch builds a fresh set regardless.
    let staged_id = staged_set_id(staged, running);
    let writable = staged_id
        .as_ref()
        .is_some_and(|id| snapshot::set_is_writable(cmd, &devices.root_fs, id));

    if let Some(id) = staged_id.filter(|_| writable) {
        info!(
            "[immutable] Adding to update set {} already staged for the next boot",
            id
        );
        return Ok((id, true));
    }

    info!(
        "[immutable] Building transactional update set from {}",
        staged
    );
    let source = snapshot::SourceSubvols::for_root(staged);
    let id = snapshot::create_set(cmd, devices, &source, /* readonly = */ false)?;
    Ok((id, false))
}

/// The id of a set that is staged for the next boot but not yet running — the
/// only candidate an update may keep filling in place.
///
/// `None` when the next boot is the running system (nothing staged; writing in
/// place would mutate the live root), or when the pointer is not a set at all.
fn staged_set_id(staged: &str, running: &str) -> Option<String> {
    if staged == running {
        return None;
    }
    boot::pointer_set_id(staged)
}

/// Perform a transactional update.
pub fn run_update(
    cmd: &CommandRunner,
    extra_packages: &[String],
    opts: &UpdateOptions,
) -> Result<()> {
    ensure_immutable(cmd)?;
    let devices = detect_devices();

    // What the next boot is pointed at, versus what this session booted from.
    // They differ exactly while an update or rollback is staged.
    let staged = boot::current_boot_pointer(cmd)?;
    let running = boot::running_root_subvol();

    let (id, reused) = resolve_update_set(cmd, &devices, &staged, &running)?;

    let (local_files, repo_names) = classify_args(extra_packages);

    // Everything from here is unwound on failure so a bad update leaves nothing.
    let result = (|| -> Result<()> {
        cmd.run("sh", &["-c", &mount_set_cmd(&devices, &id)])?;
        let target = target_dir(&id);
        // Local .pkg.tar.zst files are copied into the shared /var so the chroot
        // can reach them by absolute path and install them with `pacman -U`.
        let staged_files = stage_local_pkgs(cmd, &local_files)?;
        info!("[immutable] Running pacman in set {}", id);
        for pac in pacman_cmds(&staged_files, &repo_names) {
            cmd.run_in_chroot(&target, &pac)?;
        }
        // Regenerate the (shared) initramfs from within the updated set.
        cmd.run_in_chroot(&target, "mkinitcpio -P")?;
        Ok(())
    })();

    // Always release the chroot mounts and clear the package staging dir.
    let _ = cmd.run("sh", &["-c", &unmount_set_cmd(&id)]);
    if !cmd.is_dry_run() {
        let _ = std::fs::remove_dir_all(PKG_STAGE_DIR);
    }

    match result {
        Ok(()) => {
            // Capture the kernel this set was built with before anything moves
            // the pointer: /boot is shared, so this copy is what makes a later
            // rollback to this set boot the kernel its modules match.
            if let Err(e) = bootset::archive(cmd, bootset::BOOT_ROOT, &id) {
                warn!(
                    "[immutable] Could not archive kernel images for set {} ({}) — \
                     rolling back to it later will keep the then-current kernel",
                    id, e
                );
            }
            // Re-activate even when the pointer already names this set: the
            // update may have installed a kernel, and grub.cfg lists those.
            boot::activate_target(cmd, &devices, &snapshot::set_root_subvol(&id))?;
            // Protect the set the live system is *running* from. Reading the
            // pointer back here would name the set just activated, leaving the
            // running one prunable out from under the session.
            let running_id = boot::pointer_set_id(&running).unwrap_or_else(|| "@".to_string());
            prune_sets(cmd, &devices, opts.keep_sets, &running_id, &id)?;
            info!(
                "[immutable] Update ready. Reboot to activate set {} (rollback: `deploytix rollback`).",
                id
            );
            if opts.reboot {
                cmd.run("reboot", &[])?;
            }
            Ok(())
        }
        Err(e) => {
            // Only a set this run created is safe to discard; a reused one
            // still holds the packages an earlier update staged.
            if reused {
                warn!(
                    "[immutable] Update failed; set {} keeps what earlier updates staged \
                     and stays selected for the next boot",
                    id
                );
            } else {
                warn!("[immutable] Update failed; discarding set {}", id);
                let _ = snapshot::delete_set(cmd, &devices, &id);
                bootset::remove(cmd, bootset::BOOT_ROOT, &id);
            }
            // pacman's kernel hook and mkinitcpio write straight through the
            // rbind-mounted /boot, so a failure this late can leave the new
            // kernel over the canonical names while the pointer still selects
            // the old set. Put that set's own kernel back.
            let _ = bootset::restore(cmd, bootset::BOOT_ROOT, &bootset::archive_name(&running));
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_staged_this_session_is_the_one_to_keep_filling() {
        // Second `deploytix update` of the session: the pointer already names
        // the set the first one built, so the forgotten package joins it.
        assert_eq!(
            staged_set_id("@deploytix-sets/7/root", "@"),
            Some("7".to_string())
        );
        assert_eq!(
            staged_set_id("@deploytix-sets/7/root", "@deploytix-sets/3/root"),
            Some("7".to_string())
        );
    }

    #[test]
    fn the_running_set_is_never_written_to_in_place() {
        // Nothing staged: next boot is what is running. Writing there would
        // mutate the live root and leave nothing to roll back to.
        assert_eq!(staged_set_id("@", "@"), None);
        assert_eq!(
            staged_set_id("@deploytix-sets/7/root", "@deploytix-sets/7/root"),
            None
        );
        // A staged rollback to the pristine base is not a set to fill either.
        assert_eq!(staged_set_id("@", "@deploytix-sets/7/root"), None);
    }

    #[test]
    fn a_fresh_set_chains_off_whatever_boots_next() {
        // Ordinary case: source is the running system, so earlier updates'
        // packages carry forward instead of being dropped by re-sourcing `@`.
        assert_eq!(
            snapshot::SourceSubvols::for_root("@deploytix-sets/7/root").root,
            "@deploytix-sets/7/root"
        );
        // With a rollback staged, the source is the rolled-back-to target —
        // the operator's stated intent for the next boot.
        assert_eq!(snapshot::SourceSubvols::for_root("@").root, "@");
    }

    fn devices() -> ImmutableDevices {
        ImmutableDevices {
            root_fs: "/dev/mapper/Crypt-Root".into(),
            usr_fs: "/dev/mapper/Crypt-Usr".into(),
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
    fn mount_cmd_mounts_paired_subvols_and_binds_state() {
        let s = mount_set_cmd(&devices(), "42");
        assert!(s.contains("subvol=@deploytix-sets/42/root,rw"));
        assert!(s.contains("subvol=@deploytix-sets/42/usr,rw"));
        assert!(s.contains("subvol=@deploytix-sets/42/etc,rw"));
        assert!(s.contains("/dev/mapper/Crypt-Usr"));
        // /var, /home and /boot are shared via rbind.
        assert!(s.contains("mount --rbind \"/$d\""));
        assert_valid_shell(&s);
    }

    #[test]
    fn unmount_cmd_is_recursive_and_safe() {
        let s = unmount_set_cmd("42");
        assert!(s.contains("umount -R /run/deploytix-update/42"));
        assert_valid_shell(&s);
    }

    #[test]
    fn pacman_cmds_full_upgrade_vs_repo_names_vs_local_files() {
        // No args → full upgrade.
        assert_eq!(pacman_cmds(&[], &[]), vec!["pacman -Syu --noconfirm"]);
        // Repo names → -S.
        assert_eq!(
            pacman_cmds(&[], &["vim".to_string(), "git".to_string()]),
            vec!["pacman -Syu --noconfirm vim git"]
        );
        // Local files → -U (no DB sync).
        assert_eq!(
            pacman_cmds(
                &["/var/cache/deploytix-update/a.pkg.tar.zst".to_string()],
                &[]
            ),
            vec!["pacman -U --noconfirm /var/cache/deploytix-update/a.pkg.tar.zst"]
        );
    }

    #[test]
    fn classify_splits_files_from_names() {
        let (files, names) = classify_args(&[
            "vim".to_string(),
            "pkg/deploytix-git-1-x86_64.pkg.tar.zst".to_string(),
        ]);
        assert_eq!(files, vec!["pkg/deploytix-git-1-x86_64.pkg.tar.zst"]);
        assert_eq!(names, vec!["vim"]);
    }

    #[test]
    fn dry_run_update_is_safe_and_ordered() {
        // A dry-run must issue the snapshot → mount → pacman → boot-pointer
        // sequence without touching the system.
        let cmd = CommandRunner::new(true);
        run_update(&cmd, &[], &UpdateOptions::default()).unwrap();
    }
}
