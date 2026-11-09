//! Room on the build root for a batch, checked before the helper starts.
//!
//! # Why
//!
//! Every build goes to [`BUILD_ROOT`], on `/var`. A build that fills `/var`
//! fails partway through, and only after everything before it in the batch has
//! been downloaded and compiled. The transaction then throws the set away,
//! repository upgrades and every other build included, and with `/var` full
//! deploytix cannot even record what happened. `linux-xanmod-bore` 7.3.3 did
//! this to a `deploytix update --aur` partway through compiling, on a 50 GiB
//! `/var`.
//!
//! # What this does
//!
//! Before building, estimates what each package base needs, from its
//! `.SRCINFO` and the installed size of its packages, and compares that with
//! what is free on the filesystem holding [`BUILD_ROOT`]. A base that cannot
//! fit goes back to the caller, which decides: `deploytix update --aur` holds
//! it back and builds the rest, and `deploytix aur` refuses before building.
//!
//! The estimate is a floor, not a forecast:
//!
//! * **A kernel** needs [`KERNEL_BUILD`]. AUR kernels build a distribution
//!   config with debug info (DWARF, and BTF, which needs it), so the object
//!   tree runs to many times the installed size.
//!   `linux-xanmod-bore` 7.3.3's tree had reached 9.8 GiB with `drivers/`
//!   still compiling.
//! * **Anything else** needs twice the installed size of its packages: once
//!   in the package directory and again, compressed, in `PKGDEST`. That
//!   leaves out the sources and anything compiled from them, so it errs
//!   towards letting a build start.
//!
//! Each base is judged on its own, against all the free space. A batch of
//! bases that each fit can still fill the disk together; holding any of them
//! for that would mean guessing whether the helper clears each tree after
//! packaging it.

use crate::aur::build::BUILD_ROOT;
use crate::aur::rpc::is_valid_package_name;
use crate::aur::srcinfo;
use crate::utils::error::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const GIB: u64 = 1 << 30;

/// What a kernel base needs free to build.
pub const KERNEL_BUILD: u64 = 20 * GIB;

/// Left free beside any build, so a build that only just fits does not
/// leave `/var` full for everything else that writes there.
pub const RESERVE: u64 = GIB;

/// A base the build root has no room for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortfall {
    /// Bytes the base needs free, [`RESERVE`] included.
    pub need: u64,
    /// Bytes free when it was checked.
    pub free: u64,
    pub kernel: bool,
}

impl Shortfall {
    /// Why the base is not built, as a user-facing message.
    pub fn reason(&self) -> String {
        let what = if self.kernel {
            " (a kernel builds with debug info)"
        } else {
            ""
        };
        format!(
            "building it needs about {} free on the filesystem holding {BUILD_ROOT}{what}, \
             and {} is",
            gib(self.need),
            gib(self.free)
        )
    }
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB as f64)
}

/// The `pkgname`s a `.SRCINFO` builds.
fn pkgnames(srcinfo: &str) -> Vec<&str> {
    srcinfo::fields(srcinfo)
        .filter(|(key, _)| *key == "pkgname")
        .map(|(_, value)| value)
        .collect()
}

/// Whether a `.SRCINFO` builds a kernel: it packages the headers for one,
/// `linux*-headers`, as every AUR kernel does.
pub fn is_kernel(srcinfo: &str) -> bool {
    pkgnames(srcinfo)
        .iter()
        .any(|n| n.starts_with("linux") && n.ends_with("-headers"))
}

/// What the base `srcinfo` describes needs free to build. `installed` maps
/// package names to their installed size in bytes; a package not in it counts
/// as nothing.
pub fn need(srcinfo: &str, installed: &BTreeMap<String, u64>) -> u64 {
    if is_kernel(srcinfo) {
        return KERNEL_BUILD + RESERVE;
    }
    let size: u64 = pkgnames(srcinfo)
        .iter()
        .filter_map(|n| installed.get(*n))
        .sum();
    size.saturating_mul(2).saturating_add(RESERVE)
}

/// The bases of `srcinfos` that need more than `free` bytes.
pub fn shortfalls(
    srcinfos: &BTreeMap<String, String>,
    installed: &BTreeMap<String, u64>,
    free: u64,
) -> BTreeMap<String, Shortfall> {
    srcinfos
        .iter()
        .filter_map(|(base, text)| {
            let need = need(text, installed);
            (need > free).then(|| {
                let short = Shortfall {
                    need,
                    free,
                    kernel: is_kernel(text),
                };
                (base.clone(), short)
            })
        })
        .collect()
}

/// A size as pacman prints it, `2.93 GiB`, in bytes.
pub fn parse_size(text: &str) -> Option<u64> {
    let (number, unit) = text.trim().split_once(' ')?;
    let number: f64 = number.parse().ok()?;
    let scale = match unit.trim() {
        "B" => 1u64,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        _ => return None,
    };
    (number >= 0.0).then_some((number * scale as f64) as u64)
}

/// The query for the installed size of `names`. pacman reports each name
/// that is not installed on stderr and exits 1, which here just means it has
/// no size. `LC_ALL=C` keeps the labels English and the decimal point a point.
pub fn installed_size_cmd(names: &[String]) -> String {
    let names: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| is_valid_package_name(n))
        .collect();
    format!(
        "LC_ALL=C pacman -Qi {} 2>/dev/null || true",
        names.join(" ")
    )
}

/// Installed sizes by package name, from [`installed_size_cmd`]'s output.
pub fn parse_installed_sizes(text: &str) -> BTreeMap<String, u64> {
    let mut sizes = BTreeMap::new();
    let mut name = None;
    for (key, value) in text.lines().filter_map(|l| l.split_once(':')) {
        match key.trim() {
            "Name" => name = Some(value.trim().to_string()),
            "Installed Size" => {
                if let (Some(n), Some(size)) = (name.take(), parse_size(value)) {
                    sizes.insert(n, size);
                }
            }
            _ => {}
        }
    }
    sizes
}

/// Bytes an unprivileged user can still write on the filesystem holding
/// `path`, or on its nearest existing ancestor: the build root is created
/// later, inside the set.
// Both counts are `u32` on 32-bit targets; the conversions are not no-ops there.
#[allow(clippy::useless_conversion)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    let stat = nix::sys::statvfs::statvfs(existing).ok()?;
    Some(u64::from(stat.blocks_available()).saturating_mul(u64::from(stat.fragment_size())))
}

/// The bases of `srcinfos` the build root has no room for.
///
/// `run` answers a read-only query against the system the packages are
/// installed on, for their installed sizes. Free space is measured on the
/// host: the build root is on `/var`, which is the same in every set.
/// Nothing is reported when it cannot be measured.
pub fn check(
    srcinfos: &BTreeMap<String, String>,
    run: impl Fn(&str) -> Result<String>,
) -> Result<BTreeMap<String, Shortfall>> {
    let Some(free) = free_bytes(Path::new(BUILD_ROOT)) else {
        return Ok(BTreeMap::new());
    };
    let names: BTreeSet<String> = srcinfos
        .values()
        .flat_map(|text| pkgnames(text))
        .map(str::to_string)
        .collect();
    let installed = if names.is_empty() {
        BTreeMap::new()
    } else {
        let names: Vec<String> = names.into_iter().collect();
        parse_installed_sizes(&run(&installed_size_cmd(&names))?)
    };
    Ok(shortfalls(srcinfos, &installed, free))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KERNEL: &str = "\
pkgbase = linux-xanmod-bore
\tpkgver = 7.3.3
\tmakedepends = pahole

pkgname = linux-xanmod-bore

pkgname = linux-xanmod-bore-headers
";

    const WAYDROID: &str = "\
pkgbase = waydroid-image
\tpkgver = 20.0_20260927

pkgname = waydroid-image-gapps

pkgname = waydroid-image-vanilla
";

    fn sizes(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs.iter().map(|(n, s)| (n.to_string(), *s)).collect()
    }

    fn srcinfos(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(b, t)| (b.to_string(), t.to_string()))
            .collect()
    }

    #[test]
    fn a_base_that_packages_kernel_headers_is_a_kernel() {
        assert!(is_kernel(KERNEL));
        assert!(!is_kernel(WAYDROID));
        assert!(!is_kernel(
            "pkgbase = linux-firmware-git\n\npkgname = linux-firmware-git\n"
        ));
    }

    /// The kernel's installed size says nothing about its object tree.
    #[test]
    fn a_kernel_needs_the_kernel_allowance_whatever_its_installed_size() {
        let installed = sizes(&[("linux-xanmod-bore", 600 << 20)]);
        assert_eq!(need(KERNEL, &installed), KERNEL_BUILD + RESERVE);
    }

    #[test]
    fn anything_else_needs_twice_its_installed_packages() {
        let installed = sizes(&[("waydroid-image-gapps", 3 * GIB), ("unrelated", 9 * GIB)]);
        assert_eq!(need(WAYDROID, &installed), 6 * GIB + RESERVE);
    }

    #[test]
    fn a_package_not_installed_needs_only_the_reserve() {
        assert_eq!(need(WAYDROID, &BTreeMap::new()), RESERVE);
    }

    /// The case that discarded a whole update: 17 GiB free is not enough for
    /// the kernel, and plenty for the rest.
    #[test]
    fn only_the_bases_that_cannot_fit_fall_short() {
        let infos = srcinfos(&[("linux-xanmod-bore", KERNEL), ("waydroid-image", WAYDROID)]);
        let installed = sizes(&[("waydroid-image-gapps", 3 * GIB)]);
        let short = shortfalls(&infos, &installed, 17 * GIB);
        assert_eq!(short.keys().collect::<Vec<_>>(), vec!["linux-xanmod-bore"]);
        assert!(short["linux-xanmod-bore"].kernel);
        assert!(shortfalls(&infos, &installed, 22 * GIB).is_empty());
    }

    #[test]
    fn the_reason_names_both_sizes_and_where() {
        let reason = Shortfall {
            need: KERNEL_BUILD + RESERVE,
            free: 17 * GIB,
            kernel: true,
        }
        .reason();
        assert!(reason.contains("21.0 GiB"), "{reason}");
        assert!(reason.contains("17.0 GiB"), "{reason}");
        assert!(reason.contains(BUILD_ROOT), "{reason}");
        assert!(reason.contains("kernel"), "{reason}");
    }

    #[test]
    fn sizes_parse_as_pacman_prints_them() {
        assert_eq!(parse_size("2.00 GiB"), Some(2 * GIB));
        assert_eq!(parse_size(" 512.00 KiB"), Some(512 << 10));
        assert_eq!(parse_size("0.00 B"), Some(0));
        assert_eq!(
            parse_size("11.64 MiB"),
            Some((11.64 * (1u64 << 20) as f64) as u64)
        );
        assert_eq!(parse_size("2.00 GB"), None);
        assert_eq!(parse_size("-1.00 MiB"), None);
    }

    #[test]
    fn installed_sizes_are_read_per_package() {
        let out = "\
Name            : yay
Version         : 13.0.1-1
Installed Size  : 11.64 MiB
Packager        : Unknown Packager

Name            : waydroid-image-gapps
Installed Size  : 2.00 GiB
";
        let sizes = parse_installed_sizes(out);
        assert_eq!(sizes.len(), 2);
        assert_eq!(sizes["waydroid-image-gapps"], 2 * GIB);
    }

    /// The names come from a `.SRCINFO` anyone can edit and go into shell.
    #[test]
    fn the_size_query_drops_names_that_are_not_package_names() {
        let cmd = installed_size_cmd(&["yay".into(), "$(reboot)".into()]);
        assert!(cmd.contains("pacman -Qi yay "), "{cmd}");
        assert!(!cmd.contains("reboot"), "{cmd}");
    }

    #[test]
    fn free_space_is_measured_on_the_nearest_existing_ancestor() {
        let missing = std::env::temp_dir().join("deploytix-space-test/not/yet/made");
        assert!(free_bytes(&missing).is_some());
    }

    #[test]
    fn the_check_asks_for_the_sizes_of_every_package_the_batch_builds() {
        let infos = srcinfos(&[("waydroid-image", WAYDROID)]);
        let asked = std::cell::RefCell::new(String::new());
        check(&infos, |q| {
            *asked.borrow_mut() = q.to_string();
            Ok(String::new())
        })
        .unwrap();
        let asked = asked.into_inner();
        assert!(
            asked.contains("waydroid-image-gapps") && asked.contains("waydroid-image-vanilla"),
            "{asked}"
        );
    }
}
