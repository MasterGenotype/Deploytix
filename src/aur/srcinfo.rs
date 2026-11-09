//! `.SRCINFO` files from the AUR: what a package base builds from, read before
//! the helper starts.
//!
//! Fetched once per batch and shared by everything that settles a build in
//! advance: the PGP keys its sources are signed with ([`crate::aur::keys`]),
//! the room it needs ([`crate::aur::space`]) and the cached sources that no
//! longer match it ([`crate::aur::srccache`]).

use crate::aur::rpc::{is_valid_package_name, AurRpc, HttpGet};
use std::collections::{BTreeMap, BTreeSet};
use tracing::warn;

/// Where `.SRCINFO` files are served from, one per package base.
pub const SRCINFO_BASE: &str = "https://aur.archlinux.org/cgit/aur.git/plain/.SRCINFO";

/// The `.SRCINFO` URL of package base `base`.
pub fn srcinfo_url(base: &str) -> String {
    format!("{SRCINFO_BASE}?h={base}")
}

/// The `.SRCINFO` of each of `bases`, by base.
///
/// One the AUR would not serve is left out rather than failing the batch: the
/// AUR answered the RPC that picked these packages, so the helper gets its own
/// chance with it, as it did before anything was checked in advance.
pub fn fetch<H: HttpGet>(http: &H, bases: &[String]) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for base in bases.iter().filter(|b| is_valid_package_name(b)) {
        match http.get(&srcinfo_url(base)) {
            Ok(text) => {
                found.insert(base.clone(), text);
            }
            Err(e) => warn!("[aur] Could not read the .SRCINFO of {base}: {e}"),
        }
    }
    found
}

/// The package bases `packages` are built from, for packages named by the
/// user. A failed lookup takes each name as its own base.
pub fn bases_of<H: HttpGet>(http: H, packages: &[String]) -> Vec<String> {
    let names: Vec<&str> = packages.iter().map(String::as_str).collect();
    let bases: BTreeSet<String> = match AurRpc::new(http).info(&names) {
        Ok(found) => found.iter().map(|p| p.pkgbase().to_string()).collect(),
        Err(_) => packages.iter().cloned().collect(),
    };
    bases.into_iter().collect()
}

/// Every `key = value` line of a `.SRCINFO`, trimmed.
pub fn fields(srcinfo: &str) -> impl Iterator<Item = (&str, &str)> {
    srcinfo.lines().filter_map(|line| {
        let (key, value) = line.split_once('=')?;
        Some((key.trim(), value.trim()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::error::{DeploytixError, Result};

    struct Canned(BTreeMap<&'static str, &'static str>);

    impl HttpGet for Canned {
        fn get(&self, url: &str) -> Result<String> {
            let base = url.rsplit("?h=").next().unwrap_or_default();
            self.0
                .get(base)
                .map(|s| s.to_string())
                .ok_or_else(|| DeploytixError::CommandFailed {
                    command: "curl".to_string(),
                    stderr: "404".to_string(),
                })
        }
    }

    #[test]
    fn unreadable_and_invalid_bases_are_left_out() {
        let http = Canned(BTreeMap::from([("yay", "pkgbase = yay\n")]));
        let found = fetch(&http, &["yay".into(), "gone".into(), "$(reboot)".into()]);
        assert_eq!(found.keys().collect::<Vec<_>>(), vec!["yay"]);
    }

    #[test]
    fn fields_are_split_on_the_first_equals_and_trimmed() {
        let text = "pkgbase = x\n\tsource = a::https://h/p?x=1\n\nnoise\n";
        assert_eq!(
            fields(text).collect::<Vec<_>>(),
            vec![("pkgbase", "x"), ("source", "a::https://h/p?x=1")]
        );
    }
}
