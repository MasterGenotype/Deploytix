//! Interactive policy hook for pacman / basestrap / yay invocations.
//!
//! When an [`InteractivePolicy`] is attached to the [`CommandRunner`],
//! every user-facing package install (basestrap, pacman -S in chroot,
//! yay -S as user) is routed through the policy before execution.  The
//! policy may approve, edit (mutate the package list and/or extra flags),
//! skip, or cancel the invocation.  With no policy attached, the
//! commands run as-is — config-driven runs stay non-interactive.
//!
//! See `docs/INTERACTIVE_INSTALLER.md` for the full design.
//!
//! Internal pacman calls (`pacman -Sy`, `pacman -Scc`, `pacman-key`,
//! signature-retry fallbacks) are NOT intercepted — they are housekeeping
//! commands with no editable surface.

use std::sync::Arc;

/// Which package manager runs this invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacmanKind {
    /// `basestrap -i <root> ...` — bootstraps a fresh chroot.
    Basestrap,
    /// `pacman -S ...` (typically in chroot via artix-chroot).
    Pacman,
    /// `sudo -u <user> yay -S ...` — AUR install via yay in chroot.
    Yay,
}

impl PacmanKind {
    pub fn binary(&self) -> &'static str {
        match self {
            Self::Basestrap => "basestrap",
            Self::Pacman => "pacman",
            Self::Yay => "yay",
        }
    }

    /// Core flags — fixed, NOT editable by the policy.  Shown to the user
    /// in edit mode but stripped on save.
    pub fn core_flags(&self) -> &'static [&'static str] {
        match self {
            // basestrap takes the install root + packages directly; no
            // editable flag set.
            Self::Basestrap => &[],
            Self::Pacman => &["-S", "--noconfirm", "--needed"],
            Self::Yay => &["-S", "--noconfirm", "--needed"],
        }
    }
}

/// A single user-facing package install, presented to the policy for
/// review.  `packages` and `extra_flags` are mutable in [`PacmanDecision::EditedTo`].
#[derive(Debug, Clone)]
pub struct PacmanInvocation {
    pub kind: PacmanKind,
    /// Human-readable label (e.g. "Basestrap base system",
    /// "AUR: hhd-git", "Install GPU drivers").
    pub label: String,
    /// chroot path for Pacman / Yay; install root for Basestrap.
    pub install_root: Option<String>,
    /// Editable: the package list.
    pub packages: Vec<String>,
    /// Editable: optional extra flags (e.g. `--overwrite`, `--ignore`).
    pub extra_flags: Vec<String>,
    /// Yay only — the user account that runs makepkg.
    pub run_as_user: Option<String>,
}

impl PacmanInvocation {
    pub fn basestrap(install_root: impl Into<String>, packages: Vec<String>) -> Self {
        Self {
            kind: PacmanKind::Basestrap,
            label: "Basestrap base system".to_string(),
            install_root: Some(install_root.into()),
            packages,
            extra_flags: Vec::new(),
            run_as_user: None,
        }
    }

    pub fn pacman_chroot(
        install_root: impl Into<String>,
        label: impl Into<String>,
        packages: Vec<String>,
    ) -> Self {
        Self {
            kind: PacmanKind::Pacman,
            label: label.into(),
            install_root: Some(install_root.into()),
            packages,
            extra_flags: Vec::new(),
            run_as_user: None,
        }
    }

    pub fn yay_chroot(
        install_root: impl Into<String>,
        run_as_user: impl Into<String>,
        label: impl Into<String>,
        packages: Vec<String>,
    ) -> Self {
        Self {
            kind: PacmanKind::Yay,
            label: label.into(),
            install_root: Some(install_root.into()),
            packages,
            extra_flags: Vec::new(),
            run_as_user: Some(run_as_user.into()),
        }
    }

    /// Render the command as the user would see it.
    pub fn render(&self) -> String {
        let core = self.kind.core_flags().join(" ");
        let pkgs = self.packages.join(" ");
        let extra = if self.extra_flags.is_empty() {
            String::new()
        } else {
            format!(" {}", self.extra_flags.join(" "))
        };
        match self.kind {
            PacmanKind::Basestrap => {
                let root = self.install_root.as_deref().unwrap_or("");
                format!("basestrap -i {root}{extra} {pkgs}")
            }
            PacmanKind::Pacman => {
                format!("pacman {core}{extra} {pkgs}")
            }
            PacmanKind::Yay => {
                let user = self.run_as_user.as_deref().unwrap_or("user");
                format!("sudo -u {user} yay {core}{extra} {pkgs}")
            }
        }
    }
}

/// What the policy decided about a single invocation.
#[derive(Debug, Clone)]
pub enum PacmanDecision {
    /// Run the invocation as-is.
    Approve,
    /// Run with the edited package list / flags.
    EditedTo {
        packages: Vec<String>,
        extra_flags: Vec<String>,
    },
    /// Skip this invocation and continue the install.
    Skip,
    /// Abort the entire install.  The runner returns
    /// `DeploytixError::UserCancelled`, which triggers the existing
    /// emergency-cleanup path.
    Cancel,
}

/// User-supplied extras collected by [`InteractivePolicy::prompt_extras`].
#[derive(Debug, Clone, Default)]
pub struct ExtraPackages {
    pub pacman: Vec<String>,
    pub aur: Vec<String>,
}

impl ExtraPackages {
    pub fn is_empty(&self) -> bool {
        self.pacman.is_empty() && self.aur.is_empty()
    }
}

/// Hook for interactive review of pacman/basestrap/yay invocations and
/// for collecting post-install extras.
///
/// Both methods may block — the install thread waits on them.  A no-op
/// implementation is fine for tests.
pub trait InteractivePolicy: Send + Sync {
    /// Decide what to do with an upcoming pacman invocation.
    fn confirm_pacman(&self, inv: &PacmanInvocation) -> PacmanDecision;

    /// Collect post-install extras.  `can_use_yay` is true when the
    /// caller has yay installed (gating the AUR field).  Returns the
    /// collected extras and whether the user wants them written back to
    /// the saved config.
    fn prompt_extras(&self, can_use_yay: bool) -> (ExtraPackages, bool);
}

/// Boxed policy handle stored on `CommandRunner`.
pub type PolicyHandle = Arc<dyn InteractivePolicy>;
