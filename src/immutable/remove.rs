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
//! The case that needs the most care is not obvious from the package name. A
//! kernel package owns `/usr/lib/modules/<version>/vmlinuz` and nothing under
//! `/boot`. The kernel image and initramfs get to `/boot` through pacman
//! hooks. Artix ships `60-mkinitcpio-remove.hook`:
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
//! Its script deletes `/boot/vmlinuz-<pkgbase>` and that kernel's initramfs
//! images — from the shared `/boot`, before pacman removes a single file, and
//! for every set at once. What it does *not* touch is any other kernel's
//! images. That is the whole basis of the kernel rule:
//!
//! - Removing a kernel while **another kernel stays installed** is allowed.
//!   The survivor's images are untouched, `mkinitcpio -P` rebuilds its
//!   initramfs from inside the set, and the grub.cfg regenerated on activation
//!   lists only kernels that still exist. Every set then boots the surviving
//!   kernel, which is no different from the documented shared-`/boot`
//!   behaviour after a kernel *update*: every set already boots whatever
//!   kernel `/boot` holds.
//! - Removing the **last** kernel is refused. It would leave the set, and
//!   every rollback target sharing `/boot`, with nothing to boot.
//!
//! A kernel is recognised by the file pattern the hook keys on, the same way
//! mkinitcpio does. A list of names would miss `linux-zen`, an `-lts` kernel,
//! or anything built locally.
//!
//! # The pacman database
//!
//! The database used to be on the shared `/var`, so a removal updated it for
//! every set at once while deleting files from the new set only: roll the
//! removal back and the files came back while the database still said the
//! package was gone. It now lives inside the set's `/usr` and is bind-mounted
//! onto `/var/lib/pacman` (see [`crate::immutable::pacman_db`]), so it rolls
//! back with the files, and a failed removal is unwound by discarding the set.
//!
//! [`db_backup_cmd`] and its companions remain for the one case that is still
//! possible: a set whose migration could not be completed, where pacman writes
//! the shared database directly. There the copy is what keeps a removal that
//! fails partway from leaving the running system disagreeing with its own
//! package manager.

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

/// Why the LVM A/B backend turns down a removal.
///
/// It could express the same operation (rsync the active root into the
/// inactive slot, `pacman -R` in a chroot, `veritysetup format` a fresh hash,
/// repoint), but that is not implemented — and silently running the btrfs path
/// on an A/B system would edit a root that is not the one that boots. The CLI
/// and the GUI both show this text so the two cannot disagree.
pub const LVM_AB_REFUSAL: &str = "`deploytix remove` is not implemented for the LVM A/B backend. \
     Remove packages by staging a full `deploytix update` into the inactive \
     slot instead.";

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
    /// package as safe, and the kernel rule could not tell whether it is the
    /// last kernel. An unanswered question is treated as a blocker rather than
    /// a pass.
    pub query_failed: bool,
}

impl PackageFiles {
    /// Whether this package is a kernel: it owns the file
    /// `60-mkinitcpio-remove.hook` keys on.
    pub fn is_kernel(&self) -> bool {
        self.files
            .iter()
            .any(|f| is_kernel_image(f.trim_start_matches('/')))
    }

    /// The `<version>` directories of the kernel images this package owns.
    /// Compared against `/proc/sys/kernel/osrelease` to spot the running one.
    fn kernel_releases(&self) -> Vec<String> {
        self.files
            .iter()
            .filter_map(|f| kernel_release_of(f.trim_start_matches('/')))
            .collect()
    }
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

/// Why `pkg` must not be removed on its own account, or `None` if it is safe.
///
/// `files` is the package's file list from `pacman -Qlq`, and `encrypted` says
/// whether the root filesystem is behind LUKS.
///
/// Most rules look at what a package owns rather than what it is called. The
/// destructive cases share no naming convention, and what makes them
/// dangerous is a file the pacman hooks watch for.
///
/// The kernel rule is not here: whether a kernel may go depends on what *else*
/// is installed, so it lives in [`vet_removal`], which sees the whole set.
pub fn protected_reason(pkg: &str, files: &[String], encrypted: bool) -> Option<String> {
    if let Some((_, why)) = CORE_TOOLING.iter().find(|(name, _)| *name == pkg) {
        return Some(format!("deploytix depends on it: {why}"));
    }

    // Owns files in /boot directly: microcode, memtest86+, bootloader payloads.
    // Which mounts a set shares with the live system, and which of those a
    // rollback cannot repair, is a property of the mount model rather than of
    // removal — see `crate::immutable::UNRECOVERABLE_SHARED_MOUNTS`.
    if let Some((f, mount)) = files
        .iter()
        .find_map(|f| crate::immutable::unrecoverable_shared_mount(f).map(|m| (f, m)))
    {
        return Some(format!(
            "it owns {f} in the shared {mount}, which is not snapshotted and cannot be rolled back"
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
    kernel_release_of(path).is_some()
}

/// The `<version>` of a kernel image path (`usr/lib/modules/<version>/vmlinuz`,
/// no leading slash), or `None` for anything else.
fn kernel_release_of(path: &str) -> Option<String> {
    let mut parts = path.split('/');
    if parts.next() != Some("usr") || parts.next() != Some("lib") || parts.next() != Some("modules")
    {
        return None;
    }
    let release = parts.next().filter(|v| !v.is_empty())?;
    if parts.next() == Some("vmlinuz") && parts.next().is_none() {
        Some(release.to_string())
    } else {
        None
    }
}

/// Check every package in a set for shared-state damage, returning all the
/// reasons it is refused rather than just the first. Reporting one blocker at
/// a time makes people re-run the command until they give up.
///
/// This is the per-package half of the guard. It does not include the kernel
/// rule, which needs to know what else is installed; removals get that from
/// [`vet_removal`]. An AUR *install* uses this on its own: a build that adds a
/// kernel is additive to `/boot` and harmless, but one that overwrites
/// microcode or bootloader payloads there is not.
pub fn check_protected(
    lists: &[PackageFiles],
    encrypted: bool,
) -> std::result::Result<(), Vec<String>> {
    let blocked: Vec<String> = lists
        .iter()
        .filter_map(|p| {
            if p.query_failed {
                return Some(format!(
                    "{}: could not list its files, so the /boot, encrypted-root and \
                     kernel checks could not be run",
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

/// A kernel package installed in the root being inspected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledKernel {
    /// Owning package, e.g. `linux-zen`.
    pub package: String,
    /// The `<version>` directory under `/usr/lib/modules`, which is what
    /// `uname -r` reports when that kernel is running.
    pub release: String,
}

/// Shell that names every installed kernel: for each kernel image on disk, the
/// package that owns it and the release it is for, as `KERNEL <pkg> <release>`.
///
/// Walks the filesystem rather than a package list because that is how the
/// mkinitcpio hooks find kernels too, and it needs no names to be known in
/// advance. An image no package owns — a leftover from a manual install — is
/// skipped: pacman would not remove it, and it has no preset to boot with.
pub fn installed_kernels_cmd() -> String {
    "set -u; for f in /usr/lib/modules/*/vmlinuz; do \
     [ -f \"$f\" ] || continue; \
     v=${f#/usr/lib/modules/}; v=${v%/vmlinuz}; \
     p=$(pacman -Qqo \"$f\" 2>/dev/null) || continue; \
     printf 'KERNEL %s %s\\n' \"$p\" \"$v\"; \
     done; true"
        .to_string()
}

/// Parse [`installed_kernels_cmd`] output.
pub fn parse_installed_kernels(out: &str) -> Vec<InstalledKernel> {
    out.lines()
        .filter_map(|line| {
            let mut fields = line.trim().strip_prefix("KERNEL ")?.split_whitespace();
            let package = fields.next()?.to_string();
            let release = fields.next()?.to_string();
            Some(InstalledKernel { package, release })
        })
        .collect()
}

/// The running kernel's release, as `uname -r` would report it.
pub fn running_kernel_release() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// What a removal would do, once resolved and vetted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemovalPlan {
    /// Every package pacman would remove, in pacman's order: the names asked
    /// for plus the dependencies `-Rs` (and `-Rc`) pull in.
    pub resolved: Vec<String>,
    /// Kernel packages among [`Self::resolved`].
    pub kernels_removed: Vec<String>,
    /// Kernel packages that stay installed afterwards.
    pub kernels_remaining: Vec<String>,
    /// The kernel the system is running right now is among the removed.
    pub removes_running_kernel: bool,
    /// Things worth knowing before confirming. None of them blocks.
    pub notes: Vec<String>,
}

impl RemovalPlan {
    /// Whether the plan takes a kernel out.
    pub fn removes_kernel(&self) -> bool {
        !self.kernels_removed.is_empty()
    }
}

/// Why a removal was turned down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveRefusal {
    /// pacman could not resolve the set: a name is not installed, or something
    /// still depends on it. Carries pacman's own message.
    Unresolvable(String),
    /// pacman resolved an empty set, so there is nothing to do.
    NothingToRemove,
    /// The set resolved, but contains packages the system cannot recover from
    /// losing. One reason per package.
    Protected(Vec<String>),
    /// A command needed to answer the question could not be run at all.
    Failed(String),
}

impl RemoveRefusal {
    /// The reasons as separate lines, for a UI that lists them.
    pub fn reasons(&self) -> Vec<String> {
        match self {
            Self::Protected(r) => r.clone(),
            other => vec![other.to_string()],
        }
    }
}

impl std::fmt::Display for RemoveRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unresolvable(why) => write!(
                f,
                "pacman refused the removal (something still depends on it, or it is not \
                 installed): {why}"
            ),
            Self::NothingToRemove => write!(f, "pacman resolved nothing to remove"),
            Self::Protected(reasons) => write!(
                f,
                "refusing to remove {} package(s) the system cannot recover from:\n  {}",
                reasons.len(),
                reasons.join("\n  ")
            ),
            Self::Failed(why) => write!(f, "could not work out what would be removed: {why}"),
        }
    }
}

impl From<RemoveRefusal> for DeploytixError {
    fn from(r: RemoveRefusal) -> Self {
        DeploytixError::ConfigError(r.to_string())
    }
}

/// Vet a resolved removal set.
///
/// `lists` is the file list of every package pacman would remove, `installed`
/// is every kernel in the root (removed or not), `running_release` is what the
/// booted kernel reports as `uname -r`, and `encrypted` says whether the root
/// is behind LUKS.
///
/// Every per-package rule from [`check_protected`] applies. On top of that:
///
/// - a kernel may be removed as long as **another kernel remains installed**
///   (see the module docs for why that is safe);
/// - removing the **last** kernel is refused;
/// - taking out a kernel, and in particular the running one, is reported in
///   [`RemovalPlan::notes`] so it can be confirmed knowingly.
///
/// Pure, so the rule can be tested without pacman or a chroot.
pub fn vet_removal(
    lists: &[PackageFiles],
    installed: &[InstalledKernel],
    running_release: Option<&str>,
    encrypted: bool,
) -> std::result::Result<RemovalPlan, RemoveRefusal> {
    let mut blocked = match check_protected(lists, encrypted) {
        Ok(()) => Vec::new(),
        Err(b) => b,
    };

    let resolved: Vec<String> = lists.iter().map(|p| p.name.clone()).collect();

    // A kernel is anything owning the hook's file, or anything the on-disk
    // inventory attributes an image to. Both sources agree on a healthy system;
    // taking the union means a truncated file list cannot hide one.
    let mut kernels_removed: Vec<String> = lists
        .iter()
        .filter(|p| p.is_kernel() || installed.iter().any(|k| k.package == p.name))
        .map(|p| p.name.clone())
        .collect();
    kernels_removed.dedup();

    let mut kernels_remaining: Vec<String> = installed
        .iter()
        .filter(|k| !resolved.contains(&k.package))
        .map(|k| k.package.clone())
        .collect();
    kernels_remaining.sort();
    kernels_remaining.dedup();

    if !kernels_removed.is_empty() && kernels_remaining.is_empty() {
        for k in &kernels_removed {
            blocked.push(format!(
                "{k}: it is the only kernel installed. Removing it deletes its image and \
                 initramfs from the shared /boot, leaving this set — and every rollback \
                 target, which boots from the same /boot — with nothing to boot. Install \
                 another kernel first."
            ));
        }
    }

    if !blocked.is_empty() {
        return Err(RemoveRefusal::Protected(blocked));
    }

    let removes_running_kernel = running_release.is_some_and(|running| {
        lists
            .iter()
            .filter(|p| kernels_removed.contains(&p.name))
            .any(|p| {
                p.kernel_releases().iter().any(|r| r == running)
                    || installed
                        .iter()
                        .any(|k| k.package == p.name && k.release == running)
            })
    });

    let mut notes = Vec::new();
    if !kernels_removed.is_empty() {
        notes.push(format!(
            "Removes kernel {}. Its kernel image and initramfs are deleted from the shared \
             /boot, so no snapshot — including older ones — can boot with it afterwards. \
             The next boot uses {}.",
            kernels_removed.join(", "),
            kernels_remaining.join(", ")
        ));
        if removes_running_kernel {
            notes.push(format!(
                "That includes the kernel running right now ({}). The running system is \
                 unaffected until you reboot.",
                running_release.unwrap_or("?")
            ));
        }
    }

    Ok(RemovalPlan {
        resolved,
        kernels_removed,
        kernels_remaining,
        removes_running_kernel,
        notes,
    })
}

/// Work out what a removal would take out, and refuse it if the resulting set
/// is not safe.
///
/// `exec` runs a shell string in the root being inspected and returns its
/// output, or `None` in dry-run. The transaction passes a closure that runs
/// inside the set's chroot; the GUI preview passes one that runs on the live
/// system. Everything either closure runs is read-only: `--print` runs no
/// transaction and takes no lock, and `-Q` only reads the database.
///
/// The check runs against the resolved set, not the names typed. `-Rs` pulls
/// in dependencies nothing else needs, and a kernel or microcode package
/// arriving that way does just as much damage as one named directly.
///
/// In dry-run the plan is empty rather than an error: there is nothing to
/// inspect, and the dry run is there to show the sequence of steps.
pub fn resolve_plan<E>(
    exec: E,
    names: &[String],
    opts: &RemoveOptions,
    running_release: Option<&str>,
    encrypted: bool,
) -> std::result::Result<RemovalPlan, RemoveRefusal>
where
    E: Fn(&str) -> Result<Option<std::process::Output>>,
{
    let preflight = preflight_cmd(names, opts);
    info!("[immutable] Resolving removal set: {}", preflight);
    let out = exec(&preflight).map_err(|e| match e {
        DeploytixError::CommandFailed { stderr, .. } => {
            RemoveRefusal::Unresolvable(stderr.trim().to_string())
        }
        other => RemoveRefusal::Unresolvable(other.to_string()),
    })?;

    let Some(out) = out else {
        return Ok(RemovalPlan::default()); // dry run
    };
    let resolved: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if resolved.is_empty() {
        return Err(RemoveRefusal::NothingToRemove);
    }

    let read = |script: &str| -> std::result::Result<String, RemoveRefusal> {
        exec(script)
            .map(|o| {
                o.map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default()
            })
            .map_err(|e| RemoveRefusal::Failed(e.to_string()))
    };
    let lists = parse_file_lists(&read(&file_list_cmd(&resolved))?);
    let installed = parse_installed_kernels(&read(&installed_kernels_cmd())?);

    vet_removal(&lists, &installed, running_release, encrypted)
}

/// What the live system knows that [`resolve_plan`] needs: the running kernel
/// and whether the root is encrypted. The same whether the plan is being made
/// for a chroot or for a preview, because the set is a snapshot of this system.
fn live_context() -> (Option<String>, bool) {
    (
        running_kernel_release(),
        std::path::Path::new(crate::immutable::ROOT_FS_DEVICE).exists(),
    )
}

/// Preview a removal against the running system, without a chroot.
///
/// For the GUI: read-only, so it needs no transaction and no confirmation. The
/// transaction re-resolves inside the set it builds — which differs from the
/// live system when an update is already staged — so this is advisory, and
/// the transaction's own answer is the one that counts.
pub fn preview_removal(
    cmd: &CommandRunner,
    names: &[String],
    opts: &RemoveOptions,
) -> std::result::Result<RemovalPlan, RemoveRefusal> {
    if names.is_empty() {
        return Err(RemoveRefusal::NothingToRemove);
    }
    let (running, encrypted) = live_context();
    resolve_plan(
        |script| cmd.run("sh", &["-c", script]),
        names,
        opts,
        running.as_deref(),
        encrypted,
    )
}

/// The text shown before a removal is committed, on the CLI and in the GUI.
pub fn confirmation_text(plan: &RemovalPlan) -> String {
    let mut s = format!(
        "Remove {} package(s) from a new snapshot set?\n  {}\n",
        plan.resolved.len(),
        plan.resolved.join("\n  ")
    );
    for note in &plan.notes {
        s.push_str(&format!("\u{26a0} {note}\n"));
    }
    s.push_str("The running system is untouched until you reboot.");
    s
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
            let (running, encrypted) = live_context();
            let plan = resolve_plan(
                |script| cmd.run_in_chroot(target, script),
                &names,
                &ropts,
                running.as_deref(),
                encrypted,
            )?;
            if !plan.resolved.is_empty() {
                info!(
                    "[immutable] Removing {} package(s): {}",
                    plan.resolved.len(),
                    plan.resolved.join(" ")
                );
            }
            for note in &plan.notes {
                warn!("[immutable] {}", note);
            }

            if !assume_yes
                && !cmd.is_dry_run()
                && !crate::utils::prompt::prompt_confirm(&confirmation_text(&plan), false)?
            {
                return Err(DeploytixError::ConfigError("removal cancelled".to_string()));
            }

            // A set with its own database (the normal case since
            // `pacman_db`) needs no unwinding: the database lives inside the
            // set, so discarding the set discards it too, and the shared
            // /var/lib/pacman the running system uses is never written.
            //
            // The copy below is for the other case — a set that could not be
            // migrated, whose pacman writes straight into the shared /var. A
            // failure after this point would otherwise leave that database
            // saying the package is gone while the running system still has
            // every file of it.
            let own_db = crate::immutable::pacman_db::target_uses_own_db(target);
            if !own_db {
                cmd.run("sh", &["-c", &db_backup_cmd(set_id)])?;
            }

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
                if !own_db {
                    warn!("[immutable] Removal failed; restoring the pacman database");
                    if let Err(re) = cmd.run("sh", &["-c", &db_restore_cmd(set_id)]) {
                        warn!(
                            "[immutable] Could not restore the pacman database: {}. \
                     A copy is at {}",
                            re,
                            db_backup_path(set_id)
                        );
                    }
                }
                return Err(e);
            }

            let after = history::query_packages(cmd, target);
            let changes = history::diff(&before, &after);

            // pacman exits 0 on a transaction that removed nothing, so a removal
            // that quietly did nothing would look identical to a successful one
            // right up until the reboot.
            if !cmd.is_dry_run() && changes.removed.is_empty() {
                if !own_db {
                    let _ = cmd.run("sh", &["-c", &db_restore_cmd(set_id)]);
                }
                return Err(DeploytixError::ConfigError(format!(
                    "pacman reported success but removed nothing; requested: {}",
                    names.join(" ")
                )));
            }

            if !own_db {
                let _ = cmd.run("sh", &["-c", &db_discard_cmd(set_id)]);
            }
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

    fn kernel(name: &str, release: &str) -> PackageFiles {
        PackageFiles {
            name: name.to_string(),
            files: vec![
                format!("/usr/lib/modules/{release}/vmlinuz"),
                format!("/usr/lib/modules/{release}/modules.dep"),
            ],
            query_failed: false,
        }
    }

    fn installed(k: &[(&str, &str)]) -> Vec<InstalledKernel> {
        k.iter()
            .map(|(p, r)| InstalledKernel {
                package: p.to_string(),
                release: r.to_string(),
            })
            .collect()
    }

    /// The rule that makes kernel removal possible at all: the hook only
    /// deletes the removed kernel's images, so as long as another kernel stays
    /// installed every set still has something to boot.
    #[test]
    fn a_kernel_may_go_when_another_remains_however_it_is_named() {
        for name in ["linux", "linux-lts", "linux-zen", "my-custom-kernel"] {
            let plan = vet_removal(
                &[kernel(name, "6.9.1-artix1-1")],
                &installed(&[(name, "6.9.1-artix1-1"), ("linux-other", "6.10.0-1")]),
                None,
                false,
            )
            .unwrap_or_else(|e| panic!("{name} must be allowed with another kernel present: {e}"));
            assert_eq!(plan.kernels_removed, vec![name.to_string()]);
            assert_eq!(plan.kernels_remaining, vec!["linux-other".to_string()]);
            assert!(plan.removes_kernel());
            assert!(
                plan.notes
                    .iter()
                    .any(|n| n.contains("next boot uses linux-other")),
                "{:?}",
                plan.notes
            );
        }
    }

    /// ...and the limit of that rule: the last kernel leaves nothing to boot.
    #[test]
    fn the_last_kernel_is_refused() {
        let err = vet_removal(
            &[kernel("linux", "6.9.1-artix1-1")],
            &installed(&[("linux", "6.9.1-artix1-1")]),
            None,
            false,
        )
        .unwrap_err();
        let RemoveRefusal::Protected(reasons) = err else {
            panic!("expected a protection refusal, got {err:?}");
        };
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].starts_with("linux:"), "{}", reasons[0]);
        assert!(reasons[0].contains("only kernel"), "{}", reasons[0]);
    }

    /// Removing both installed kernels in one go is the same as removing the
    /// last one: nothing survives.
    #[test]
    fn removing_every_kernel_at_once_is_refused() {
        let err = vet_removal(
            &[
                kernel("linux", "6.9.1-1"),
                kernel("linux-zen", "6.9.1-zen1-1"),
            ],
            &installed(&[("linux", "6.9.1-1"), ("linux-zen", "6.9.1-zen1-1")]),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(err.reasons().len(), 2, "{err:?}");
    }

    /// The kernel inventory names the kernel even when its file list did not
    /// arrive, so a truncated `pacman -Qlq` cannot make a kernel look like an
    /// ordinary package.
    #[test]
    fn the_inventory_identifies_a_kernel_whose_file_list_is_bare() {
        let err = vet_removal(
            &[pf("linux", &["/usr/share/licenses/linux/LICENSE"])],
            &installed(&[("linux", "6.9.1-1")]),
            None,
            false,
        )
        .unwrap_err();
        assert!(matches!(err, RemoveRefusal::Protected(_)), "{err:?}");
    }

    #[test]
    fn removing_the_running_kernel_is_noted_but_allowed() {
        let plan = vet_removal(
            &[kernel("linux", "6.9.1-artix1-1")],
            &installed(&[("linux", "6.9.1-artix1-1"), ("linux-zen", "6.9.1-zen1-1")]),
            Some("6.9.1-artix1-1"),
            false,
        )
        .expect("allowed");
        assert!(plan.removes_running_kernel);
        assert!(
            plan.notes.iter().any(|n| n.contains("running right now")),
            "{:?}",
            plan.notes
        );

        let other = vet_removal(
            &[kernel("linux", "6.9.1-artix1-1")],
            &installed(&[("linux", "6.9.1-artix1-1"), ("linux-zen", "6.9.1-zen1-1")]),
            Some("6.9.1-zen1-1"),
            false,
        )
        .expect("allowed");
        assert!(!other.removes_running_kernel);
    }

    /// A removal that touches no kernel has nothing to say about kernels.
    #[test]
    fn an_ordinary_removal_has_no_kernel_notes() {
        let plan = vet_removal(
            &[pf("firefox", &["/usr/bin/firefox"])],
            &installed(&[("linux", "6.9.1-1")]),
            Some("6.9.1-1"),
            true,
        )
        .expect("allowed");
        assert!(!plan.removes_kernel());
        assert!(plan.notes.is_empty(), "{:?}", plan.notes);
        assert_eq!(plan.kernels_remaining, vec!["linux".to_string()]);
        assert_eq!(plan.resolved, vec!["firefox".to_string()]);
    }

    #[test]
    fn installed_kernels_round_trip() {
        let parsed = parse_installed_kernels(
            "KERNEL linux 6.9.1-artix1-1\nKERNEL linux-zen 6.9.1-zen1-1\nnoise\nKERNEL broken\n",
        );
        assert_eq!(
            parsed,
            installed(&[("linux", "6.9.1-artix1-1"), ("linux-zen", "6.9.1-zen1-1")])
        );
        assert!(parse_installed_kernels("").is_empty());
    }

    #[test]
    fn confirmation_lists_the_set_and_the_notes() {
        let plan = RemovalPlan {
            resolved: vec!["linux".into(), "linux-headers".into()],
            notes: vec!["Removes kernel linux.".into()],
            ..Default::default()
        };
        let text = confirmation_text(&plan);
        assert!(text.contains("Remove 2 package(s)"));
        assert!(text.contains("  linux\n  linux-headers\n"));
        assert!(text.contains("Removes kernel linux."));
        assert!(text.ends_with("until you reboot."));
    }

    /// `resolve_plan` in dry-run: the exec returns `None`, and the plan must be
    /// empty rather than an error so the dry run can carry on to the next step.
    #[test]
    fn a_dry_run_resolves_to_an_empty_plan() {
        let plan = resolve_plan(|_| Ok(None), &["foo".into()], &opts(), None, false).unwrap();
        assert!(plan.resolved.is_empty());
        assert!(!plan.removes_kernel());
    }

    /// A pacman failure is pacman's message, not a stack of wrappers.
    #[test]
    fn pacmans_refusal_is_surfaced_as_unresolvable() {
        let err = resolve_plan(
            |_| {
                Err(DeploytixError::CommandFailed {
                    command: "pacman".into(),
                    stderr: "error: target not found: nope\n".into(),
                })
            },
            &["nope".into()],
            &opts(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(
            err,
            RemoveRefusal::Unresolvable("error: target not found: nope".into())
        );
        assert!(err.to_string().contains("target not found"));
    }

    /// End to end against canned command output: a kernel removal that leaves
    /// another kernel installed resolves to a plan with the right notes.
    #[test]
    fn resolve_plan_threads_the_three_queries_together() {
        let exec = |script: &str| -> Result<Option<std::process::Output>> {
            use std::os::unix::process::ExitStatusExt;
            let stdout = if script.contains("--print") {
                "linux\nlinux-headers\n"
            } else if script.contains("-Qlq") {
                "PKG linux\n/usr/lib/modules/6.9.1-1/vmlinuz\nPKG linux-headers\n/usr/src/x\n"
            } else {
                "KERNEL linux 6.9.1-1\nKERNEL linux-zen 6.9.1-zen1-1\n"
            };
            Ok(Some(std::process::Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            }))
        };
        let plan = resolve_plan(exec, &["linux".into()], &opts(), Some("6.9.1-1"), false)
            .expect("allowed");
        assert_eq!(plan.resolved, vec!["linux", "linux-headers"]);
        assert_eq!(plan.kernels_removed, vec!["linux"]);
        assert_eq!(plan.kernels_remaining, vec!["linux-zen"]);
        assert!(plan.removes_running_kernel);
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
        // The last kernel and a core tool, in one request: both must be named.
        let lists = vec![
            pf("firefox", &["/usr/bin/firefox"]),
            pf("linux", &["/usr/lib/modules/6.9/vmlinuz"]),
            pf("pacman", &[]),
        ];
        let err = vet_removal(&lists, &installed(&[("linux", "6.9")]), None, false).unwrap_err();
        let reasons = err.reasons();
        assert_eq!(reasons.len(), 2, "both blockers reported: {reasons:?}");
        assert!(reasons.iter().any(|e| e.starts_with("linux:")));
        assert!(reasons.iter().any(|e| e.starts_with("pacman:")));
    }

    /// The per-package guard on its own no longer says anything about kernels:
    /// an AUR build that installs one only adds to /boot.
    #[test]
    fn the_per_package_guard_does_not_refuse_a_kernel() {
        assert!(check_protected(
            &[pf("linux-tkg", &["/usr/lib/modules/6.9-tkg/vmlinuz"])],
            false
        )
        .is_ok());
    }

    #[test]
    fn a_safe_package_passes_the_guard() {
        assert!(check_protected(&[pf("firefox", &["/usr/bin/firefox"])], true).is_ok());
    }

    /// If `pacman -Qlq` fails, every file-based rule has nothing to match and
    /// would report the package as safe, and the kernel rule could not tell
    /// whether the package is a kernel at all. An unanswered question blocks.
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
        assert!(vet_removal(&lists, &[], None, false).is_err());
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
        valid_shell(&installed_kernels_cmd());
    }

    /// The inventory must ask pacman who owns each image, not guess from the
    /// directory name, and must survive a machine with no kernel image at all
    /// (the glob then matches nothing that exists).
    #[test]
    fn the_kernel_inventory_asks_pacman_and_tolerates_an_empty_glob() {
        let s = installed_kernels_cmd();
        assert!(s.contains("pacman -Qqo"));
        assert!(s.contains("/usr/lib/modules/*/vmlinuz"));
        assert!(s.contains("[ -f \"$f\" ] || continue"));
        assert!(s.ends_with("true"), "must exit 0 even with nothing found");
    }
}
