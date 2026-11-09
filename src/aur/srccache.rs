//! The shared source cache, kept from serving a file the PKGBUILD no longer
//! means.
//!
//! # Why
//!
//! Every build downloads into one `SRCDEST` ([`crate::aur::build`]), on `/var`,
//! so a download outlives the transaction that made it and a failed run does
//! not fetch everything again. makepkg finds a cached source by its file name
//! alone, and a file already there is never fetched again. A PKGBUILD that
//! keeps a name across versions therefore gets the old file and fails its
//! checksum. `linux-xanmod-bore` 7.3.3 fetches
//! `…/linux-xanmod-bore/7.3/0001-bore.patch`; a failed 7.2.12 build had cached
//! `…/7.2/0001-bore.patch` under the same name, and every later
//! `deploytix update --aur` failed `One or more files did not pass the
//! validity check!`.
//!
//! # What this does
//!
//! Before building, checks each cached source the new `.SRCINFO` names
//! against the checksum it now publishes, and removes the ones that differ so
//! makepkg fetches them again. Matching files stay, so the kernel tarball is
//! not downloaded again just because a patch changed.
//!
//! Not covered: two package bases in one batch that name different files the
//! same. The first build's download is what the second finds, and nothing run
//! before either can tell which one should win.

use crate::aur::build::BUILD_ROOT;
use crate::aur::srcinfo;
use crate::utils::command::CommandRunner;
use std::collections::BTreeMap;
use std::path::Path;
use tracing::{info, warn};

/// makepkg's checksum arrays, strongest first, with the tool that computes
/// each. Only the strongest present is checked: a file that matches it is the
/// file the PKGBUILD means. `cksums` (CRC) is left out as too weak to say so.
pub const ALGORITHMS: &[(&str, &str)] = &[
    ("b2", "b2sum"),
    ("sha512", "sha512sum"),
    ("sha384", "sha384sum"),
    ("sha256", "sha256sum"),
    ("sha224", "sha224sum"),
    ("sha1", "sha1sum"),
    ("md5", "md5sum"),
];

/// A cached source a `.SRCINFO` names, with the checksum it must have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    /// The file name in `SRCDEST`.
    pub file: String,
    /// The tool from [`ALGORITHMS`] that computes `sum`.
    pub tool: &'static str,
    pub sum: String,
}

/// The name makepkg caches `source` under, or `None` for a source it does not
/// cache in `SRCDEST` as a file: a local file, which sits beside the PKGBUILD,
/// or a VCS checkout, which is a directory.
///
/// Mirrors makepkg's `get_filename` and `get_protocol`.
pub fn cached_name(source: &str) -> Option<String> {
    let (name, url) = match source.split_once("::") {
        Some((name, url)) => (name, url),
        None => (source, source),
    };
    let proto = url.split_once("://")?.0;
    let proto = proto.split('+').next().unwrap_or(proto);
    if matches!(proto, "bzr" | "fossil" | "git" | "hg" | "svn") {
        return None;
    }
    let file = name.rsplit('/').next().unwrap_or(name);
    let file = file.split('#').next().unwrap_or(file);
    // Joined to SRCDEST below; makepkg would not produce these, and a
    // `.SRCINFO` is anyone's to edit.
    if file.is_empty() || file == "." || file == ".." {
        return None;
    }
    Some(file.to_string())
}

/// The cached sources `srcinfo` names for `arch`, each with the checksum it
/// must now have. Sources whose checksum is `SKIP` are left out.
pub fn expected(srcinfo: &str, arch: &str) -> Vec<Expected> {
    let mut lists: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, value) in srcinfo::fields(srcinfo) {
        lists
            .entry(key.to_string())
            .or_default()
            .push(value.to_string());
    }
    let mut out = Vec::new();
    // `source` pairs with `sha256sums`, `source_x86_64` with
    // `sha256sums_x86_64`, by position.
    for suffix in [String::new(), format!("_{arch}")] {
        let Some(sources) = lists.get(&format!("source{suffix}")) else {
            continue;
        };
        let Some((tool, sums)) = ALGORITHMS.iter().find_map(|(alg, tool)| {
            let sums = lists.get(&format!("{alg}sums{suffix}"))?;
            Some((*tool, sums))
        }) else {
            continue;
        };
        if sums.len() != sources.len() {
            continue;
        }
        for (source, sum) in sources.iter().zip(sums) {
            if sum == "SKIP" {
                continue;
            }
            if let Some(file) = cached_name(source) {
                out.push(Expected {
                    file,
                    tool,
                    sum: sum.to_ascii_lowercase(),
                });
            }
        }
    }
    out
}

/// `SRCDEST`, as [`crate::aur::build::build_env`] sets it.
fn srcdest() -> String {
    format!("{BUILD_ROOT}/src")
}

/// Remove from `SRCDEST` each cached source of `srcinfos` whose checksum is
/// not the one its `.SRCINFO` now publishes. Returns the files removed.
///
/// Runs on the host: `SRCDEST` is on `/var`, which is the same in every set.
pub fn prune(cmd: &CommandRunner, srcinfos: &BTreeMap<String, String>) -> Vec<String> {
    let dir = srcdest();
    let mut removed = Vec::new();
    for (base, text) in srcinfos {
        for e in expected(text, std::env::consts::ARCH) {
            let path = Path::new(&dir).join(&e.file);
            // A regular file only: never follow a link out of the cache.
            if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                continue;
            }
            let path_str = path.to_string_lossy().to_string();
            let actual = match cmd.run(e.tool, &[&path_str]) {
                Ok(Some(out)) => String::from_utf8_lossy(&out.stdout)
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
                // A dry run computes nothing and so removes nothing.
                Ok(None) => continue,
                Err(err) => {
                    warn!("[aur] Could not checksum cached {}: {err}", e.file);
                    continue;
                }
            };
            if actual == e.sum {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    info!(
                        "[aur] Removed cached {} for {base}: it is not the file the \
                         PKGBUILD now names, so makepkg fetches it again",
                        e.file
                    );
                    removed.push(e.file);
                }
                Err(err) => warn!("[aur] Could not remove stale {}: {err}", path.display()),
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    const XANMOD: &str = "\
pkgbase = linux-xanmod-bore
\tpkgver = 7.3.3
\tsource = https://cdn.kernel.org/pub/linux/kernel/v7.x/linux-7.3.tar.xz
\tsource = https://cdn.kernel.org/pub/linux/kernel/v7.x/linux-7.3.tar.sign
\tsource = https://raw.githubusercontent.com/micros24/linux-xanmod-bore/7.3/0001-bore.patch
\tsource = config
\tsha256sums = AAAA
\tsha256sums = SKIP
\tsha256sums = bbbb
\tsha256sums = cccc
\tb2sums = 1111
\tb2sums = SKIP
\tb2sums = 2222
\tb2sums = 3333

pkgname = linux-xanmod-bore
";

    fn exp(file: &str, tool: &'static str, sum: &str) -> Expected {
        Expected {
            file: file.into(),
            tool,
            sum: sum.into(),
        }
    }

    /// The strongest sum is the one checked; local files and SKIP are not.
    #[test]
    fn expected_pairs_sources_with_the_strongest_sums() {
        assert_eq!(
            expected(XANMOD, "x86_64"),
            vec![
                exp("linux-7.3.tar.xz", "b2sum", "1111"),
                exp("0001-bore.patch", "b2sum", "2222"),
            ]
        );
    }

    #[test]
    fn arch_specific_sources_pair_with_their_own_sums() {
        let text = "\
\tsource_x86_64 = https://h/app-x86_64.tar.xz
\tsha256sums_x86_64 = ABCD
\tsource_aarch64 = https://h/app-aarch64.tar.xz
\tsha256sums_aarch64 = EF01
";
        assert_eq!(
            expected(text, "x86_64"),
            vec![exp("app-x86_64.tar.xz", "sha256sum", "abcd")]
        );
    }

    /// A count mismatch means the pairing is unknown; better to leave the
    /// cache than remove the wrong file.
    #[test]
    fn mismatched_counts_check_nothing() {
        let text = "\tsource = https://h/a\n\tsource = https://h/b\n\tsha256sums = 00\n";
        assert!(expected(text, "x86_64").is_empty());
    }

    #[test]
    fn cached_name_follows_makepkg() {
        assert_eq!(
            cached_name("https://h/x/0001-bore.patch").as_deref(),
            Some("0001-bore.patch")
        );
        assert_eq!(
            cached_name("bore-7.3.patch::https://h/x/0001-bore.patch").as_deref(),
            Some("bore-7.3.patch")
        );
        assert_eq!(
            cached_name("https://h/f.tar.gz#sig").as_deref(),
            Some("f.tar.gz")
        );
        assert_eq!(cached_name("config"), None);
        assert_eq!(cached_name("hhd::git+https://github.com/hhd-dev/hhd"), None);
        assert_eq!(cached_name("git://h/repo.git"), None);
        assert_eq!(cached_name("svn+https://h/trunk"), None);
    }

    #[test]
    fn a_name_that_would_leave_the_cache_is_ignored() {
        assert_eq!(cached_name("..::https://h/x"), None);
        assert_eq!(cached_name("https://h/"), None);
        assert_eq!(
            cached_name("../../etc/passwd::https://h/x").as_deref(),
            Some("passwd")
        );
    }
}
