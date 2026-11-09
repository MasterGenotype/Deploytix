//! Upgrading every installed AUR package: `deploytix update --aur`.
//!
//! # What counts as outdated
//!
//! A package is a candidate when pacman calls it *foreign* (`pacman -Qm`: in
//! no configured sync database) and the AUR has a package of that name at a
//! version [`pkgdeps::version::vercmp`] orders after the installed one. That is
//! the same test `yay -Qua` and `paru -Qua` make, done here rather than handed
//! to a helper because the helpers do not agree on the flag (`-Sua`, `-Au`,
//! `--aur`), and because answering it in-process gives the transaction the
//! list up front: it can say what it is about to build, and hold back what it
//! must not.
//!
//! Foreign packages the AUR does not have — a locally built `.pkg.tar.zst`, a
//! package since dropped from the AUR — are left as installed and named in the
//! log. Asking a helper to build them would fail, and a failed build discards
//! the whole set, repository upgrades included.
//!
//! VCS packages (`-git` and friends) are only rebuilt when their AUR version
//! moves past the installed one. A local build stamps a newer `pkgver` than the
//! AUR's snapshot, so a `-git` package normally reads as up to date here, as it
//! does to a helper run without `--devel`.
//!
//! # What is held back
//!
//! Every build goes into one helper invocation, so anything that would fail
//! fails them all — after the kernels in the batch have finished compiling.
//! Four things are therefore settled before building, not after:
//!
//! * **`IgnorePkg`** in the set's `pacman.conf`. `pacman -Syu` honours it and
//!   so do the helpers; deploytix pins `gamescope-git` this way, and a pacman
//!   hook aborts any gamescope install that `deploytix-update-gamescope` did
//!   not start.
//! * **deploytix's own packages** ([`crate::install::CUSTOM_PACKAGE_NAMES`]).
//!   They come from the local `[deploytix]` repository, built from this tree
//!   or a vendored fork. On a host without that repository configured they
//!   read as foreign, and the AUR has packages by some of the same names that
//!   are different builds.
//! * **shared boot state.** A package whose *installed* version already owns
//!   files the guard in [`crate::immutable::remove::check_protected`] refuses
//!   (Xen's `/boot/xen.gz`, say) will own them again once rebuilt. The guard
//!   still runs after the build too, for files a new version adds.
//! * **source-signing keys** no keyserver has. A helper fetches a PKGBUILD's
//!   `validpgpkeys` from one keyserver, after downloading every source in the
//!   batch, and gives up on the lot when it cannot. [`crate::aur::keys`]
//!   imports them first, from more than one, and what is still missing holds
//!   its package back.
//!
//! A hold covers the whole package base. A helper builds a split package by
//! building its base, so `xen-docs` beside a held `xen` would compile all of
//! Xen to install the docs, and a kernel's `-headers` beside a pinned kernel
//! would move the headers past the kernel they are for. Every outdated
//! package sharing a base with a held one is held with it.
//!
//! # Where it runs
//!
//! Inside the set `deploytix update` has just run `pacman -Syu` in, and after
//! it: an AUR build links against whatever the set holds, so it should link
//! against the upgraded libraries, not the ones being replaced. The set is
//! queried rather than the running system because a composed update builds on
//! a staged set, whose AUR packages may already differ from what is booted.

use crate::aur::helper::AurHelper;
use crate::aur::install::build_in_target;
use crate::aur::keys;
use crate::aur::rpc::{AurRpc, CurlGet, HttpGet};
use crate::immutable::{history, remove};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use pkgdeps::model::Package;
use pkgdeps::version::vercmp;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use tracing::{info, warn};

/// An installed AUR package the AUR has a newer version of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outdated {
    pub name: String,
    /// The AUR package base it is built from; split packages share one.
    pub base: String,
    pub installed: String,
    pub available: String,
}

/// A foreign package left at its installed version on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub name: String,
    pub reason: String,
}

/// The installed foreign packages, sorted against what the AUR has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpgradePlan {
    /// Newer in the AUR: these are rebuilt.
    pub outdated: Vec<Outdated>,
    /// In the AUR at the installed version, or an older one.
    pub current: Vec<String>,
    /// Foreign, but not in the AUR: left as installed.
    pub not_in_aur: Vec<String>,
    /// Never taken from the AUR here; see the module docs.
    pub held: Vec<Held>,
}

impl UpgradePlan {
    /// The names to hand the helper.
    pub fn names(&self) -> Vec<String> {
        self.outdated.iter().map(|o| o.name.clone()).collect()
    }

    /// The package bases the helper will build, each once.
    pub fn bases(&self) -> Vec<String> {
        let mut bases: Vec<String> = self.outdated.iter().map(|o| o.base.clone()).collect();
        bases.sort();
        bases.dedup();
        bases
    }

    /// Move each outdated package `reason` names into [`Self::held`], and
    /// every outdated package built from the same base with it.
    fn hold_where(&mut self, reason: impl Fn(&Outdated) -> Option<String>) {
        let (held, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.outdated)
            .into_iter()
            .map(|o| (reason(&o), o))
            .partition(|(why, _)| why.is_some());
        let mut held: Vec<(String, Outdated)> = held
            .into_iter()
            .map(|(why, o)| (why.unwrap_or_default(), o))
            .collect();

        let held_bases: BTreeMap<String, String> = held
            .iter()
            .map(|(_, o)| (o.base.clone(), o.name.clone()))
            .collect();
        let (siblings, kept): (Vec<_>, Vec<_>) = kept
            .into_iter()
            .map(|(_, o)| o)
            .partition(|o| held_bases.contains_key(&o.base));
        held.extend(siblings.into_iter().map(|o| {
            let why = format!(
                "{} is held back, and both are built from package base {}",
                held_bases[&o.base], o.base
            );
            (why, o)
        }));

        self.outdated = kept;
        self.held.extend(held.into_iter().map(|(reason, o)| Held {
            name: o.name,
            reason,
        }));
    }

    /// Hold back outdated packages that `IgnorePkg` names or deploytix ships
    /// itself.
    pub fn hold_pinned(&mut self, ignored: &[String]) {
        self.hold_where(|o| hold_reason(&o.name, ignored));
    }

    /// Hold back outdated packages that fail the shared-state guard. `lists`
    /// are the file lists of their installed versions.
    pub fn hold_shared_state(&mut self, lists: &[remove::PackageFiles], encrypted: bool) {
        self.hold_where(|o| {
            let files = lists.iter().find(|l| l.name == o.name)?;
            let reasons = remove::check_protected(std::slice::from_ref(files), encrypted).err()?;
            // The guard prefixes each reason with the package, which the
            // `Held` already carries.
            let prefix = format!("{}: ", o.name);
            Some(
                reasons
                    .iter()
                    .map(|r| r.strip_prefix(&prefix).unwrap_or(r))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        });
    }

    /// Hold back outdated packages whose base verifies its sources against a
    /// key the build user's keyring lacks. `missing` maps a base to those keys.
    pub fn hold_unverifiable(&mut self, missing: &BTreeMap<String, Vec<String>>, user: &str) {
        self.hold_where(|o| {
            missing
                .get(&o.base)
                .map(|keys| keys::missing_reason(keys, user))
        });
    }
}

/// The in-chroot query for foreign packages, as `name version` lines.
///
/// pacman exits 1 when a query matches nothing, which here just means no
/// foreign packages are installed, so that status is not an error.
pub const FOREIGN_QUERY: &str = "pacman -Qm || true";

/// The `IgnorePkg` patterns in a `pacman.conf`.
///
/// Only the `[options]` section carries them. `Include`d files are not
/// followed: pacman allows `IgnorePkg` there, but deploytix never writes it
/// anywhere but the main file.
pub fn ignored_packages(pacman_conf: &str) -> Vec<String> {
    let mut in_options = false;
    let mut patterns = Vec::new();
    for line in pacman_conf.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_options = section.trim() == "options";
            continue;
        }
        if !in_options {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "IgnorePkg" {
                patterns.extend(value.split_whitespace().map(str::to_string));
            }
        }
    }
    patterns
}

/// Whether `name` matches an `IgnorePkg` pattern. pacman matches them with
/// `fnmatch(3)`; `*` and `?` are the wildcards anyone uses there.
fn glob_match(pattern: &[u8], name: &[u8]) -> bool {
    match (pattern.split_first(), name.split_first()) {
        (None, None) => true,
        (Some((b'*', rest)), _) => {
            glob_match(rest, name) || (!name.is_empty() && glob_match(pattern, &name[1..]))
        }
        (Some((b'?', rest)), Some((_, name_rest))) => glob_match(rest, name_rest),
        (Some((p, rest)), Some((n, name_rest))) if p == n => glob_match(rest, name_rest),
        _ => false,
    }
}

/// Why `name` is never taken from the AUR, or `None` if it may be.
fn hold_reason(name: &str, ignored: &[String]) -> Option<String> {
    if crate::install::CUSTOM_PACKAGE_NAMES.contains(&name) {
        return Some(
            "deploytix ships it in its own [deploytix] repository; the AUR's is a different build"
                .to_string(),
        );
    }
    ignored
        .iter()
        .find(|p| glob_match(p.as_bytes(), name.as_bytes()))
        .map(|p| format!("IgnorePkg = {p} in pacman.conf"))
}

/// Sort `installed` foreign packages against the AUR's metadata for them.
pub fn plan(installed: &BTreeMap<String, String>, aur: &[Package]) -> UpgradePlan {
    let available: BTreeMap<&str, &Package> = aur.iter().map(|p| (p.name.as_str(), p)).collect();

    let mut plan = UpgradePlan::default();
    for (name, version) in installed {
        match available.get(name.as_str()) {
            None => plan.not_in_aur.push(name.clone()),
            Some(aur) if vercmp(&aur.version, version) == Ordering::Greater => {
                plan.outdated.push(Outdated {
                    name: name.clone(),
                    base: aur.pkgbase().to_string(),
                    installed: version.clone(),
                    available: aur.version.clone(),
                })
            }
            Some(_) => plan.current.push(name.clone()),
        }
    }
    plan
}

/// Look `installed` up in the AUR, sort it, and hold back what is pinned.
///
/// A failed lookup is an error rather than an empty plan: an update asked to
/// upgrade AUR packages that quietly upgrades none of them looks like success
/// right up until someone checks.
pub fn check<H: HttpGet>(
    rpc: &AurRpc<H>,
    installed: &BTreeMap<String, String>,
    ignored: &[String],
) -> Result<UpgradePlan> {
    if installed.is_empty() {
        return Ok(UpgradePlan::default());
    }
    let names: Vec<&str> = installed.keys().map(String::as_str).collect();
    let aur = rpc.info(&names).map_err(|e| {
        DeploytixError::ConfigError(format!(
            "could not check the AUR for updates: {e}\n\
             Run `deploytix update` without --aur to upgrade from the repositories alone."
        ))
    })?;
    let mut plan = plan(installed, &aur);
    plan.hold_pinned(ignored);
    Ok(plan)
}

/// Sort `installed` against the AUR and hold back everything that must not be
/// built: [`check`], then the shared-state guard on what is left.
///
/// `run` answers a read-only query against the system being planned for (the
/// set, or the running system for a preview); `pacman_conf` is that system's.
pub fn plan_for<H: HttpGet>(
    rpc: &AurRpc<H>,
    installed: &BTreeMap<String, String>,
    pacman_conf: &str,
    run: impl Fn(&str) -> Result<String>,
) -> Result<UpgradePlan> {
    let ignored = ignored_packages(pacman_conf);
    let mut plan = check(rpc, installed, &ignored)?;
    if !plan.outdated.is_empty() {
        let lists = remove::parse_file_lists(&run(&remove::file_list_cmd(&plan.names()))?);
        let encrypted = std::path::Path::new(crate::immutable::ROOT_FS_DEVICE).exists();
        plan.hold_shared_state(&lists, encrypted);
    }
    Ok(plan)
}

/// The foreign packages installed on the running system.
pub fn installed_on_host() -> BTreeMap<String, String> {
    history::parse_package_list(&host_query(FOREIGN_QUERY))
}

/// What `deploytix update --aur` would rebuild if it ran now, worked out
/// against the running system. Read-only; the update GUI shows it before
/// anything is built. The transaction plans again inside the set it builds,
/// which differs from this only when an update is already staged.
pub fn preview_on_host() -> Result<UpgradePlan> {
    let installed = installed_on_host();
    let conf = std::fs::read_to_string("/etc/pacman.conf").unwrap_or_default();
    plan_for(&AurRpc::new(CurlGet), &installed, &conf, |s| {
        Ok(host_query(s))
    })
}

/// Run a read-only query on the running system. A failure reads as no output.
fn host_query(script: &str) -> String {
    std::process::Command::new("sh")
        .args(["-c", script])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Run a read-only query against the set at `target`.
///
/// A dry run mounts nothing, so it asks the running system instead: the
/// closest thing to what the set would hold, and harmless to read.
fn query(cmd: &CommandRunner, target: &str, script: &str) -> Result<String> {
    if cmd.is_dry_run() {
        return Ok(host_query(script));
    }
    Ok(match cmd.run_in_chroot(target, script)? {
        Some(out) => String::from_utf8_lossy(&out.stdout).to_string(),
        None => String::new(),
    })
}

/// The `pacman.conf` the set's pacman reads. Missing reads as empty: then
/// nothing is ignored, which is what pacman would do too.
fn read_pacman_conf(cmd: &CommandRunner, target: &str) -> String {
    let path = if cmd.is_dry_run() {
        "/etc/pacman.conf".to_string()
    } else {
        format!("{target}/etc/pacman.conf")
    };
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Rebuild every outdated AUR package in the set at `target`.
///
/// Returns what was rebuilt. Errors — the AUR unreachable, a build failing, a
/// new version that writes to `/boot` — propagate, which discards the set.
pub fn upgrade_in_target(
    cmd: &CommandRunner,
    target: &str,
    helper: AurHelper,
    build_user: &str,
) -> Result<Vec<String>> {
    let installed = history::parse_package_list(&query(cmd, target, FOREIGN_QUERY)?);
    if installed.is_empty() {
        info!("[aur] No AUR packages installed; nothing to upgrade");
        return Ok(Vec::new());
    }

    info!(
        "[aur] Checking {} foreign package(s) against the AUR",
        installed.len()
    );
    let mut plan = plan_for(
        &AurRpc::new(CurlGet),
        &installed,
        &read_pacman_conf(cmd, target),
        |s| query(cmd, target, s),
    )?;
    // Here rather than in `plan_for`: importing keys writes to the keyring,
    // and the GUI's preview of the plan must stay read-only.
    if !plan.outdated.is_empty() {
        let missing = keys::ensure_for_bases(cmd, target, build_user, &plan.bases())?;
        plan.hold_unverifiable(&missing, build_user);
    }

    if !plan.not_in_aur.is_empty() {
        info!(
            "[aur] Not in the AUR, left as installed: {}",
            plan.not_in_aur.join(", ")
        );
    }
    for h in &plan.held {
        warn!("[aur] Not upgrading {}: {}", h.name, h.reason);
    }
    if plan.outdated.is_empty() {
        info!(
            "[aur] No AUR package to upgrade ({} up to date)",
            plan.current.len()
        );
        return Ok(Vec::new());
    }

    info!("[aur] {} AUR package(s) to upgrade:", plan.outdated.len());
    for o in &plan.outdated {
        info!("[aur]   {} {} -> {}", o.name, o.installed, o.available);
    }
    let names = plan.names();
    build_in_target(cmd, target, helper, build_user, &names)?;
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aur::rpc::AurRpc;

    fn aur_pkg(name: &str, version: &str) -> Package {
        Package::new(name, version, crate::aur::rpc::AUR_REPO)
    }

    fn outdated(name: &str, base: &str, installed: &str, available: &str) -> Outdated {
        Outdated {
            name: name.into(),
            base: base.into(),
            installed: installed.into(),
            available: available.into(),
        }
    }

    fn installed(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn only_packages_the_aur_has_newer_are_rebuilt() {
        let plan = plan(
            &installed(&[
                ("hhd-git", "r900.abc-1"),
                ("zen-browser-bin", "1.0-1"),
                ("yay", "12.4.2-1"),
            ]),
            &[
                aur_pkg("hhd-git", "r800.def-1"),
                aur_pkg("zen-browser-bin", "1.1-1"),
                aur_pkg("yay", "12.4.2-1"),
            ],
        );
        assert_eq!(
            plan.outdated,
            vec![Outdated {
                name: "zen-browser-bin".into(),
                base: "zen-browser-bin".into(),
                installed: "1.0-1".into(),
                available: "1.1-1".into(),
            }]
        );
        // A local -git build is newer than the AUR's snapshot of it.
        assert_eq!(plan.current, vec!["hhd-git", "yay"]);
        assert!(plan.not_in_aur.is_empty());
    }

    /// A locally built package handed to a helper would fail its build, and a
    /// failed build throws away the whole set.
    #[test]
    fn foreign_packages_missing_from_the_aur_are_left_alone() {
        let plan = plan(
            &installed(&[("mullvad-vpn-runit", "1-1"), ("paru", "2.0-1")]),
            &[aur_pkg("paru", "2.1-1")],
        );
        assert_eq!(plan.names(), vec!["paru"]);
        assert_eq!(plan.not_in_aur, vec!["mullvad-vpn-runit"]);
    }

    #[test]
    fn an_epoch_bump_is_an_upgrade_even_to_a_lower_version() {
        let plan = plan(
            &installed(&[("foo", "2.0-1")]),
            &[aur_pkg("foo", "1:1.0-1")],
        );
        assert_eq!(plan.names(), vec!["foo"]);
    }

    #[test]
    fn a_pkgrel_bump_alone_is_an_upgrade() {
        let plan = plan(&installed(&[("foo", "1.0-1")]), &[aur_pkg("foo", "1.0-2")]);
        assert_eq!(plan.names(), vec!["foo"]);
    }

    struct Canned(std::result::Result<&'static str, &'static str>);

    impl HttpGet for Canned {
        fn get(&self, _url: &str) -> Result<String> {
            self.0
                .map(str::to_string)
                .map_err(|e| DeploytixError::CommandFailed {
                    command: "curl".to_string(),
                    stderr: e.to_string(),
                })
        }
    }

    const PARU_2_1: &str = r#"{"version":5,"type":"multiinfo","resultcount":1,
        "results":[{"Name":"paru","Version":"2.1-1"}]}"#;

    const NEWER_EVERYTHING: &str = r#"{"version":5,"type":"multiinfo","resultcount":3,
        "results":[{"Name":"paru","Version":"2.1-1"},
                   {"Name":"gamescope-git","Version":"3.16.24.r14.g42868877-1"},
                   {"Name":"linux-xanmod-bore","Version":"7.2.12-1"}]}"#;

    #[test]
    fn check_sorts_against_the_rpc_response() {
        let rpc = AurRpc::new(Canned(Ok(PARU_2_1)));
        let plan = check(
            &rpc,
            &installed(&[("paru", "2.0-1"), ("local-only", "1-1")]),
            &[],
        )
        .unwrap();
        assert_eq!(plan.names(), vec!["paru"]);
        assert_eq!(plan.not_in_aur, vec!["local-only"]);
    }

    /// The AUR's gamescope-git is upstream Valve's; deploytix's is a fork with
    /// different meson options, and a pacman hook aborts the transaction if
    /// anything else installs it.
    #[test]
    fn deploytix_own_packages_are_never_taken_from_the_aur() {
        let rpc = AurRpc::new(Canned(Ok(NEWER_EVERYTHING)));
        let plan = check(
            &rpc,
            &installed(&[("gamescope-git", "r2513.754b539-1"), ("paru", "2.0-1")]),
            &[],
        )
        .unwrap();
        assert_eq!(plan.names(), vec!["paru"]);
        assert_eq!(plan.held.len(), 1);
        assert_eq!(plan.held[0].name, "gamescope-git");
    }

    #[test]
    fn ignore_pkg_is_honoured_like_pacman_does() {
        let rpc = AurRpc::new(Canned(Ok(NEWER_EVERYTHING)));
        let plan = check(
            &rpc,
            &installed(&[("linux-xanmod-bore", "7.1.13-1"), ("paru", "2.0-1")]),
            &["linux-xanmod*".to_string()],
        )
        .unwrap();
        assert_eq!(plan.names(), vec!["paru"]);
        assert_eq!(plan.held[0].name, "linux-xanmod-bore");
        assert!(plan.held[0].reason.contains("IgnorePkg"));
    }

    /// A hold is only worth reporting when it holds something back.
    #[test]
    fn a_pinned_package_without_an_update_is_not_reported_as_held() {
        let rpc = AurRpc::new(Canned(Ok(PARU_2_1)));
        let plan = check(&rpc, &installed(&[("deploytix-git", "1.5.2-1")]), &[]).unwrap();
        assert!(plan.held.is_empty(), "{:?}", plan.held);
        assert_eq!(plan.not_in_aur, vec!["deploytix-git"]);
    }

    #[test]
    fn nothing_installed_makes_no_request() {
        let rpc = AurRpc::new(Canned(Err("must not be called")));
        assert_eq!(
            check(&rpc, &BTreeMap::new(), &[]).unwrap(),
            UpgradePlan::default()
        );
    }

    /// An unreachable AUR must fail the transaction, not stage a set that
    /// silently upgraded nothing.
    #[test]
    fn an_unreachable_aur_is_an_error_not_an_empty_plan() {
        let rpc = AurRpc::new(Canned(Err("Could not resolve host")));
        let err = check(&rpc, &installed(&[("paru", "2.0-1")]), &[]).unwrap_err();
        assert!(err.to_string().contains("without --aur"), "{err}");
    }

    /// Xen installs xen.gz into /boot. The post-build guard would refuse it —
    /// after every other package in the batch had finished compiling.
    #[test]
    fn a_package_that_already_owns_boot_files_is_held_before_building() {
        let mut plan = UpgradePlan {
            outdated: vec![
                outdated("xen", "xen", "4.19-1", "4.21-1"),
                outdated("yay", "yay", "13.0-1", "13.1-1"),
            ],
            ..Default::default()
        };
        let lists = remove::parse_file_lists(
            "PKG xen\n/boot/\n/boot/xen.gz\n/usr/bin/xl\nPKG yay\n/usr/bin/yay\n",
        );
        plan.hold_shared_state(&lists, false);
        assert_eq!(plan.names(), vec!["yay"]);
        assert_eq!(plan.held[0].name, "xen");
        assert!(plan.held[0].reason.contains("/boot"), "{:?}", plan.held);
        assert!(!plan.held[0].reason.starts_with("xen:"), "{:?}", plan.held);
    }

    /// Seen on a real host: xen held for /boot, xen-docs not, so yay compiled
    /// all of Xen to install the docs.
    #[test]
    fn a_split_sibling_of_a_package_held_for_boot_files_is_held_too() {
        let mut plan = UpgradePlan {
            outdated: vec![
                outdated("xen", "xen", "4.19-1", "4.21-1"),
                outdated("xen-docs", "xen", "4.19-1", "4.21-1"),
                outdated("yay", "yay", "13.0-1", "13.1-1"),
            ],
            ..Default::default()
        };
        let lists = remove::parse_file_lists(
            "PKG xen\n/boot/xen.gz\nPKG xen-docs\n/usr/share/doc/xen/\nPKG yay\n/usr/bin/yay\n",
        );
        plan.hold_shared_state(&lists, false);
        assert_eq!(plan.names(), vec!["yay"]);
        let docs = plan.held.iter().find(|h| h.name == "xen-docs").unwrap();
        assert!(docs.reason.contains("xen is held back"), "{:?}", docs);
    }

    /// Seen on a real host: no keyserver yay asked had the kernel's signing
    /// key, and yay gave up on every package in the batch.
    #[test]
    fn a_base_whose_signing_key_is_missing_is_held_with_its_siblings() {
        let mut plan = UpgradePlan {
            outdated: vec![
                outdated(
                    "linux-xanmod-bore",
                    "linux-xanmod-bore",
                    "7.1.13-1",
                    "7.2.12-1",
                ),
                outdated(
                    "linux-xanmod-bore-headers",
                    "linux-xanmod-bore",
                    "7.1.13-1",
                    "7.2.12-1",
                ),
                outdated("yay", "yay", "13.0-1", "13.1-1"),
            ],
            ..Default::default()
        };
        assert_eq!(plan.bases(), vec!["linux-xanmod-bore", "yay"]);
        let missing = BTreeMap::from([(
            "linux-xanmod-bore".to_string(),
            vec!["ABAF11C65A2970B130ABE3C479BE3E4300411886".to_string()],
        )]);
        plan.hold_unverifiable(&missing, "deck");
        assert_eq!(plan.names(), vec!["yay"]);
        let held: Vec<_> = plan.held.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(held, vec!["linux-xanmod-bore", "linux-xanmod-bore-headers"]);
        assert!(plan.held[0].reason.contains("ABAF11C6"), "{:?}", plan.held);
    }

    const XANMOD: &str = r#"{"version":5,"type":"multiinfo","resultcount":3,
        "results":[{"Name":"linux-xanmod-bore","PackageBase":"linux-xanmod-bore","Version":"7.2.12-1"},
                   {"Name":"linux-xanmod-bore-headers","PackageBase":"linux-xanmod-bore","Version":"7.2.12-1"},
                   {"Name":"paru","PackageBase":"paru","Version":"2.1-1"}]}"#;

    /// Pinning a kernel by name must not move its headers past it, or DKMS
    /// builds modules for a kernel that is not installed.
    #[test]
    fn ignore_pkg_on_a_kernel_holds_its_headers() {
        let rpc = AurRpc::new(Canned(Ok(XANMOD)));
        let plan = check(
            &rpc,
            &installed(&[
                ("linux-xanmod-bore", "7.1.13-1"),
                ("linux-xanmod-bore-headers", "7.1.13-1"),
                ("paru", "2.0-1"),
            ]),
            &["linux-xanmod-bore".to_string()],
        )
        .unwrap();
        assert_eq!(plan.names(), vec!["paru"]);
        let held: Vec<_> = plan.held.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(held, vec!["linux-xanmod-bore", "linux-xanmod-bore-headers"]);
    }

    /// Without a hold, split siblings are rebuilt together as usual.
    #[test]
    fn split_siblings_upgrade_together_when_nothing_holds_them() {
        let rpc = AurRpc::new(Canned(Ok(XANMOD)));
        let plan = check(
            &rpc,
            &installed(&[
                ("linux-xanmod-bore", "7.1.13-1"),
                ("linux-xanmod-bore-headers", "7.1.13-1"),
            ]),
            &[],
        )
        .unwrap();
        assert_eq!(
            plan.names(),
            vec!["linux-xanmod-bore", "linux-xanmod-bore-headers"]
        );
        assert!(plan.held.is_empty(), "{:?}", plan.held);
    }

    #[test]
    fn ignore_pkg_is_read_from_options_only() {
        let conf = "\
[options]
HoldPkg = pacman glibc
IgnorePkg = gamescope-git   # pinned by deploytix
IgnorePkg=foo bar
#IgnorePkg = commented
[extra]
IgnorePkg = not-an-option
";
        assert_eq!(ignored_packages(conf), vec!["gamescope-git", "foo", "bar"]);
    }

    #[test]
    fn ignore_globs_match_like_fnmatch() {
        assert!(glob_match(b"linux*", b"linux-xanmod-bore"));
        assert!(glob_match(b"*-git", b"hhd-git"));
        assert!(glob_match(b"x?n", b"xen"));
        assert!(!glob_match(b"linux", b"linux-headers"));
        assert!(!glob_match(b"*-git", b"hhd-ui"));
    }

    #[test]
    fn the_foreign_query_tolerates_an_empty_result() {
        assert!(FOREIGN_QUERY.contains("pacman -Qm"));
        assert!(FOREIGN_QUERY.ends_with("|| true"));
    }
}
