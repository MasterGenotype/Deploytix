//! `deploytix remove` — transactional package removal.
//!
//! This is the same transaction as [`crate::immutable::update`] with a
//! different pacman verb. It snapshots the running trio into a new writable
//! set, runs `pacman -Rs` inside it, regenerates the initramfs, and points the
//! next boot at the result. `deploytix rollback` undoes a removal the same way
//! it undoes an update.
//!
//! # Why removal needs a guard and updating does not
//!
//! `/boot` and `/var` are shared and not snapshotted. `mount_set_cmd` bind
//! mounts them from the live system into every set's chroot.
//!
//! Adding files to shared space is harmless. Deleting them is not. A removal
//! that takes something out of `/boot` takes it out for the running set and
//! every rollback target at the same time, and no snapshot can bring it back.
//!
//! The dangerous case is not obvious from the package name. A kernel package
//! owns `/usr/lib/modules/<version>/vmlinuz` and nothing under `/boot`. The
//! kernel image and initramfs get to `/boot` through pacman hooks. Artix ships
//! `60-mkinitcpio-remove.hook`:
//!
//! ```text
//! [Trigger]
//! Type = Path
//! Operation = Remove
//! Target = usr/lib/modules/*/vmlinuz
//! [Action]
//! When = PreTransaction
//! ```
//!
//! The preset it runs writes to `/boot/initramfs-linux.img`. So removing a
//! kernel wipes shared `/boot` before pacman deletes a single file, and every
//! set is unbootable at once.
//!
//! [`protected_reason`] therefore recognises a kernel by that file pattern, the
//! same way mkinitcpio does. A list of names would miss `linux-zen`, an `-lts`
//! kernel, or anything built locally.
//!
//! # Known limitation: the pacman database
//!
//! The pacman database is on the shared `/var`. A removal updates it for every
//! set at once, while deleting files from the new set only. Roll a removal back
//! and the files come back while the database still says the package is gone.
//!
//! Updates already have the same problem in the other direction: roll one back
//! and the database reports versions the files no longer match. It comes from
//! `/var` being shared, not from this command, and is described in
//! `docs/IMMUTABLE_SYSTEM.md`.
//!
//! What this module does guarantee is that a removal which fails partway leaves
//! the database as it found it. See [`db_backup_cmd`].

use crate::immutable::history;
use crate::immutable::update::{ensure_immutable, run_in_new_set, UpdateOptions};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use tracing::{info, warn};

/// Options controlling a transactional removal.
pub struct RemoveOptions {
    /// Number of previous sets to retain when pruning.
    pub keep_sets: usize,
    /// Reboot automatically once the removal is staged.
    pub reboot: bool,
    /// `-c`: also remove everything that depends on the named packages.
    pub cascade: bool,
    /// `-n`: also delete configuration files instead of leaving `.pacsave`.
    pub purge: bool,
    /// Skip the confirmation prompt for the resolved removal set.
    pub assume_yes: bool,
}

impl Default for RemoveOptions {
    fn default() -> Self {
        Self {
            keep_sets: 3,
            reboot: false,
            cascade: false,
            purge: false,
            assume_yes: false,
        }
    }
}

/// The live pacman database, on the shared `/var`.
const PACMAN_LOCAL_DB: &str = "/var/lib/pacman/local";

/// Where the pre-transaction database copy is parked.
fn db_backup_path(id: &str) -> String {
    format!("/var/lib/pacman/local.deploytix-{id}")
}

/// Copy the pacman database aside before the transaction.
///
/// `pacman -R` writes to the shared `/var` straight away. Without this, a
/// removal that fails after pacman ran but before the set is activated would
/// leave the database saying the package is gone while the running system
/// still has every file. The system keeps working and quietly disagrees with
/// its own package manager.
///
/// `cp -a --reflink=auto` makes the copy free on btrfs: it shares the data
/// until one side changes, so it is instant and uses no extra space. On a
/// filesystem without reflink support it falls back to a normal copy instead
/// of failing.
pub fn db_backup_cmd(id: &str) -> String {
    format!(
        "set -e; rm -rf {backup}; cp -a --reflink=auto {db} {backup}",
        db = PACMAN_LOCAL_DB,
        backup = db_backup_path(id),
    )
}

/// Put the saved database back, replacing whatever pacman left behind.
pub fn db_restore_cmd(id: &str) -> String {
    format!(
        "set -e; test -d {backup}; rm -rf {db}.failed; mv {db} {db}.failed; \
         mv {backup} {db}; rm -rf {db}.failed",
        db = PACMAN_LOCAL_DB,
        backup = db_backup_path(id),
    )
}

/// Drop the saved database once the transaction has committed.
pub fn db_discard_cmd(id: &str) -> String {
    format!("rm -rf {}", db_backup_path(id))
}

/// The `-R` short flags for these options.
///
/// - `-s` (`--recursive`) is always on. Leaving behind the dependencies a
///   package pulled in would collect orphans that cannot be cleaned up
///   interactively, because `/usr` is read-only.
/// - `-c` (`--cascade`) only when asked. It removes everything that depends on
///   the target, which reaches base packages quickly.
/// - `-n` (`--nosave`) only when asked. `/etc` is the snapshotted `@etc`, so
///   the `.pacsave` files pacman leaves by default get rolled back with the
///   set anyway.
///
/// `-d`/`--nodeps` is deliberately unreachable. It lets pacman remove a package
/// something still needs, and on this system you would not find out until the
/// next reboot.
fn remove_flags(opts: &RemoveOptions) -> String {
    let mut f = String::from("-R");
    f.push('s');
    if opts.cascade {
        f.push('c');
    }
    if opts.purge {
        f.push('n');
    }
    f
}

/// Work out what would actually be removed, without removing anything.
///
/// `--print` runs no transaction. `--print-format %n` prints bare package names
/// instead of `name-version-rel`, which is the form the guard needs to look up
/// each package's file list. Exits non-zero if something still depends on the
/// package, printing what.
pub fn preflight_cmd(names: &[String], opts: &RemoveOptions) -> String {
    format!(
        "pacman {} --print --print-format '%n' {}",
        remove_flags(opts),
        names.join(" ")
    )
}

/// The real removal, non-interactive because it runs inside a chroot.
pub fn remove_cmd(names: &[String], opts: &RemoveOptions) -> String {
    format!(
        "pacman {} --noconfirm {}",
        remove_flags(opts),
        names.join(" ")
    )
}

/// List the files owned by each resolved package, in one chroot call.
///
/// `-Q` queries the local database, `-l` lists the files a package owns, and
/// `-q` drops the package-name column so the output is just paths. Each list is
/// preceded by a `PKG <name>` line so the results can be told apart.
pub fn file_list_cmd(names: &[String]) -> String {
    let mut s = String::from("set -u; ");
    for n in names {
        // `ERR` rather than `|| true`: a package whose file list cannot be
        // produced must not look like one that owns no files, or the checks
        // below would all pass by default. See [`PackageFiles::query_failed`].
        s.push_str(&format!(
            "printf 'PKG {n}\\n'; pacman -Qlq {n} 2>/dev/null || printf 'ERR {n}\\n'; "
        ));
    }
    s.push_str("true");
    s
}

/// What one package owns, and whether we managed to find out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFiles {
    pub name: String,
    pub files: Vec<String>,
    /// `pacman -Qlq` failed for this package. Every file-based rule in
    /// [`protected_reason`] then has nothing to match, so it would report the
    /// package as safe. Given that one of those rules is "this is a kernel and
    /// removing it wipes the shared /boot for every rollback target", an
    /// unanswered question is treated as a blocker rather than a pass.
    pub query_failed: bool,
}

/// Parse [`file_list_cmd`] output.
pub fn parse_file_lists(out: &str) -> Vec<PackageFiles> {
    let mut result: Vec<PackageFiles> = Vec::new();
    for line in out.lines() {
        let line = line.trim_end();
        if let Some(name) = line.strip_prefix("PKG ") {
            result.push(PackageFiles {
                name: name.trim().to_string(),
                files: Vec::new(),
                query_failed: false,
            });
        } else if line.starts_with("ERR ") {
            if let Some(last) = result.last_mut() {
                last.query_failed = true;
            }
        } else if !line.is_empty() {
            if let Some(last) = result.last_mut() {
                last.files.push(line.to_string());
            }
        }
    }
    result
}

/// Packages deploytix needs in order to update or roll back at all.
///
/// The other rules in [`protected_reason`] are worked out from what a package
/// owns. These four cannot be, because nothing in their file lists shows that
/// the update mechanism depends on them. Removing any one leaves a system with
/// a read-only `/usr` that can no longer repair itself.
const CORE_TOOLING: &[(&str, &str)] = &[
    (
        "pacman",
        "every transactional operation runs pacman in a chroot",
    ),
    (
        "mkinitcpio",
        "the initramfs is regenerated after every transaction",
    ),
    (
        "grub",
        "the boot pointer is a grub.cfg regenerated on every activation",
    ),
    (
        "btrfs-progs",
        "snapshot sets are created and deleted with btrfs(8)",
    ),
];

/// Why `pkg` must not be removed, or `None` if it is safe.
///
/// `files` is the package's file list from `pacman -Qlq`, and `encrypted` says
/// whether the root filesystem is behind LUKS.
///
/// Most rules look at what a package owns rather than what it is called. The
/// destructive cases share no naming convention: a kernel might be `linux`,
/// `linux-lts`, `linux-zen` or something built locally, and what makes it
/// dangerous is a single file the pacman hooks watch for.
pub fn protected_reason(pkg: &str, files: &[String], encrypted: bool) -> Option<String> {
    if let Some((_, why)) = CORE_TOOLING.iter().find(|(name, _)| *name == pkg) {
        return Some(format!("deploytix depends on it: {why}"));
    }

    // A kernel, recognised the way 60-mkinitcpio-remove.hook recognises one.
    // Its PreTransaction action deletes the initramfs and kernel image from the
    // shared /boot. That happens for every set at once, before pacman removes
    // any file, and no snapshot can restore them.
    if files
        .iter()
        .any(|f| is_kernel_image(f.trim_start_matches('/')))
    {
        return Some(
            "it is a kernel: removing it deletes the initramfs and kernel image from the \
             shared /boot, which every snapshot set boots from — including every rollback target"
                .to_string(),
        );
    }

    // Owns files in /boot directly: microcode, memtest86+, bootloader payloads.
    // /boot is bind mounted from the live system into every set, so deleting
    // these removes them everywhere.
    if let Some(f) = files.iter().find(|f| f.starts_with("/boot/")) {
        return Some(format!(
            "it owns {f} in the shared /boot, which is not snapshotted and cannot be rolled back"
        ));
    }

    // On a LUKS root, 90-mkinitcpio-install.hook fires when usr/bin/cryptsetup
    // or usr/lib/initcpio/* is removed. It regenerates an initramfs that can no
    // longer unlock the root, and writes it to the shared /boot, so the running
    // system and every rollback target would boot it.
    if encrypted {
        if let Some(f) = files.iter().find(|f| {
            let f = f.trim_start_matches('/');
            f == "usr/bin/cryptsetup" || f.starts_with("usr/lib/initcpio/")
        }) {
            return Some(format!(
                "the root filesystem is encrypted and this package owns {f}; removing it \
                 regenerates an initramfs that cannot unlock the root, into the shared /boot"
            ));
        }
    }

    None
}

/// Whether `path` (relative, no leading slash) is the kernel image the
/// mkinitcpio hooks watch for: `usr/lib/modules/<version>/vmlinuz`.
fn is_kernel_image(path: &str) -> bool {
    let mut parts = path.split('/');
    parts.next() == Some("usr")
        && parts.next() == Some("lib")
        && parts.next() == Some("modules")
        && parts.next().is_some_and(|v| !v.is_empty())
        && parts.next() == Some("vmlinuz")
        && parts.next().is_none()
}

/// Check every package in the resolved removal set, returning all the reasons
/// it is refused rather than just the first. Reporting one blocker at a time
/// makes people re-run the command until they give up.
pub fn check_protected(
    lists: &[PackageFiles],
    encrypted: bool,
) -> std::result::Result<(), Vec<String>> {
    let blocked: Vec<String> = lists
        .iter()
        .filter_map(|p| {
            if p.query_failed {
                return Some(format!(
                    "{}: could not list its files, so the kernel, /boot and \
                     encrypted-root checks could not be run",
                    p.name
                ));
            }
            protected_reason(&p.name, &p.files, encrypted).map(|why| format!("{}: {why}", p.name))
        })
        .collect();
    if blocked.is_empty() {
        Ok(())
    } else {
        Err(blocked)
    }
}

/// Work out what a removal would take out, and refuse it if anything in that
/// set is protected.
///
/// The check runs against the resolved set, not the names typed on the command
/// line. `-Rs` pulls in dependencies nothing else needs, and a kernel or
/// microcode package arriving that way does just as much damage as one named
/// directly.
fn resolve_and_check(
    cmd: &CommandRunner,
    target: &str,
    names: &[String],
    opts: &RemoveOptions,
) -> Result<Vec<String>> {
    let preflight = preflight_cmd(names, opts);
    info!("[immutable] Resolving removal set: {}", preflight);
    let out = cmd.run_in_chroot(target, &preflight).map_err(|e| {
        DeploytixError::ConfigError(format!(
            "pacman refused the removal (something still depends on it, or it is not installed): {e}"
        ))
    })?;

    let Some(out) = out else {
        return Ok(Vec::new()); // dry run
    };
    let resolved: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    if resolved.is_empty() {
        return Err(DeploytixError::ConfigError(
            "pacman resolved nothing to remove".to_string(),
        ));
    }

    let lists = match cmd.run_in_chroot(target, &file_list_cmd(&resolved))? {
        Some(o) => parse_file_lists(&String::from_utf8_lossy(&o.stdout)),
        None => Vec::new(),
    };

    let encrypted = std::path::Path::new(crate::immutable::ROOT_FS_DEVICE).exists();
    if let Err(reasons) = check_protected(&lists, encrypted) {
        return Err(DeploytixError::ConfigError(format!(
            "refusing to remove {} package(s) the system cannot recover from:\n  {}",
            reasons.len(),
            reasons.join("\n  ")
        )));
    }

    Ok(resolved)
}

/// Perform a transactional package removal.
pub fn run_remove(cmd: &CommandRunner, packages: &[String], opts: &RemoveOptions) -> Result<()> {
    if packages.is_empty() {
        return Err(DeploytixError::ConfigError(
            "no packages given to remove".to_string(),
        ));
    }
    ensure_immutable(cmd)?;

    let names: Vec<String> = packages.to_vec();
    let request = history::Request::Remove(names.clone());
    let flags = remove_flags(opts);
    let assume_yes = opts.assume_yes;
    let cascade = opts.cascade;
    let purge = opts.purge;

    run_in_new_set(
        cmd,
        &into_update_opts(opts),
        request,
        move |cmd, target, set_id| {
            let ropts = RemoveOptions {
                cascade,
                purge,
                assume_yes,
                ..Default::default()
            };

            // Resolve and vet before anything is touched. The set is a snapshot and
            // `--print` changes nothing, so this is accurate and free to abort.
            let resolved = resolve_and_check(cmd, target, &names, &ropts)?;
            if !resolved.is_empty() {
                info!(
                    "[immutable] Removing {} package(s): {}",
                    resolved.len(),
                    resolved.join(" ")
                );
            }

            if !assume_yes && !cmd.is_dry_run() {
                let prompt = format!(
                    "Remove {} package(s) from a new snapshot set?\n  {}\n\
                 The running system is untouched until you reboot.",
                    resolved.len(),
                    resolved.join("\n  ")
                );
                if !crate::utils::prompt::prompt_confirm(&prompt, false)? {
                    return Err(DeploytixError::ConfigError("removal cancelled".to_string()));
                }
            }

            // The pacman DB is on the shared /var, so a failure after this point
            // would otherwise leave it disagreeing with the running system.
            cmd.run("sh", &["-c", &db_backup_cmd(set_id)])?;

            let before = history::query_packages(cmd, target);
            let outcome = (|| -> Result<()> {
                cmd.run_in_chroot(target, &remove_cmd(&names, &ropts))?;
                // Regenerate the (shared) initramfs from within the set. If the
                // removal took out something the initramfs needs, this fails and
                // the whole transaction is discarded — which is the point.
                cmd.run_in_chroot(target, "mkinitcpio -P")?;
                Ok(())
            })();

            if let Err(e) = outcome {
                warn!("[immutable] Removal failed; restoring the pacman database");
                if let Err(re) = cmd.run("sh", &["-c", &db_restore_cmd(set_id)]) {
                    warn!(
                        "[immutable] Could not restore the pacman database: {}. \
                     A copy is at {}",
                        re,
                        db_backup_path(set_id)
                    );
                }
                return Err(e);
            }

            let after = history::query_packages(cmd, target);
            let changes = history::diff(&before, &after);

            // pacman exits 0 on a transaction that removed nothing, so a removal
            // that quietly did nothing would look identical to a successful one
            // right up until the reboot.
            if !cmd.is_dry_run() && changes.removed.is_empty() {
                let _ = cmd.run("sh", &["-c", &db_restore_cmd(set_id)]);
                return Err(DeploytixError::ConfigError(format!(
                    "pacman reported success but removed nothing; requested: {}",
                    names.join(" ")
                )));
            }

            let _ = cmd.run("sh", &["-c", &db_discard_cmd(set_id)]);
            info!("[immutable] Removed with `pacman {}`", flags);
            Ok(changes)
        },
    )
}

/// Removal uses the same pruning and reboot behaviour as an update.
fn into_update_opts(opts: &RemoveOptions) -> UpdateOptions {
    UpdateOptions {
        keep_sets: opts.keep_sets,
        reboot: opts.reboot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> RemoveOptions {
        RemoveOptions::default()
    }

    /// `-s` is not optional. There is no interactive way to clean up orphans
    /// afterwards, because `/usr` is read-only.
    #[test]
    fn recursive_is_always_on_and_nodeps_is_unreachable() {
        assert_eq!(remove_flags(&opts()), "-Rs");
        for o in [
            RemoveOptions {
                cascade: true,
                ..Default::default()
            },
            RemoveOptions {
                purge: true,
                ..Default::default()
            },
        ] {
            let f = remove_flags(&o);
            assert!(f.starts_with("-Rs"), "recursive must stay on: {f}");
            assert!(!f.contains('d'), "--nodeps must not be reachable: {f}");
        }
    }

    #[test]
    fn cascade_and_purge_add_their_flags() {
        let f = remove_flags(&RemoveOptions {
            cascade: true,
            purge: true,
            ..Default::default()
        });
        assert_eq!(f, "-Rscn");
    }

    /// `--print` makes this safe to run against a real set, and
    /// `--print-format %n` makes the output usable as input to `pacman -Qlq`.
    #[test]
    fn preflight_prints_bare_names_and_changes_nothing() {
        let c = preflight_cmd(&["foo".into(), "bar".into()], &opts());
        assert!(c.contains("--print"));
        assert!(c.contains("--print-format '%n'"));
        assert!(!c.contains("--noconfirm"), "a print run confirms nothing");
        assert!(c.ends_with("foo bar"));
    }

    #[test]
    fn removal_is_noninteractive_inside_the_chroot() {
        let c = remove_cmd(&["foo".into()], &opts());
        assert!(c.starts_with("pacman -Rs --noconfirm"));
        assert!(!c.contains("--print"));
    }

    /// A kernel owns usr/lib/modules/<ver>/vmlinuz and nothing under /boot.
    /// This is the path 60-mkinitcpio-remove.hook watches for.
    #[test]
    fn kernels_are_identified_by_the_file_the_hook_keys_on() {
        assert!(is_kernel_image("usr/lib/modules/6.9.1-artix1-1/vmlinuz"));
        assert!(is_kernel_image("usr/lib/modules/x/vmlinuz"));
        // Near misses that must not be treated as kernels.
        assert!(!is_kernel_image("usr/lib/modules/6.9.1/build/vmlinuz"));
        assert!(!is_kernel_image("usr/lib/modules/vmlinuz"));
        assert!(!is_kernel_image("usr/lib/modules//vmlinuz"));
        assert!(!is_kernel_image("boot/vmlinuz-linux"));
    }

    #[test]
    fn a_kernel_is_refused_however_it_is_named() {
        for name in ["linux", "linux-lts", "linux-zen", "my-custom-kernel"] {
            let files = vec![
                "/usr/lib/modules/6.9.1-artix1-1/vmlinuz".to_string(),
                "/usr/lib/modules/6.9.1-artix1-1/modules.dep".to_string(),
            ];
            let why = protected_reason(name, &files, false)
                .unwrap_or_else(|| panic!("{name} must be refused"));
            assert!(why.contains("kernel"), "{why}");
        }
    }

    /// /boot is bind mounted from the live system into every set, so anything
    /// deleted there is gone for every rollback target too.
    #[test]
    fn packages_owning_shared_boot_files_are_refused() {
        let files = vec!["/boot/amd-ucode.img".to_string()];
        let why = protected_reason("amd-ucode", &files, false).expect("refused");
        assert!(why.contains("/boot/amd-ucode.img"));
        assert!(why.contains("not snapshotted"));
    }

    /// Only when the root is actually encrypted. On an unencrypted install,
    /// cryptsetup is an ordinary package.
    #[test]
    fn cryptsetup_is_refused_only_on_an_encrypted_root() {
        let files = vec!["/usr/bin/cryptsetup".to_string()];
        assert!(protected_reason("cryptsetup", &files, true).is_some());
        assert!(protected_reason("cryptsetup", &files, false).is_none());

        let hook = vec!["/usr/lib/initcpio/hooks/mountcrypt".to_string()];
        assert!(protected_reason("deploytix-hooks", &hook, true).is_some());
    }

    #[test]
    fn deploytixs_own_tooling_is_refused() {
        for pkg in ["pacman", "mkinitcpio", "grub", "btrfs-progs"] {
            assert!(
                protected_reason(pkg, &[], false).is_some(),
                "{pkg} must be refused"
            );
        }
        assert!(protected_reason("firefox", &[], false).is_none());
    }

    /// Reporting one blocker at a time makes people re-run until they give up.
    fn pf(name: &str, files: &[&str]) -> PackageFiles {
        PackageFiles {
            name: name.to_string(),
            files: files.iter().map(|f| f.to_string()).collect(),
            query_failed: false,
        }
    }

    #[test]
    fn every_blocked_package_is_reported_at_once() {
        let lists = vec![
            pf("firefox", &["/usr/bin/firefox"]),
            pf("linux", &["/usr/lib/modules/6.9/vmlinuz"]),
            pf("pacman", &[]),
        ];
        let err = check_protected(&lists, false).unwrap_err();
        assert_eq!(err.len(), 2, "both blockers reported: {err:?}");
        assert!(err.iter().any(|e| e.starts_with("linux:")));
        assert!(err.iter().any(|e| e.starts_with("pacman:")));
    }

    #[test]
    fn a_safe_package_passes_the_guard() {
        assert!(check_protected(&[pf("firefox", &["/usr/bin/firefox"])], true).is_ok());
    }

    /// If `pacman -Qlq` fails, every file-based rule has nothing to match and
    /// would report the package as safe. One of those rules is "this is a
    /// kernel, and removing it wipes the shared /boot for every rollback
    /// target", so an unanswered question has to block.
    #[test]
    fn a_package_whose_files_could_not_be_listed_is_blocked() {
        let lists = vec![PackageFiles {
            name: "mystery".to_string(),
            files: vec![],
            query_failed: true,
        }];
        let err = check_protected(&lists, false).unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].contains("could not list its files"), "{:?}", err);
    }

    /// But a package that genuinely owns nothing, like a meta-package, is not
    /// the same thing and must not be blocked by that rule.
    #[test]
    fn a_package_that_genuinely_owns_nothing_is_not_blocked() {
        assert!(check_protected(&[pf("some-meta-package", &[])], false).is_ok());
    }

    #[test]
    fn file_lists_round_trip() {
        let cmd = file_list_cmd(&["foo".into(), "bar".into()]);
        assert!(cmd.contains("pacman -Qlq foo"));
        assert!(cmd.contains("pacman -Qlq bar"));
        assert!(
            cmd.contains("printf 'ERR foo"),
            "a failed query must be marked"
        );

        let parsed = parse_file_lists("PKG foo\n/usr/bin/foo\n/usr/share/foo\nPKG bar\n/etc/bar\n");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "foo");
        assert_eq!(parsed[0].files, vec!["/usr/bin/foo", "/usr/share/foo"]);
        assert!(!parsed[0].query_failed);
        assert_eq!(parsed[1].name, "bar");
        assert_eq!(parsed[1].files, vec!["/etc/bar"]);
    }

    /// The `ERR` marker is what separates "owns nothing" from "could not ask".
    #[test]
    fn a_failed_query_is_marked_not_silently_empty() {
        let parsed = parse_file_lists("PKG good\n/usr/bin/good\nPKG broken\nERR broken\n");
        assert_eq!(parsed.len(), 2);
        assert!(!parsed[0].query_failed);
        assert!(parsed[1].query_failed, "the ERR line must be recorded");
        assert!(parsed[1].files.is_empty());
    }

    /// A package with no files still gets an entry. Without one the check
    /// would skip it.
    #[test]
    fn a_package_owning_nothing_still_appears() {
        let parsed = parse_file_lists("PKG base\nPKG foo\n/usr/bin/foo\n");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "base");
        assert!(parsed[0].files.is_empty());
        assert!(!parsed[0].query_failed);
    }

    /// The reflink is what makes the database backup free on btrfs.
    #[test]
    fn db_backup_reflinks_and_restore_is_reversible() {
        let b = db_backup_cmd("42");
        assert!(b.contains("cp -a --reflink=auto"));
        assert!(b.contains("/var/lib/pacman/local"));

        let r = db_restore_cmd("42");
        assert!(r.contains("/var/lib/pacman/local.deploytix-42"));
        // The live DB is moved aside before the backup takes its place, so an
        // interrupted restore never leaves both gone.
        let mv_aside = r.find(".failed").expect("live db is moved aside");
        let mv_back = r
            .find("mv /var/lib/pacman/local.deploytix-42")
            .expect("restored");
        assert!(mv_aside < mv_back);

        assert!(db_discard_cmd("42").contains("local.deploytix-42"));
    }

    fn valid_shell(script: &str) {
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
    fn generated_shell_is_valid() {
        valid_shell(&db_backup_cmd("1"));
        valid_shell(&db_restore_cmd("1"));
        valid_shell(&db_discard_cmd("1"));
        valid_shell(&file_list_cmd(&["foo".into(), "bar".into()]));
    }
}
