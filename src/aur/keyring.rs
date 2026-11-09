//! The keyring an AUR build verifies its sources with, and the build user's
//! own keyring, which a build no longer touches.
//!
//! # Why a keyring of its own
//!
//! gpg 2.4 can keep public keys in an SQLite database served by `keyboxd`, a
//! daemon the first gpg command starts, which then holds a lock naming its PID
//! for as long as it runs. A build runs in `artix-chroot`, and two things it
//! does break that:
//!
//! * It runs the command in a PID namespace of its own. A keyboxd started
//!   there writes a PID that means something only inside that namespace, and
//!   is killed with the namespace, its lock still held. gpg breaks a lock only
//!   when that PID is not running, and on the host the same small numbers are
//!   kernel threads, so the lock stays. Every later gpg then waits on it and
//!   gives up, which is how `deploytix update --aur` failed with
//!   `error reading '[stream]': SQL library used incorrectly`.
//! * It mounts a fresh `/run`, so gpg there cannot reach the session's
//!   daemons under `/run/user/<uid>`. It starts a second keyboxd beside the
//!   session's, which from inside the namespace takes the session's lock for a
//!   dead one and breaks it.
//!
//! `/home` is rbound into the set, so both happened to the user's own keyring,
//! and `gpg --list-keys` on the running system timed out.
//!
//! # What this does
//!
//! Builds use [`GNUPGHOME`], a keyring deploytix owns, under the build root on
//! `/var`, which every set shares. Its `common.conf` is written by deploytix
//! and leaves keyboxd off, so keys live in `pubring.kbx`, which only the gpg
//! process writing to it locks, and only for as long as it runs. The agents gpg
//! may still start in a chroot hold no lock: they die with the namespace and
//! leave sockets the next one replaces.
//!
//! Keys reach it from the build user's own keyring first, read on the host
//! where its daemons are the session's, then from keyservers (see
//! [`crate::aur::keys`]). A key the user imports by hand therefore still
//! counts.
//!
//! [`repair_user_keyring`] removes a keyboxd lock an earlier deploytix
//! stranded in the user's keyring. Nothing here strands one any more.
//!
//! Best-effort throughout. A keyring this cannot read or fix is reported and
//! left to gpg, never a reason to fail the build.

use crate::utils::command::CommandRunner;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// The keyring every AUR build verifies its sources with, under
/// [`crate::aur::build::BUILD_ROOT`].
pub const GNUPGHOME: &str = "/var/cache/deploytix/build/gnupg";

/// Shell that creates [`GNUPGHOME`] for `user`, with a `common.conf` that
/// leaves keyboxd off.
///
/// `common.conf` is rewritten every time rather than created once: gpg reads
/// `use-keyboxd` from it and has no option to turn it off, so the one way to
/// be certain is to own the file. Idempotent otherwise.
pub fn ensure_cmd(user: &str) -> String {
    format!(
        "install -d -m 700 -o {user} -g {user} {GNUPGHOME} && \
         install -m 600 -o {user} -g {user} /dev/null {GNUPGHOME}/common.conf"
    )
}

/// gpg as `user`, on the build keyring.
pub fn build_gpg(user: &str) -> String {
    format!("sudo -u {user} env GNUPGHOME={GNUPGHOME} gpg --batch")
}

/// keyboxd's lock, relative to the user's home. `sudo -u` resets `GNUPGHOME`,
/// so gpg run that way as the user uses the default `~/.gnupg`.
pub const KEYBOXD_LOCK: &str = ".gnupg/public-keys.d/pubring.db.lock";

/// Who a gpg lock file names as its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockOwner {
    pub pid: u32,
    pub host: String,
}

/// Parse a gpg lock file: the owner's PID, then its hostname, a line each.
pub fn parse_lock(text: &str) -> Option<LockOwner> {
    let mut lines = text.lines();
    let pid = lines.next()?.trim().parse().ok()?;
    let host = lines.next()?.trim().to_string();
    Some(LockOwner { pid, host })
}

/// The parts of `/proc/<pid>/status` that decide whether a process can own a
/// lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcStatus {
    pub name: String,
    pub uid: u32,
    /// Its PID in the innermost namespace it belongs to, which is the PID it
    /// sees for itself and writes into a lock.
    pub ns_pid: u32,
}

/// Parse `/proc/<pid>/status`. `NSpid` lists the PID from the outermost
/// namespace inwards, so the last entry is the one the process knows.
pub fn parse_status(text: &str) -> Option<ProcStatus> {
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':').map(str::trim))
    };
    Some(ProcStatus {
        name: field("Name")?.to_string(),
        uid: field("Uid")?.split_whitespace().next()?.parse().ok()?,
        ns_pid: field("NSpid")?.split_whitespace().last()?.parse().ok()?,
    })
}

/// Whether a keyboxd of `uid` that wrote `owner.pid` is still running.
///
/// Matched on name and user as well as PID: a sandboxed app has small
/// namespace PIDs of its own, and one of them being 6 says nothing about the
/// lock.
pub fn owner_is_alive(owner: &LockOwner, uid: u32, procs: &[ProcStatus]) -> bool {
    procs
        .iter()
        .any(|p| p.name == "keyboxd" && p.uid == uid && p.ns_pid == owner.pid)
}

/// Every process's status, from the host's `/proc`.
fn processes() -> Vec<ProcStatus> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .filter_map(|e| std::fs::read_to_string(e.path().join("status")).ok())
        .filter_map(|s| parse_status(&s))
        .collect()
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_string())
}

/// Remove the lock at `lock` if `uid` has no live keyboxd that could own it.
/// Returns whether it was removed.
///
/// A lock naming another host is left alone: that keyring is shared over the
/// network, and whether its owner is alive cannot be told from here.
fn release_if_stale(lock: &Path, uid: u32) -> bool {
    let Ok(text) = std::fs::read_to_string(lock) else {
        return false;
    };
    let Some(owner) = parse_lock(&text) else {
        debug!("[aur] Unrecognised gpg lock at {}", lock.display());
        return false;
    };
    if hostname().as_deref() != Some(owner.host.as_str())
        || owner_is_alive(&owner, uid, &processes())
    {
        return false;
    }
    // A keyboxd that took the lock since it was read made a new file; leave it.
    if std::fs::read_to_string(lock).ok().as_deref() != Some(text.as_str()) {
        return false;
    }
    if let Err(e) = std::fs::remove_file(lock) {
        warn!(
            "[aur] Could not remove stale gpg lock {}: {e}",
            lock.display()
        );
        return false;
    }
    // gpg takes a lock by hard-linking a temporary file to it, named after the
    // host and PID. Without its owner nothing would ever remove that either.
    let suffix = format!(".{}.{}", owner.host, owner.pid);
    if let Some(Ok(entries)) = lock.parent().map(std::fs::read_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(".#lk") && name.ends_with(&suffix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    info!(
        "[aur] Removed a stale gpg keyring lock at {} (its keyboxd, PID {}, is gone)",
        lock.display(),
        owner.pid
    );
    true
}

/// [`KEYBOXD_LOCK`] under `user`'s home, with their UID.
fn lock_of(user: &str) -> Option<(PathBuf, u32)> {
    match nix::unistd::User::from_name(user) {
        Ok(Some(u)) => Some((u.dir.join(KEYBOXD_LOCK), u.uid.as_raw())),
        _ => {
            warn!("[aur] Could not look up {user}; not checking their gpg keyring lock");
            None
        }
    }
}

/// Remove a keyboxd lock in `user`'s keyring whose owner is gone.
///
/// Runs on the host, which sees the processes of every namespace, so a
/// keyboxd killed with a chroot's namespace is certain to be gone.
pub fn repair_user_keyring(cmd: &CommandRunner, user: &str) {
    if cmd.is_dry_run() {
        return;
    }
    if let Some((path, uid)) = lock_of(user) {
        release_if_stale(&path, uid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: u32 = 1000;

    fn keyboxd(uid: u32, ns_pid: u32) -> ProcStatus {
        ProcStatus {
            name: "keyboxd".into(),
            uid,
            ns_pid,
        }
    }

    fn lock(pid: u32) -> LockOwner {
        LockOwner {
            pid,
            host: "artix".into(),
        }
    }

    #[test]
    fn the_build_keyring_is_on_var_under_the_build_root() {
        assert!(GNUPGHOME.starts_with(crate::aur::build::BUILD_ROOT));
        assert!(GNUPGHOME.starts_with("/var/"));
    }

    /// keyboxd is what strands a lock; an empty `common.conf` leaves it off.
    #[test]
    fn the_build_keyring_is_created_without_keyboxd() {
        let sh = ensure_cmd("deck");
        assert!(
            sh.contains(&format!("-m 700 -o deck -g deck {GNUPGHOME}")),
            "{sh}"
        );
        assert!(
            sh.contains(&format!("/dev/null {GNUPGHOME}/common.conf")),
            "{sh}"
        );
        assert!(!sh.contains("use-keyboxd"), "{sh}");
    }

    #[test]
    fn build_gpg_runs_as_the_user_on_the_build_keyring() {
        assert_eq!(
            build_gpg("deck"),
            format!("sudo -u deck env GNUPGHOME={GNUPGHOME} gpg --batch")
        );
    }

    #[test]
    fn a_lock_file_names_pid_then_host() {
        assert_eq!(parse_lock("6\nartix\n"), Some(lock(6)));
        assert_eq!(parse_lock("garbage\n"), None);
        assert_eq!(parse_lock("6\n"), None);
    }

    #[test]
    fn status_reads_the_innermost_namespace_pid() {
        let status = "Name:\tkeyboxd\nUmask:\t0022\nState:\tS (sleeping)\n\
                      Uid:\t1000\t1000\t1000\t1000\nNSpid:\t48213\t6\n";
        assert_eq!(parse_status(status), Some(keyboxd(1000, 6)));
        let host = "Name:\tkeyboxd\nUid:\t1000\t1000\t1000\t1000\nNSpid:\t7160\n";
        assert_eq!(parse_status(host), Some(keyboxd(1000, 7160)));
    }

    /// A kernel thread has a status file but is nobody's lock owner; it is
    /// what PID 6 is on the host.
    #[test]
    fn a_kernel_thread_at_the_lock_pid_does_not_keep_it_alive() {
        let kworker = ProcStatus {
            name: "kworker/R-kvfree_rcu_reclaim".into(),
            uid: 0,
            ns_pid: 6,
        };
        assert!(!owner_is_alive(&lock(6), UID, &[kworker]));
    }

    /// The case that hung gpg on the running system: a host keyboxd waiting
    /// on the lock a namespaced one left.
    #[test]
    fn a_keyboxd_waiting_on_the_lock_is_not_its_owner() {
        assert!(!owner_is_alive(&lock(6), UID, &[keyboxd(UID, 7160)]));
    }

    #[test]
    fn the_keyboxd_holding_the_lock_keeps_it() {
        assert!(owner_is_alive(&lock(7160), UID, &[keyboxd(UID, 7160)]));
        // From inside a namespace too, seen from the host.
        assert!(owner_is_alive(&lock(6), UID, &[keyboxd(UID, 6)]));
    }

    #[test]
    fn another_users_keyboxd_does_not_own_the_lock() {
        assert!(!owner_is_alive(&lock(6), UID, &[keyboxd(1001, 6)]));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("deploytix-keyring-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// PID 0 is never a process, so the lock is stale on any host.
    #[test]
    fn a_stale_lock_and_its_temporary_file_are_removed() {
        let Some(host) = hostname() else { return };
        let dir = scratch("stale");
        let lock = dir.join("pubring.db.lock");
        let content = format!("0\n{host}\n");
        std::fs::write(&lock, &content).unwrap();
        let temp = dir.join(format!(".#lk0x00007f07480022b0.{host}.0"));
        std::fs::hard_link(&lock, &temp).unwrap();
        let unrelated = dir.join(format!(".#lk0x00007f07480022b0.{host}.10"));
        std::fs::write(&unrelated, "10\n").unwrap();

        assert!(release_if_stale(&lock, UID));
        assert!(!lock.exists() && !temp.exists());
        assert!(unrelated.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_lock_from_another_host_is_left() {
        let dir = scratch("foreign");
        let lock = dir.join("pubring.db.lock");
        std::fs::write(&lock, "0\nsome-other-host.invalid\n").unwrap();
        assert!(!release_if_stale(&lock, UID));
        assert!(lock.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_lock_is_nothing_to_do() {
        let dir = scratch("none");
        assert!(!release_if_stale(&dir.join("pubring.db.lock"), UID));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
