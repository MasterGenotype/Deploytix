//! Detection of, and command construction for, third-party AUR helpers.
//!
//! Deploytix's installer can build `yay` from source, but a deployed system is
//! whatever its owner has made it: they may have installed `paru` instead, or
//! nothing at all. The updater therefore detects what is present rather than
//! assuming, and drives it through one interface.
//!
//! # Two shapes of helper
//!
//! Most helpers (`yay`, `paru`, `pikaur`, `trizen`) are pacman-like: they take
//! `-S`, they must **not** run as root because they call `makepkg`, and they
//! escalate for the final install themselves. `aura` is the exception — it is
//! designed to be run as root and takes `-A` for AUR targets. Getting this
//! backwards is not a cosmetic error: `makepkg` refuses outright to run as
//! root, so a helper invoked with the wrong privilege simply fails.
//!
//! # Build directories
//!
//! Nothing here passes a helper-specific build-directory flag. Those flags
//! differ per helper and some do not exist at all, whereas every one of these
//! ends up invoking `makepkg`, which honours the environment from
//! [`super::build::build_env`] over its own config file. One mechanism, no
//! per-helper guesswork.

use std::fmt;

/// A supported AUR helper, in the order [`detect`] prefers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AurHelper {
    /// Rust, actively maintained, closest to pacman's interface.
    Paru,
    /// Go; what deploytix's own installer builds.
    Yay,
    /// Python, review-oriented.
    Pikaur,
    /// Perl, long-standing.
    Trizen,
    /// Haskell; the odd one out — runs as root and uses `-A`.
    Aura,
}

impl AurHelper {
    /// Every helper this module knows, most preferred first.
    ///
    /// Preference is by how actively maintained the helper is and how closely
    /// it follows pacman's interface, which is what makes its behaviour under
    /// `--noconfirm` predictable.
    pub const ALL: &'static [Self] = &[
        Self::Paru,
        Self::Yay,
        Self::Pikaur,
        Self::Trizen,
        Self::Aura,
    ];

    /// The executable name, which is also the package name for all five.
    pub fn binary(self) -> &'static str {
        match self {
            Self::Paru => "paru",
            Self::Yay => "yay",
            Self::Pikaur => "pikaur",
            Self::Trizen => "trizen",
            Self::Aura => "aura",
        }
    }

    /// Whether this helper expects to be run as root.
    ///
    /// `aura` does. The rest must run as an unprivileged user because they
    /// invoke `makepkg`, which exits rather than build as root.
    pub fn runs_as_root(self) -> bool {
        matches!(self, Self::Aura)
    }

    /// The subcommand flag that means "install these AUR targets".
    fn install_flag(self) -> &'static str {
        match self {
            Self::Aura => "-A",
            _ => "-S",
        }
    }

    /// Flags that make the helper non-interactive.
    ///
    /// `--needed` is not universal, so it is only passed to the helpers that
    /// document it; skipping it costs a rebuild, passing it where unsupported
    /// is a hard argument error.
    fn noninteractive_flags(self) -> &'static [&'static str] {
        match self {
            Self::Paru | Self::Yay => &["--noconfirm", "--needed"],
            Self::Pikaur | Self::Trizen => &["--noconfirm"],
            Self::Aura => &["--noconfirm"],
        }
    }

    /// Absolute path the binary would occupy under `root`.
    ///
    /// `root` is a chroot prefix; `""` means the live system.
    pub fn path_in(self, root: &str) -> String {
        format!("{root}/usr/bin/{}", self.binary())
    }

    /// The command that installs `packages`, ready for a chroot shell.
    ///
    /// `env_prefix` is prepended inside the privilege drop rather than outside
    /// it, so the build environment survives into the process that actually
    /// runs `makepkg`. `sudo -u` alone would discard it.
    ///
    /// `build_user` is ignored for a root-run helper.
    pub fn install_cmd(self, build_user: &str, env_prefix: &str, packages: &[String]) -> String {
        let flags = self.noninteractive_flags().join(" ");
        let targets = packages.join(" ");
        let core = format!(
            "{env}{bin} {flag} {flags} {targets}",
            env = env_prefix,
            bin = self.binary(),
            flag = self.install_flag(),
            flags = flags,
            targets = targets,
        );
        if self.runs_as_root() {
            core
        } else {
            // `env` carries the assignments across the privilege boundary:
            // sudo resets the environment, so a bare prefix outside `sudo -u`
            // would be dropped before makepkg ever sees it.
            format!("sudo -u {build_user} env {core}")
        }
    }
}

impl fmt::Display for AurHelper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.binary())
    }
}

/// Shell that prints the name of each known helper present under `root`, one
/// per line, in preference order.
///
/// A single command rather than one probe per helper: the caller is often
/// talking to a chroot, where each invocation is a process spawn.
pub fn detect_cmd(root: &str) -> String {
    let tests: Vec<String> = AurHelper::ALL
        .iter()
        .map(|h| format!("[ -x {} ] && echo {}", h.path_in(root), h.binary()))
        .collect();
    format!("{}; true", tests.join("; "))
}

/// Parse [`detect_cmd`] output into helpers, most preferred first.
///
/// Unknown lines are ignored rather than failing: this reads the output of a
/// shell that may also have emitted warnings.
pub fn parse_detected(stdout: &str) -> Vec<AurHelper> {
    let mut found: Vec<AurHelper> = stdout
        .lines()
        .map(str::trim)
        .filter_map(|line| AurHelper::ALL.iter().copied().find(|h| h.binary() == line))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The helper to use out of those detected: the most preferred one present.
pub fn preferred(detected: &[AurHelper]) -> Option<AurHelper> {
    AurHelper::ALL
        .iter()
        .copied()
        .find(|h| detected.contains(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paru_outranks_yay() {
        assert_eq!(
            preferred(&[AurHelper::Yay, AurHelper::Paru]),
            Some(AurHelper::Paru)
        );
    }

    #[test]
    fn preference_is_independent_of_detection_order() {
        let a = preferred(&[AurHelper::Trizen, AurHelper::Yay]);
        let b = preferred(&[AurHelper::Yay, AurHelper::Trizen]);
        assert_eq!(a, b);
        assert_eq!(a, Some(AurHelper::Yay));
    }

    #[test]
    fn no_helpers_means_none() {
        assert_eq!(preferred(&[]), None);
    }

    #[test]
    fn only_aura_runs_as_root() {
        for h in AurHelper::ALL {
            assert_eq!(h.runs_as_root(), *h == AurHelper::Aura, "{h}");
        }
    }

    #[test]
    fn unprivileged_helpers_drop_privileges_and_keep_the_environment() {
        let pkgs = vec!["hhd-git".to_string()];
        let cmd = AurHelper::Yay.install_cmd("deck", "BUILDDIR=/var/x ", &pkgs);
        assert!(cmd.starts_with("sudo -u deck env "), "got: {cmd}");
        // The assignment must land inside the privilege drop, or sudo discards
        // it and makepkg builds on the tmpfs after all.
        let sudo_at = cmd.find("sudo -u").unwrap();
        let env_at = cmd.find("BUILDDIR=").unwrap();
        assert!(
            env_at > sudo_at,
            "env prefix escaped the privilege drop: {cmd}"
        );
    }

    #[test]
    fn aura_does_not_drop_privileges_and_uses_its_own_flag() {
        let pkgs = vec!["hhd-git".to_string()];
        let cmd = AurHelper::Aura.install_cmd("deck", "BUILDDIR=/var/x ", &pkgs);
        assert!(!cmd.contains("sudo"), "aura must run as root: {cmd}");
        assert!(cmd.contains("aura -A"), "got: {cmd}");
    }

    #[test]
    fn needed_only_where_supported() {
        let pkgs = vec!["p".to_string()];
        assert!(AurHelper::Yay
            .install_cmd("u", "", &pkgs)
            .contains("--needed"));
        assert!(AurHelper::Paru
            .install_cmd("u", "", &pkgs)
            .contains("--needed"));
        // Passing an unsupported flag is an argument error, not a warning.
        assert!(!AurHelper::Trizen
            .install_cmd("u", "", &pkgs)
            .contains("--needed"));
        assert!(!AurHelper::Aura
            .install_cmd("u", "", &pkgs)
            .contains("--needed"));
    }

    #[test]
    fn every_helper_is_noninteractive() {
        let pkgs = vec!["p".to_string()];
        for h in AurHelper::ALL {
            assert!(
                h.install_cmd("u", "", &pkgs).contains("--noconfirm"),
                "{h} would block waiting for input"
            );
        }
    }

    #[test]
    fn all_targets_reach_the_command() {
        let pkgs = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let cmd = AurHelper::Paru.install_cmd("u", "", &pkgs);
        for p in &pkgs {
            assert!(cmd.contains(p.as_str()), "{p} missing from {cmd}");
        }
    }

    #[test]
    fn detect_cmd_probes_every_helper_under_the_root() {
        let sh = detect_cmd("/run/deploytix-update/123");
        for h in AurHelper::ALL {
            assert!(
                sh.contains(&h.path_in("/run/deploytix-update/123")),
                "{h} not probed"
            );
        }
        // Must not fail the shell when nothing is found.
        assert!(sh.ends_with("; true"));
    }

    #[test]
    fn detect_cmd_on_live_system_uses_absolute_paths() {
        assert!(detect_cmd("").contains("/usr/bin/yay"));
    }

    #[test]
    fn parse_ignores_noise_and_orders_by_preference() {
        let out = "yay\nwarning: something\nparu\n\n";
        assert_eq!(parse_detected(out), vec![AurHelper::Paru, AurHelper::Yay]);
    }

    #[test]
    fn parse_deduplicates() {
        assert_eq!(parse_detected("yay\nyay\n"), vec![AurHelper::Yay]);
    }

    #[test]
    fn parse_empty_is_empty() {
        assert!(parse_detected("").is_empty());
        assert!(parse_detected("\n \n").is_empty());
    }
}
