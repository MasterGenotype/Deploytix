//! The PGP keys an AUR build verifies its sources against, settled before
//! building.
//!
//! # Why
//!
//! A PKGBUILD's `validpgpkeys` names the keys `makepkg` checks signed sources
//! against, and the build fails without them. A helper imports the missing
//! ones itself, but from the one keyserver gpg is configured with, and only
//! after it has downloaded the sources of every package in the batch. When
//! that keyserver lacks the key the helper gives up, taking every build in the
//! batch with it, and the transaction throws the set away, repository upgrades
//! included.
//!
//! That is not hypothetical. keyserver.ubuntu.com, gpg's default, has no copy
//! of Linus Torvalds' signing key (`ABAF11C6…`), which every kernel package
//! that builds from a kernel.org tarball lists. keys.openpgp.org has it, but
//! without a user ID, and gpg refuses a key without one. A `deploytix update
//! --aur` with a kernel in it ran for half an hour and then discarded the set.
//!
//! # What this does
//!
//! Reads each package base's `.SRCINFO` from the AUR, then, as the build user
//! inside the set, imports whichever of its keys the keyring lacks, trying the
//! configured keyserver and then each of [`KEYSERVERS`]. What is still missing
//! goes back to the caller, which decides: `deploytix update --aur` holds the
//! package back and builds the rest, and `deploytix aur` refuses before
//! building.
//!
//! The keyring is the build user's `~/.gnupg`. `/home` is rbound into the set,
//! so it is the same keyring on the running system, and a key imported there
//! is already present for the next transaction.
//!
//! An unreadable `.SRCINFO` is not an error. The AUR answered the RPC that
//! picked these packages, so the helper's own import gets its chance, as it
//! did before this check existed.

use crate::aur::rpc::{is_valid_package_name, AurRpc, CurlGet, HttpGet};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::collections::{BTreeMap, BTreeSet};
use tracing::{info, warn};

/// Where `.SRCINFO` files are served from, one per package base.
pub const SRCINFO_BASE: &str = "https://aur.archlinux.org/cgit/aur.git/plain/.SRCINFO";

/// Keyservers tried after the one gpg is configured with, in order.
pub const KEYSERVERS: &[&str] = &["hkps://keyserver.ubuntu.com", "hkps://keys.openpgp.org"];

/// The `.SRCINFO` URL of package base `base`.
pub fn srcinfo_url(base: &str) -> String {
    format!("{SRCINFO_BASE}?h={base}")
}

/// Whether `key` is a key ID or fingerprint: hex digits, 8 to 64 of them.
///
/// The keys come from a file anyone with an AUR account can edit and go into
/// a shell command, so anything else is dropped rather than quoted.
fn is_key_id(key: &str) -> bool {
    (8..=64).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The `validpgpkeys` of a `.SRCINFO`, deduplicated, in the order listed.
pub fn parse_valid_pgp_keys(srcinfo: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for line in srcinfo.lines() {
        let Some((field, value)) = line.split_once('=') else {
            continue;
        };
        let key = value.trim().to_ascii_uppercase();
        if field.trim() == "validpgpkeys" && is_key_id(&key) && !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// The keys each of `bases` lists, leaving out bases that list none.
pub fn required_keys<H: HttpGet>(http: &H, bases: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut required = BTreeMap::new();
    for base in bases.iter().filter(|b| is_valid_package_name(b)) {
        match http.get(&srcinfo_url(base)) {
            Ok(srcinfo) => {
                let keys = parse_valid_pgp_keys(&srcinfo);
                if !keys.is_empty() {
                    required.insert(base.clone(), keys);
                }
            }
            Err(e) => warn!("[aur] Could not read the .SRCINFO of {base}: {e}"),
        }
    }
    required
}

/// Shell that imports each of `keys` missing from `user`'s keyring and prints
/// `IMPORTED <key>` or `MISSING <key>` for each one it had to look for.
///
/// Every attempt is judged by whether the key is then in the keyring, not by
/// gpg's exit status: a keyserver that serves the key without a user ID makes
/// `--recv-keys` report the key as processed and import nothing.
pub fn import_cmd(user: &str, keys: &[String]) -> String {
    let gpg = format!("sudo -u {user} gpg --batch");
    let mut attempts = vec!["get \"$k\"".to_string()];
    attempts.extend(
        KEYSERVERS
            .iter()
            .map(|s| format!("get \"$k\" --keyserver {s}")),
    );
    format!(
        "have() {{ {gpg} --list-keys \"$1\" >/dev/null 2>&1; }}; \
         get() {{ k=$1; shift; {gpg} \"$@\" --recv-keys \"$k\" >/dev/null 2>&1; have \"$k\"; }}; \
         for k in {keys}; do have \"$k\" && continue; \
         if {attempts}; then echo \"IMPORTED $k\"; else echo \"MISSING $k\"; fi; done; true",
        keys = keys.join(" "),
        attempts = attempts.join(" || "),
    )
}

/// The keys [`import_cmd`] reported as imported, and as still missing.
pub fn parse_import_output(stdout: &str) -> (Vec<String>, Vec<String>) {
    let mut imported = Vec::new();
    let mut missing = Vec::new();
    for line in stdout.lines() {
        if let Some(k) = line.trim().strip_prefix("IMPORTED ") {
            imported.push(k.to_string());
        } else if let Some(k) = line.trim().strip_prefix("MISSING ") {
            missing.push(k.to_string());
        }
    }
    (imported, missing)
}

/// For each base in `required`, the keys it needs that are still missing.
pub fn unresolved(
    required: &BTreeMap<String, Vec<String>>,
    missing: &[String],
) -> BTreeMap<String, Vec<String>> {
    required
        .iter()
        .filter_map(|(base, keys)| {
            let lacking: Vec<String> = keys
                .iter()
                .filter(|k| missing.contains(k))
                .cloned()
                .collect();
            (!lacking.is_empty()).then(|| (base.clone(), lacking))
        })
        .collect()
}

/// Make sure `user`'s keyring inside `target` holds the keys `bases` verify
/// their sources against. Returns, per base, the keys no keyserver supplied.
pub fn ensure_for_bases(
    cmd: &CommandRunner,
    target: &str,
    user: &str,
    bases: &[String],
) -> Result<BTreeMap<String, Vec<String>>> {
    let required = required_keys(&CurlGet, bases);
    let keys: BTreeSet<String> = required.values().flatten().cloned().collect();
    if keys.is_empty() {
        return Ok(BTreeMap::new());
    }
    let keys: Vec<String> = keys.into_iter().collect();
    info!(
        "[aur] Checking {} source-signing key(s) for {user}",
        keys.len()
    );
    let stdout = match cmd.run_in_chroot(target, &import_cmd(user, &keys))? {
        Some(out) => String::from_utf8_lossy(&out.stdout).to_string(),
        // A dry run imports nothing and so finds nothing missing.
        None => return Ok(BTreeMap::new()),
    };
    let (imported, missing) = parse_import_output(&stdout);
    for k in &imported {
        info!("[aur] Imported PGP key {k} into {user}'s keyring");
    }
    Ok(unresolved(&required, &missing))
}

/// [`ensure_for_bases`] for packages named by the user, whose bases are looked
/// up first. A failed lookup checks each name as its own base.
pub fn ensure_for_packages(
    cmd: &CommandRunner,
    target: &str,
    user: &str,
    packages: &[String],
) -> Result<BTreeMap<String, Vec<String>>> {
    let names: Vec<&str> = packages.iter().map(String::as_str).collect();
    let bases: BTreeSet<String> = match AurRpc::new(CurlGet).info(&names) {
        Ok(found) => found.iter().map(|p| p.pkgbase().to_string()).collect(),
        Err(_) => packages.iter().cloned().collect(),
    };
    ensure_for_bases(cmd, target, user, &bases.into_iter().collect::<Vec<_>>())
}

/// Why a base cannot be built, for a log line or an error: the keys and where
/// they were looked for.
pub fn missing_reason(keys: &[String], user: &str) -> String {
    format!(
        "its sources are signed with {}, which no keyserver could supply (tried gpg's \
         configured one, {}); import it into {user}'s keyring from wherever its developer \
         publishes it and run again",
        keys.join(", "),
        KEYSERVERS.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::error::DeploytixError;

    const XANMOD_SRCINFO: &str = "\
pkgbase = linux-xanmod-bore
\tpkgver = 7.2.12
\tsource = https://cdn.kernel.org/pub/linux/kernel/v7.x/linux-7.2.tar.sign
\tvalidpgpkeys = ABAF11C65A2970B130ABE3C479BE3E4300411886
\tvalidpgpkeys = 647F28654894E3BD457199BE38DBBDC86092693E

pkgname = linux-xanmod-bore

pkgname = linux-xanmod-bore-headers
";

    const LINUS: &str = "ABAF11C65A2970B130ABE3C479BE3E4300411886";
    const GREG: &str = "647F28654894E3BD457199BE38DBBDC86092693E";

    #[test]
    fn validpgpkeys_are_read_from_srcinfo() {
        assert_eq!(parse_valid_pgp_keys(XANMOD_SRCINFO), vec![LINUS, GREG]);
    }

    #[test]
    fn a_srcinfo_without_keys_needs_none() {
        assert!(parse_valid_pgp_keys("pkgbase = yay\n\tpkgver = 13.1.0\n").is_empty());
    }

    /// The keys land in a shell command; a `.SRCINFO` is anyone's to edit.
    #[test]
    fn anything_but_a_hex_key_id_is_dropped() {
        let srcinfo = "\
\tvalidpgpkeys = ABAF11C6; rm -rf ~
\tvalidpgpkeys = $(reboot)
\tvalidpgpkeys = 1234
\tvalidpgpkeys = 79be3e4300411886
\tvalidpgpkeys = 79BE3E4300411886
";
        assert_eq!(parse_valid_pgp_keys(srcinfo), vec!["79BE3E4300411886"]);
    }

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
    fn required_keys_skips_bases_without_keys_and_unreadable_srcinfo() {
        let http = Canned(BTreeMap::from([
            ("linux-xanmod-bore", XANMOD_SRCINFO),
            ("yay", "pkgbase = yay\n"),
        ]));
        let required = required_keys(
            &http,
            &["linux-xanmod-bore".into(), "yay".into(), "gone".into()],
        );
        assert_eq!(
            required,
            BTreeMap::from([(
                "linux-xanmod-bore".to_string(),
                vec![LINUS.into(), GREG.into()]
            )])
        );
    }

    #[test]
    fn the_import_runs_as_the_build_user_and_falls_back_across_keyservers() {
        let sh = import_cmd("deck", &[LINUS.to_string()]);
        assert!(sh.contains("sudo -u deck gpg --batch"), "{sh}");
        for server in KEYSERVERS {
            assert!(sh.contains(server), "{server} not tried: {sh}");
        }
        assert!(sh.contains(LINUS));
        assert!(sh.ends_with("; true"), "{sh}");
    }

    /// The configured keyserver goes first: it is the one the user chose.
    #[test]
    fn the_configured_keyserver_is_tried_before_the_fallbacks() {
        let sh = import_cmd("deck", &[LINUS.to_string()]);
        let plain = sh.find("if get \"$k\" ||").expect(&sh);
        let first_fallback = sh.find(KEYSERVERS[0]).unwrap();
        assert!(plain < first_fallback, "{sh}");
    }

    #[test]
    fn import_output_is_parsed_into_imported_and_missing() {
        let out = format!("IMPORTED {GREG}\ngpg: noise\nMISSING {LINUS}\n");
        assert_eq!(
            parse_import_output(&out),
            (vec![GREG.to_string()], vec![LINUS.to_string()])
        );
    }

    #[test]
    fn only_bases_lacking_a_key_are_unresolved() {
        let required = BTreeMap::from([
            (
                "linux-xanmod-bore".to_string(),
                vec![LINUS.into(), GREG.into()],
            ),
            ("protonplus".to_string(), vec![GREG.into()]),
        ]);
        assert_eq!(
            unresolved(&required, &[LINUS.to_string()]),
            BTreeMap::from([("linux-xanmod-bore".to_string(), vec![LINUS.to_string()])])
        );
        assert!(unresolved(&required, &[]).is_empty());
    }

    #[test]
    fn the_reason_names_the_key_and_the_keyring() {
        let why = missing_reason(&[LINUS.to_string()], "deck");
        assert!(
            why.contains(LINUS) && why.contains("deck's keyring"),
            "{why}"
        );
    }
}
