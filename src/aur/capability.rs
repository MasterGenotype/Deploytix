//! What this system can actually do with the AUR, decided before any
//! transaction starts.
//!
//! The updater runs as root under polkit on a system deployed months earlier,
//! with no deployment config to consult. Everything an AUR build needs — a
//! helper, an unprivileged account to build as, `base-devel` — is therefore a
//! runtime question, and each one can be missing independently.
//!
//! This module answers it once and records *why* anything is missing, so the
//! UI can grey a control out with a reason instead of letting the user start a
//! transaction that fails ten minutes in. Nothing here mutates system state.

use crate::aur::helper::{self, AurHelper};
use crate::utils::command::CommandRunner;
use std::fmt;

/// Lowest uid a normal login account gets on Arch/Artix (`SYS_UID_MAX` is
/// 999). Accounts below this are system accounts and must never be handed a
/// build.
const FIRST_REGULAR_UID: u32 = 1000;

/// Upper bound excluding `nobody` (65534) and the 16-bit sentinel.
const LAST_REGULAR_UID: u32 = 60000;

/// Why no AUR build can run right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blocker {
    /// No supported helper is installed.
    NoHelper,
    /// No unprivileged account to run `makepkg` as.
    NoBuildUser,
    /// `base-devel` is absent, so builds fail at the first compiler call.
    NoBaseDevel,
}

impl fmt::Display for Blocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::NoHelper => "No AUR helper found. Install paru or yay to build AUR packages.",
            Self::NoBuildUser => {
                "No unprivileged user account found. makepkg refuses to run as root, \
                 so an AUR build needs a normal user to build as."
            }
            Self::NoBaseDevel => {
                "base-devel is not installed, so builds would fail at the first \
                 compiler invocation."
            }
        };
        f.write_str(msg)
    }
}

/// How the build user was arrived at. Worth showing: "the person who launched
/// this" and "a guess from /etc/passwd" deserve different confidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildUserSource {
    /// From `PKEXEC_UID` — the desktop user who authenticated. Most reliable:
    /// it is who actually asked for the operation.
    Pkexec,
    /// From `SUDO_UID`, when launched from a terminal with sudo.
    Sudo,
    /// First regular account in `/etc/passwd`. A guess, and labelled as one.
    PasswdScan,
}

impl BuildUserSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pkexec => "the user who authenticated",
            Self::Sudo => "the user who ran sudo",
            Self::PasswdScan => "first regular account (guessed)",
        }
    }

    /// Whether this should be confirmed by the user before building.
    pub fn is_guess(self) -> bool {
        matches!(self, Self::PasswdScan)
    }
}

/// The account an AUR build would run `makepkg` as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildUser {
    pub name: String,
    pub uid: u32,
    pub source: BuildUserSource,
}

/// What this system can do with the AUR.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AurCapability {
    /// Every helper found, most preferred first.
    pub helpers: Vec<AurHelper>,
    /// The one that would be used.
    pub helper: Option<AurHelper>,
    pub build_user: Option<BuildUser>,
    pub base_devel: bool,
    pub blockers: Vec<Blocker>,
}

impl AurCapability {
    /// Whether an AUR build could start right now.
    pub fn is_ready(&self) -> bool {
        self.blockers.is_empty()
    }

    /// One-line summary for the System tab.
    pub fn summary(&self) -> String {
        if let Some(blocker) = self.blockers.first() {
            return blocker.to_string();
        }
        // is_ready() implies both are Some, but render defensively rather than
        // unwrapping in a UI path.
        match (&self.helper, &self.build_user) {
            (Some(h), Some(u)) => format!("{h}, building as {}", u.name),
            _ => "Unavailable".to_string(),
        }
    }

    /// Derive the blocker list from the parts. Kept separate from probing so it
    /// can be tested without a system to probe.
    fn with_blockers(mut self) -> Self {
        let mut blockers = Vec::new();
        if self.helper.is_none() {
            blockers.push(Blocker::NoHelper);
        }
        if self.build_user.is_none() {
            blockers.push(Blocker::NoBuildUser);
        }
        if !self.base_devel {
            blockers.push(Blocker::NoBaseDevel);
        }
        self.blockers = blockers;
        self
    }
}

/// Pick the build user from the environment, falling back to `/etc/passwd`.
///
/// `PKEXEC_UID` first because it names the person who actually authenticated
/// for this operation — the updater is launched through polkit, so it is set in
/// the normal case. `SUDO_UID` covers a terminal launch. Only when neither is
/// present does this guess, and the guess is labelled as one so the UI can say
/// so.
pub fn resolve_build_user(
    getenv: &dyn Fn(&str) -> Option<String>,
    passwd: &str,
) -> Option<BuildUser> {
    for (var, source) in [
        ("PKEXEC_UID", BuildUserSource::Pkexec),
        ("SUDO_UID", BuildUserSource::Sudo),
    ] {
        if let Some(uid) = getenv(var).and_then(|v| v.trim().parse::<u32>().ok()) {
            if let Some(name) = username_for_uid(passwd, uid) {
                return Some(BuildUser { name, uid, source });
            }
        }
    }
    first_regular_user(passwd).map(|(name, uid)| BuildUser {
        name,
        uid,
        source: BuildUserSource::PasswdScan,
    })
}

/// Look up a username by uid in `/etc/passwd` content.
fn username_for_uid(passwd: &str, want: u32) -> Option<String> {
    passwd_rows(passwd)
        .find(|(_, uid, _)| *uid == want)
        .map(|(name, _, _)| name)
}

/// The first regular (non-system) account with a real shell.
///
/// Lowest uid wins so the result is stable regardless of how `/etc/passwd` is
/// ordered — appending a user must not change which account builds.
fn first_regular_user(passwd: &str) -> Option<(String, u32)> {
    passwd_rows(passwd)
        .filter(|(_, uid, shell)| {
            (FIRST_REGULAR_UID..=LAST_REGULAR_UID).contains(uid) && is_login_shell(shell)
        })
        .min_by_key(|(_, uid, _)| *uid)
        .map(|(name, uid, _)| (name, uid))
}

/// Yield `(name, uid, shell)` for well-formed rows, skipping anything else.
fn passwd_rows(passwd: &str) -> impl Iterator<Item = (String, u32, String)> + '_ {
    passwd.lines().filter_map(|line| {
        let mut f = line.split(':');
        let name = f.next()?;
        let _passwd = f.next()?;
        let uid: u32 = f.next()?.parse().ok()?;
        // gid, gecos, home, shell
        let shell = f.nth(3).unwrap_or("");
        if name.is_empty() {
            return None;
        }
        Some((name.to_string(), uid, shell.to_string()))
    })
}

/// Whether a shell field denotes an account a person can build under.
fn is_login_shell(shell: &str) -> bool {
    !matches!(
        shell.trim(),
        "" | "/usr/bin/nologin"
            | "/sbin/nologin"
            | "/usr/sbin/nologin"
            | "/bin/false"
            | "/usr/bin/false"
    )
}

/// Shell that prints `base-devel` if the group is installed.
///
/// `pacman -Qg` rather than `-Qi`: `base-devel` is a package *group*, so it is
/// never itself an installed package and `-Qi base-devel` always fails.
fn base_devel_cmd() -> &'static str {
    "pacman -Qg base-devel >/dev/null 2>&1 && echo base-devel; true"
}

/// Probe `root` for everything an AUR build needs.
///
/// `root` is a chroot prefix; `""` means the live system. Read-only: it runs
/// `[ -x ... ]` tests and a `pacman -Qg` query and changes nothing.
pub fn probe(cmd: &CommandRunner, root: &str) -> AurCapability {
    let helpers = probe_helpers(cmd, root);
    let helper = helper::preferred(&helpers);
    let base_devel = probe_base_devel(cmd, root);
    let passwd = read_passwd(root);
    let build_user = resolve_build_user(&|k| std::env::var(k).ok(), &passwd);

    AurCapability {
        helpers,
        helper,
        build_user,
        base_devel,
        blockers: Vec::new(),
    }
    .with_blockers()
}

/// Read `/etc/passwd` from `root`, or empty on failure.
///
/// An unreadable passwd file yields no build user and therefore a blocker,
/// which is the right outcome — better than guessing a name that may not exist.
fn read_passwd(root: &str) -> String {
    std::fs::read_to_string(format!("{root}/etc/passwd")).unwrap_or_default()
}

fn probe_helpers(cmd: &CommandRunner, root: &str) -> Vec<AurHelper> {
    match cmd.run("sh", &["-c", &helper::detect_cmd(root)]) {
        Ok(Some(out)) => helper::parse_detected(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

fn probe_base_devel(cmd: &CommandRunner, root: &str) -> bool {
    let sh = if root.is_empty() {
        base_devel_cmd().to_string()
    } else {
        format!("chroot {root} sh -c '{}'", base_devel_cmd())
    };
    match cmd.run("sh", &["-c", &sh]) {
        Ok(Some(out)) => String::from_utf8_lossy(&out.stdout).contains("base-devel"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "\
root:x:0:0::/root:/bin/bash
bin:x:1:1::/:/usr/bin/nologin
http:x:33:33::/srv/http:/usr/bin/nologin
nobody:x:65534:65534:Nobody:/:/usr/bin/nologin
superphenotype:x:1000:1000::/home/superphenotype:/bin/bash
guest:x:1001:1001::/home/guest:/bin/zsh
";

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| {
            owned
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn pkexec_uid_wins_over_sudo_and_the_passwd_scan() {
        let user = resolve_build_user(
            &env_of(&[("PKEXEC_UID", "1001"), ("SUDO_UID", "1000")]),
            PASSWD,
        )
        .expect("a build user");
        assert_eq!(user.name, "guest");
        assert_eq!(user.source, BuildUserSource::Pkexec);
        assert!(!user.source.is_guess());
    }

    #[test]
    fn sudo_uid_used_when_pkexec_absent() {
        let user = resolve_build_user(&env_of(&[("SUDO_UID", "1000")]), PASSWD).unwrap();
        assert_eq!(user.name, "superphenotype");
        assert_eq!(user.source, BuildUserSource::Sudo);
    }

    #[test]
    fn falls_back_to_lowest_regular_uid_and_admits_it_is_a_guess() {
        let user = resolve_build_user(&env_of(&[]), PASSWD).unwrap();
        assert_eq!(user.name, "superphenotype");
        assert_eq!(user.uid, 1000);
        assert!(user.source.is_guess());
    }

    #[test]
    fn passwd_order_does_not_change_the_guess() {
        let reordered = "\
guest:x:1001:1001::/home/guest:/bin/zsh
superphenotype:x:1000:1000::/home/superphenotype:/bin/bash
";
        assert_eq!(first_regular_user(reordered).unwrap().0, "superphenotype");
    }

    #[test]
    fn never_picks_root_or_a_system_account() {
        let only_system = "\
root:x:0:0::/root:/bin/bash
bin:x:1:1::/:/usr/bin/nologin
";
        assert!(first_regular_user(only_system).is_none());
        assert!(resolve_build_user(&env_of(&[]), only_system).is_none());
    }

    #[test]
    fn never_picks_nobody() {
        let with_nobody = "nobody:x:65534:65534:Nobody:/:/bin/bash\n";
        assert!(first_regular_user(with_nobody).is_none());
    }

    #[test]
    fn an_env_uid_with_no_matching_account_falls_through_to_the_scan() {
        // A stale PKEXEC_UID must not produce a username that does not exist.
        let user = resolve_build_user(&env_of(&[("PKEXEC_UID", "4242")]), PASSWD).unwrap();
        assert_eq!(user.source, BuildUserSource::PasswdScan);
        assert_eq!(user.name, "superphenotype");
    }

    #[test]
    fn a_nonnumeric_env_uid_is_ignored() {
        let user = resolve_build_user(&env_of(&[("PKEXEC_UID", "nonsense")]), PASSWD).unwrap();
        assert_eq!(user.source, BuildUserSource::PasswdScan);
    }

    #[test]
    fn nologin_accounts_are_not_build_users() {
        assert!(!is_login_shell("/usr/bin/nologin"));
        assert!(!is_login_shell("/bin/false"));
        assert!(!is_login_shell(""));
        assert!(is_login_shell("/bin/bash"));
    }

    #[test]
    fn malformed_passwd_lines_are_skipped_not_fatal() {
        let junk = "garbage\n::::\nsuperphenotype:x:1000:1000::/home/s:/bin/bash\n";
        assert_eq!(first_regular_user(junk).unwrap().0, "superphenotype");
    }

    #[test]
    fn unreadable_passwd_yields_a_blocker_rather_than_a_wrong_name() {
        assert!(resolve_build_user(&env_of(&[]), "").is_none());
    }

    #[test]
    fn every_missing_piece_is_reported_not_just_the_first() {
        let cap = AurCapability::default().with_blockers();
        assert_eq!(cap.blockers.len(), 3);
        assert!(cap.blockers.contains(&Blocker::NoHelper));
        assert!(cap.blockers.contains(&Blocker::NoBuildUser));
        assert!(cap.blockers.contains(&Blocker::NoBaseDevel));
        assert!(!cap.is_ready());
    }

    #[test]
    fn fully_equipped_system_is_ready() {
        let cap = AurCapability {
            helpers: vec![AurHelper::Paru],
            helper: Some(AurHelper::Paru),
            build_user: Some(BuildUser {
                name: "deck".into(),
                uid: 1000,
                source: BuildUserSource::Pkexec,
            }),
            base_devel: true,
            blockers: Vec::new(),
        }
        .with_blockers();
        assert!(cap.is_ready());
        assert_eq!(cap.summary(), "paru, building as deck");
    }

    #[test]
    fn summary_explains_the_blocker_rather_than_saying_unavailable() {
        let cap = AurCapability::default().with_blockers();
        assert!(cap.summary().contains("No AUR helper"));
    }

    #[test]
    fn base_devel_probe_uses_group_query() {
        // -Qi base-devel always fails: it is a group, never an installed package.
        assert!(base_devel_cmd().contains("-Qg"));
        assert!(!base_devel_cmd().contains("-Qi"));
    }
}
