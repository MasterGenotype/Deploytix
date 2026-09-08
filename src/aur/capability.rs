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
///
/// Blockers are computed from the parts rather than stored. Storing them meant
/// a value built any way other than through the probe -- `Default::default()`,
/// a struct literal in a caller -- carried an empty blocker list and therefore
/// reported itself ready, which is the one answer that must never be wrong,
/// since it gates a build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AurCapability {
    /// Every helper found, most preferred first.
    pub helpers: Vec<AurHelper>,
    /// The one that would be used.
    pub helper: Option<AurHelper>,
    pub build_user: Option<BuildUser>,
    pub base_devel: bool,
}

impl AurCapability {
    /// Everything missing, in the order it is worth reporting.
    ///
    /// Derived, so it cannot disagree with the fields it describes.
    pub fn blockers(&self) -> Vec<Blocker> {
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
        blockers
    }

    /// Whether an AUR build could start right now.
    pub fn is_ready(&self) -> bool {
        self.blockers().is_empty()
    }

    /// One-line summary for the System tab.
    pub fn summary(&self) -> String {
        if let Some(blocker) = self.blockers().first() {
            return blocker.to_string();
        }
        // is_ready() implies both are Some, but render defensively rather than
        // unwrapping in a UI path.
        match (&self.helper, &self.build_user) {
            (Some(h), Some(u)) => format!("{h}, building as {}", u.name),
            _ => "Unavailable".to_string(),
        }
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

/// Binaries `makepkg` cannot build anything without.
///
/// The last-resort check, and the most honest one: what actually decides
/// whether a build works is whether the toolchain is there, not what a
/// package query is named.
const BUILD_TOOLS: &[&str] = &["gcc", "make", "fakeroot", "patch", "ld"];

/// Shell that prints `base-devel` if this system can build packages.
///
/// Three checks, because the first two each answer only for one era of Arch:
///
/// * `-Qi` — `base-devel` is a **meta package** on current Arch and Artix. It
///   was converted from a group in early 2022.
/// * `-Qg` — before that it was a package *group*, which `-Qi` cannot see. Old
///   deployments still look like this.
/// * the toolchain itself — covers a system that has every build tool but
///   never had the meta package recorded, which is what an install that
///   pulled the tools in individually looks like.
///
/// Checking only `-Qg`, as this did until it was reported in the wild, reports
/// "base-devel is not installed" on every modern system: the group no longer
/// exists, so the query finds nothing however complete the toolchain is.
fn base_devel_cmd() -> String {
    let tools = BUILD_TOOLS
        .iter()
        .map(|t| format!("command -v {t} >/dev/null 2>&1"))
        .collect::<Vec<_>>()
        .join(" && ");
    format!(
        "{{ pacman -Qi base-devel >/dev/null 2>&1 || \
           pacman -Qg base-devel >/dev/null 2>&1 || \
           {{ {tools}; }} ; }} && echo base-devel; true"
    )
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
    }
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
        base_devel_cmd()
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
        let cap = AurCapability::default();
        assert_eq!(cap.blockers().len(), 3);
        assert!(cap.blockers().contains(&Blocker::NoHelper));
        assert!(cap.blockers().contains(&Blocker::NoBuildUser));
        assert!(cap.blockers().contains(&Blocker::NoBaseDevel));
    }

    #[test]
    fn a_default_capability_is_never_ready() {
        // Regression: blockers used to be a stored field, so any value not
        // built by the probe carried an empty list and reported itself ready.
        // That is the one answer that must never be wrong -- it gates a build.
        assert!(!AurCapability::default().is_ready());
    }

    #[test]
    fn a_partially_equipped_system_is_not_ready() {
        let cap = AurCapability {
            helpers: vec![AurHelper::Paru],
            helper: Some(AurHelper::Paru),
            build_user: None,
            base_devel: true,
        };
        assert!(!cap.is_ready());
        assert_eq!(cap.blockers(), vec![Blocker::NoBuildUser]);
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
        };
        assert!(cap.is_ready());
        assert_eq!(cap.summary(), "paru, building as deck");
    }

    #[test]
    fn summary_explains_the_blocker_rather_than_saying_unavailable() {
        let cap = AurCapability::default();
        assert!(cap.summary().contains("No AUR helper"));
    }

    #[test]
    fn base_devel_probe_accepts_the_meta_package_the_group_and_the_toolchain() {
        // Regression: this used to check only -Qg, which reports "not
        // installed" on every current system -- base-devel became a meta
        // package in 2022, so the group query finds nothing however complete
        // the toolchain is.
        let sh = base_devel_cmd();
        assert!(
            sh.contains("-Qi base-devel"),
            "must see the meta package: {sh}"
        );
        assert!(
            sh.contains("-Qg base-devel"),
            "must still see the legacy group: {sh}"
        );
        for tool in BUILD_TOOLS {
            assert!(
                sh.contains(&format!("command -v {tool}")),
                "toolchain fallback missing {tool}: {sh}"
            );
        }
    }

    #[test]
    fn base_devel_probe_never_fails_the_shell() {
        // It runs inside a probe whose failure must mean "absent", not "error".
        assert!(base_devel_cmd().trim_end().ends_with("true"));
    }

    #[test]
    fn base_devel_probe_is_a_disjunction_not_a_conjunction() {
        // Any one of the three is sufficient; requiring all three would report
        // a perfectly buildable system as unable to build.
        let sh = base_devel_cmd();
        assert!(
            sh.contains("||"),
            "the three checks must be alternatives: {sh}"
        );
    }
}
