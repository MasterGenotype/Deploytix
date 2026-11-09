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
//! Reads each package base's `.SRCINFO` from the AUR, then puts whichever of
//! its keys are missing into the build keyring, [`keyring::GNUPGHOME`]. Each is
//! looked for in the build user's own keyring first, then on gpg's default
//! keyserver and each of [`KEYSERVERS`]. What is still missing goes back to
//! the caller, which decides: `deploytix update --aur` holds the package back
//! and builds the rest, and `deploytix aur` refuses before building.
//!
//! This runs on the host, not in the set. The build keyring is on `/var`,
//! which every set shares, so a key put there is present in this set and the
//! next. The user's keyring is only read, and on the host, where gpg reaches
//! the session's own daemons. [`crate::aur::keyring`] has why a build must not
//! use that keyring.
//!
//! The `.SRCINFO`s come from [`crate::aur::srcinfo::fetch`]; a base whose
//! `.SRCINFO` could not be read is not checked, and the helper's own import
//! gets its chance, as it did before this check existed.

use crate::aur::keyring;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::collections::{BTreeMap, BTreeSet};
use tracing::info;

/// Keyservers tried, in order, after the user's keyring and gpg's default
/// keyserver.
pub const KEYSERVERS: &[&str] = &["hkps://keyserver.ubuntu.com", "hkps://keys.openpgp.org"];

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

/// The keys each base in `srcinfos` lists, leaving out bases that list none.
pub fn required_keys(srcinfos: &BTreeMap<String, String>) -> BTreeMap<String, Vec<String>> {
    srcinfos
        .iter()
        .filter_map(|(base, text)| {
            let keys = parse_valid_pgp_keys(text);
            (!keys.is_empty()).then(|| (base.clone(), keys))
        })
        .collect()
}

/// Shell, for the host, that puts each of `keys` missing from the build
/// keyring there, and prints `IMPORTED <key>` or `MISSING <key>` for each one
/// it had to look for.
///
/// `user`'s own keyring is tried first: a key they imported by hand, from
/// wherever its developer publishes it, is the answer for a key no keyserver
/// has. Every attempt is judged by whether the key is then in the build
/// keyring, not by gpg's exit status: a keyserver that serves the key without
/// a user ID makes `--recv-keys` report the key as processed and import
/// nothing.
///
/// Ends by stopping the agents gpg started for the build keyring, which would
/// otherwise stay running on the host.
pub fn import_cmd(user: &str, keys: &[String]) -> String {
    let gpg = keyring::build_gpg(user);
    let mut attempts = vec!["mine \"$k\"".to_string(), "get \"$k\"".to_string()];
    attempts.extend(
        KEYSERVERS
            .iter()
            .map(|s| format!("get \"$k\" --keyserver {s}")),
    );
    format!(
        "have() {{ {gpg} --list-keys \"$1\" >/dev/null 2>&1; }}; \
         mine() {{ sudo -u {user} gpg --batch --export \"$1\" 2>/dev/null | \
         {gpg} --import >/dev/null 2>&1; have \"$1\"; }}; \
         get() {{ k=$1; shift; {gpg} \"$@\" --recv-keys \"$k\" >/dev/null 2>&1; have \"$k\"; }}; \
         for k in {keys}; do have \"$k\" && continue; \
         if {attempts}; then echo \"IMPORTED $k\"; else echo \"MISSING $k\"; fi; done; \
         sudo -u {user} env GNUPGHOME={home} gpgconf --kill all >/dev/null 2>&1; true",
        keys = keys.join(" "),
        attempts = attempts.join(" || "),
        home = keyring::GNUPGHOME,
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

/// Make sure the build keyring holds the keys the bases in `srcinfos` verify
/// their sources against. Returns, per base, the keys neither `user`'s keyring
/// nor any keyserver supplied.
pub fn ensure_for_bases(
    cmd: &CommandRunner,
    user: &str,
    srcinfos: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, Vec<String>>> {
    let required = required_keys(srcinfos);
    let keys: BTreeSet<String> = required.values().flatten().cloned().collect();
    if keys.is_empty() {
        return Ok(BTreeMap::new());
    }
    let keys: Vec<String> = keys.into_iter().collect();
    info!(
        "[aur] Checking {} source-signing key(s) for {user}",
        keys.len()
    );
    // Before reading their keyring: an earlier deploytix may have left it
    // locked, and every gpg there would wait on that and fail.
    keyring::repair_user_keyring(cmd, user);
    cmd.run("sh", &["-c", &keyring::ensure_cmd(user)])?;
    let stdout = match cmd.run("sh", &["-c", &import_cmd(user, &keys)])? {
        Some(out) => String::from_utf8_lossy(&out.stdout).to_string(),
        // A dry run imports nothing and so finds nothing missing.
        None => return Ok(BTreeMap::new()),
    };
    let (imported, missing) = parse_import_output(&stdout);
    for k in &imported {
        info!("[aur] Imported PGP key {k} into the build keyring");
    }
    Ok(unresolved(&required, &missing))
}

/// Why a base cannot be built, for a log line or an error: the keys and where
/// they were looked for.
pub fn missing_reason(keys: &[String], user: &str) -> String {
    format!(
        "its sources are signed with {}, which neither {user}'s keyring nor any keyserver \
         could supply (tried gpg's default, {}); import it into {user}'s keyring from \
         wherever its developer publishes it and run again",
        keys.join(", "),
        KEYSERVERS.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn required_keys_skips_bases_without_keys() {
        let srcinfos = BTreeMap::from([
            ("linux-xanmod-bore".to_string(), XANMOD_SRCINFO.to_string()),
            ("yay".to_string(), "pkgbase = yay\n".to_string()),
        ]);
        assert_eq!(
            required_keys(&srcinfos),
            BTreeMap::from([(
                "linux-xanmod-bore".to_string(),
                vec![LINUS.into(), GREG.into()]
            )])
        );
    }

    #[test]
    fn the_import_runs_as_the_build_user_and_falls_back_across_keyservers() {
        let sh = import_cmd("deck", &[LINUS.to_string()]);
        assert!(sh.contains(&keyring::build_gpg("deck")), "{sh}");
        for server in KEYSERVERS {
            assert!(sh.contains(server), "{server} not tried: {sh}");
        }
        assert!(sh.contains(LINUS));
        assert!(sh.ends_with("; true"), "{sh}");
    }

    /// A key the user imported by hand beats every keyserver: it is how a key
    /// no keyserver serves gets in.
    #[test]
    fn the_users_keyring_is_tried_before_any_keyserver() {
        let sh = import_cmd("deck", &[LINUS.to_string()]);
        let mine = sh.find("if mine \"$k\" || get \"$k\" ||").expect(&sh);
        let first_fallback = sh.find(KEYSERVERS[0]).unwrap();
        assert!(mine < first_fallback, "{sh}");
    }

    /// Read from, never written to, and by the user's own default gpg.
    #[test]
    fn the_users_keyring_is_only_exported_from() {
        let sh = import_cmd("deck", &[LINUS.to_string()]);
        assert!(
            sh.contains("sudo -u deck gpg --batch --export \"$1\""),
            "{sh}"
        );
        assert_eq!(sh.matches("sudo -u deck gpg").count(), 1, "{sh}");
    }

    /// Agents started for the build keyring on the host would outlive the run.
    #[test]
    fn the_build_keyrings_agents_are_stopped_afterwards() {
        let sh = import_cmd("deck", &[LINUS.to_string()]);
        let kill = format!(
            "sudo -u deck env GNUPGHOME={} gpgconf --kill all",
            keyring::GNUPGHOME
        );
        assert!(sh.contains(&kill), "{sh}");
        assert!(sh.find(&kill) > sh.find("done;"), "{sh}");
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
