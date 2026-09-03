//! `deploytix update` — transactional system update.
//!
//! The running system's `/` and `/usr` are read-only, so updates never modify it
//! in place. Instead:
//!
//! 1. Snapshot the **running** `{root, usr, etc}` trio into a new *writable*
//!    set. That is the base `{@, @usr, @etc}` only on a never-updated system;
//!    once an update has been activated the running trio is a snapshot set, and
//!    snapshotting it is what makes successive updates stack rather than each
//!    one rebasing onto the install-time base. The running trio is read from
//!    the kernel cmdline (see [`crate::immutable::boot::running_subvols`]),
//!    which is what the initramfs itself mounted.
//! 2. Mount that set (root + paired usr/etc, with `/var`, `/home`, `/boot`
//!    bind-mounted so state and the kernel/initramfs are shared).
//! 3. Run `pacman -Syu` (or install the requested packages) and regenerate the
//!    initramfs *inside the set* via `artix-chroot`.
//! 4. On success, point the default boot entry at the new set and regenerate
//!    grub.cfg; the change takes effect on the next reboot. On failure, delete
//!    the half-built set and leave the running system untouched.
//!
//! Old sets are pruned to [`UpdateOptions::keep_sets`], never removing the
//! running set or the one just built.
//!
//! ## Caveat: shared `/boot`
//! `/boot` is a separate, non-snapshotted partition, so the kernel and initramfs
//! are shared across all sets. A rollback restores userspace (`@`/`@usr`/`@etc`)
//! but boots with the most recently installed kernel. The `mountcrypt` hook is
//! version-independent, so this is safe; only kernel *contents* are not rolled
//! back. This is documented in `docs/IMMUTABLE_SYSTEM.md`.

use crate::immutable::snapshot::{self, ImmutableDevices};
use crate::immutable::{boot, detect_devices, history};
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

/// Which of `sets` (oldest first) may be deleted, keeping the newest `keep` and
/// never `running` or `keep_id`.
///
/// Split out from [`prune_sets`] because the protection is the whole point:
/// deleting the set the system is booted from leaves it unbootable on the next
/// reboot, and deleting the one just staged throws away the update.
pub(crate) fn sets_to_prune(
    sets: &[String],
    keep: usize,
    running: &str,
    keep_id: &str,
) -> Vec<String> {
    let removable: Vec<String> = sets
        .iter()
        .filter(|id| id.as_str() != running && id.as_str() != keep_id)
        .cloned()
        .collect();
    if removable.len() <= keep {
        return Vec::new();
    }
    removable[..removable.len() - keep].to_vec()
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
    for id in sets_to_prune(&sets, keep, running, keep_id) {
        if let Err(e) = snapshot::delete_set(cmd, devices, &id) {
            warn!("[immutable] Failed to prune set {}: {}", id, e);
        }
    }
    Ok(())
}

/// Perform a transactional update.
pub fn run_update(
    cmd: &CommandRunner,
    extra_packages: &[String],
    opts: &UpdateOptions,
) -> Result<()> {
    ensure_immutable(cmd)?;
    let devices = detect_devices();

    // What the system is running *now*, read from the kernel cmdline before
    // anything moves the boot pointer. Two things depend on it: the new set is
    // snapshotted from it (so updates stack instead of each one rebasing onto
    // the install-time `@`), and pruning must never delete it.
    let running = boot::running_set_id();
    let source = boot::running_subvols();

    info!(
        "[immutable] Building transactional update set from the running system ({})",
        source.root
    );
    let id = snapshot::create_set(cmd, &devices, &source, /* readonly = */ false)?;

    let (local_files, repo_names) = classify_args(extra_packages);

    // Everything from here is unwound on failure so a bad update leaves nothing.
    let started_at = history::now_secs();
    let start = std::time::Instant::now();

    let result = (|| -> Result<history::PackageChanges> {
        cmd.run("sh", &["-c", &mount_set_cmd(&devices, &id)])?;
        let target = target_dir(&id);
        // Local .pkg.tar.zst files are copied into the shared /var so the chroot
        // can reach them by absolute path and install them with `pacman -U`.
        let staged = stage_local_pkgs(cmd, &local_files)?;
        // Bracket the transaction with two `pacman -Q` reads. The pacman DB is
        // on the shared /var (rbound into the chroot), so it is not snapshotted
        // and these two reads are the only way to know what this set changed.
        let before = history::query_packages(cmd, &target);
        info!("[immutable] Running pacman in set {}", id);
        for pac in pacman_cmds(&staged, &repo_names) {
            cmd.run_in_chroot(&target, &pac)?;
        }
        let after = history::query_packages(cmd, &target);
        // Regenerate the (shared) initramfs from within the updated set.
        cmd.run_in_chroot(&target, "mkinitcpio -P")?;
        Ok(history::diff(&before, &after))
    })();

    // Always release the chroot mounts and clear the package staging dir.
    let _ = cmd.run("sh", &["-c", &unmount_set_cmd(&id)]);
    if !cmd.is_dry_run() {
        let _ = std::fs::remove_dir_all(PKG_STAGE_DIR);
    }

    // Best-effort history entry, written for failures too — a failed update is
    // exactly what a user wants to look at afterwards.
    if !cmd.is_dry_run() {
        history::write_record(&history::UpdateRecord {
            started_at,
            duration_secs: start.elapsed().as_secs(),
            backend: history::Backend::Btrfs,
            target: id.clone(),
            request: history::Request::classify(&repo_names, &local_files),
            outcome: match &result {
                Ok(_) => history::Outcome::Succeeded,
                Err(e) => history::Outcome::Failed(e.to_string()),
            },
            changes: result.as_ref().ok().cloned().unwrap_or_default(),
        });
    }

    match result {
        Ok(_) => {
            // Point the next boot at the set just built. `id` is the newest set
            // by construction (ids are epoch seconds), so this is always a step
            // forward, never back onto an older one.
            boot::activate_target(cmd, &devices, &snapshot::set_root_subvol(&id))?;
            // `running` was captured before the activation above: reading the
            // boot pointer here would return the set we just staged, leaving
            // the actually-booted set unprotected and eligible for deletion.
            prune_sets(cmd, &devices, opts.keep_sets, &running, &id)?;
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
            warn!("[immutable] Update failed; discarding set {}", id);
            let _ = snapshot::delete_set(cmd, &devices, &id);
            Err(e)
        }
    }
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

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pruning_keeps_the_newest_n_and_deletes_the_oldest() {
        let sets = ids(&["1", "2", "3", "4", "5"]);
        // running and keep_id are excluded first, then the newest `keep` of
        // what remains survive.
        assert_eq!(sets_to_prune(&sets, 2, "5", "5"), ids(&["1", "2"]));
        assert!(sets_to_prune(&sets, 10, "5", "5").is_empty());
        assert!(sets_to_prune(&[], 3, "@", "1").is_empty());
    }

    /// The regression: `run_update` read the boot pointer *after* staging the
    /// new set, so `running` was the new set and the actually-booted one fell
    /// into the deletable list. Deleting it makes the next boot fail and
    /// destroys the obvious rollback target.
    #[test]
    fn the_running_set_is_never_pruned_even_when_it_is_old() {
        let sets = ids(&["1", "2", "3", "4", "5"]);
        // Booted on the oldest set, staging the newest, keeping just one.
        let doomed = sets_to_prune(&sets, 1, "1", "5");
        assert!(
            !doomed.contains(&"1".to_string()),
            "the running set must survive pruning, got {doomed:?}"
        );
        assert!(
            !doomed.contains(&"5".to_string()),
            "the staged set must survive"
        );
        assert_eq!(doomed, ids(&["2", "3"]));
    }

    #[test]
    fn dry_run_update_is_safe_and_ordered() {
        // A dry-run must issue the snapshot → mount → pacman → boot-pointer
        // sequence without touching the system.
        let cmd = CommandRunner::new(true);
        run_update(&cmd, &[], &UpdateOptions::default()).unwrap();
    }
}
