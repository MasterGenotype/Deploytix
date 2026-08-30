//! Resolving the human user behind a root process.
//!
//! Deploytix's graphical tools run as root via `sudo` (which sets `SUDO_USER`)
//! or `pkexec`/polkit (which sets `PKEXEC_UID`). Under both, `$HOME` is root's,
//! so anything that wants the *user's* home — a file browser defaulting
//! somewhere useful, a build directory `makepkg` can write to — has to resolve
//! it from those variables instead.
//!
//! The `/etc/passwd` parsing is split into pure functions so it can be tested
//! without a matching account on the host.

use std::path::PathBuf;

/// Home directory of the user who invoked this process, or `None` when it was
/// not started through sudo/pkexec (e.g. a plain root shell).
pub fn invoking_user_home() -> Option<PathBuf> {
    if let Ok(user) = std::env::var("SUDO_USER") {
        let home = PathBuf::from(format!("/home/{user}"));
        if home.is_dir() {
            return Some(home);
        }
    }
    if let Ok(uid) = std::env::var("PKEXEC_UID") {
        if let Ok(uid) = uid.parse::<u32>() {
            if let Some(home) = home_dir_for_uid(uid) {
                return Some(home);
            }
        }
    }
    None
}

/// Username of the user who invoked this process. `root` is treated as absent,
/// since callers want it to drop privileges to somebody.
pub fn invoking_username() -> Option<String> {
    if let Ok(user) = std::env::var("SUDO_USER") {
        if !user.is_empty() && user != "root" {
            return Some(user);
        }
    }
    if let Ok(uid) = std::env::var("PKEXEC_UID") {
        if let Ok(uid) = uid.parse::<u32>() {
            return username_for_uid(uid);
        }
    }
    None
}

/// Home directory for `uid`, if it exists on disk.
pub fn home_dir_for_uid(uid: u32) -> Option<PathBuf> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    let home = passwd_home_for_uid(&passwd, uid)?;
    home.is_dir().then_some(home)
}

/// Username for `uid`.
pub fn username_for_uid(uid: u32) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd_username_for_uid(&passwd, uid)
}

/// Whether `name` exists as an account.
pub fn user_exists(name: &str) -> bool {
    std::fs::read_to_string("/etc/passwd")
        .map(|passwd| passwd_has_user(&passwd, name))
        .unwrap_or(false)
}

// ── Pure `/etc/passwd` parsing ─────────────────────────────────────────────

/// Fields of a `/etc/passwd` line, if it has the expected shape.
fn passwd_fields(line: &str) -> Option<(&str, u32, &str)> {
    let fields: Vec<&str> = line.split(':').collect();
    if fields.len() < 6 {
        return None;
    }
    let uid = fields[2].parse::<u32>().ok()?;
    Some((fields[0], uid, fields[5]))
}

/// Home directory recorded for `uid` (not checked for existence).
fn passwd_home_for_uid(passwd: &str, uid: u32) -> Option<PathBuf> {
    passwd.lines().find_map(|line| {
        let (_, line_uid, home) = passwd_fields(line)?;
        (line_uid == uid).then(|| PathBuf::from(home))
    })
}

/// Username recorded for `uid`.
fn passwd_username_for_uid(passwd: &str, uid: u32) -> Option<String> {
    passwd.lines().find_map(|line| {
        let (name, line_uid, _) = passwd_fields(line)?;
        (line_uid == uid).then(|| name.to_string())
    })
}

/// Whether `passwd` contains an account named `name`.
fn passwd_has_user(passwd: &str, name: &str) -> bool {
    passwd
        .lines()
        .any(|line| line.split(':').next() == Some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/bash
nobody:x:65534:65534:Nobody:/:/usr/bin/nologin
alice:x:1000:1000:Alice:/home/alice:/bin/bash
";

    #[test]
    fn finds_home_and_username_by_uid() {
        assert_eq!(
            passwd_home_for_uid(PASSWD, 1000),
            Some(PathBuf::from("/home/alice"))
        );
        assert_eq!(
            passwd_username_for_uid(PASSWD, 1000),
            Some("alice".to_string())
        );
    }

    #[test]
    fn an_unknown_uid_resolves_to_nothing() {
        assert_eq!(passwd_home_for_uid(PASSWD, 4242), None);
        assert_eq!(passwd_username_for_uid(PASSWD, 4242), None);
    }

    #[test]
    fn short_and_malformed_lines_are_skipped() {
        // Comments and truncated lines turn up in the wild; they must not
        // shadow a real entry further down the file.
        let passwd = "# comment\nbroken:x:1000\n\nalice:x:1000:1000:Alice:/home/alice:/bin/bash\n";
        assert_eq!(
            passwd_home_for_uid(passwd, 1000),
            Some(PathBuf::from("/home/alice"))
        );
    }

    #[test]
    fn a_non_numeric_uid_field_does_not_panic() {
        assert_eq!(
            passwd_home_for_uid("bad:x:notanumber:0:x:/tmp:/sh", 0),
            None
        );
    }

    #[test]
    fn user_lookup_matches_only_the_name_field() {
        assert!(passwd_has_user(PASSWD, "nobody"));
        assert!(passwd_has_user(PASSWD, "alice"));
        assert!(!passwd_has_user(PASSWD, "carol"));
        // "x" appears as the password field on every line but is not a user.
        assert!(!passwd_has_user(PASSWD, "x"));
    }
}
