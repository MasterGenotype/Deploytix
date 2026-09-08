//! Building and installing AUR packages transactionally.
//!
//! # What this is, and what it reuses
//!
//! An AUR install is the same transaction as an update with a different
//! command inside the chroot, so it is a `body` handed to
//! [`crate::immutable::update::run_in_new_set`] rather than a new transaction
//! type. Snapshotting the running trio, mounting it with the booted system's
//! topology, moving the boot pointer on success and discarding the half-built
//! set on failure are all inherited unchanged — which matters, because those
//! are the boot-critical parts and they are already exercised by `update` and
//! `remove`.
//!
//! What is specific to the AUR is only this:
//!
//! * the build runs as an unprivileged user, because `makepkg` refuses to run
//!   as root (see [`crate::aur::capability`] for how that user is chosen);
//! * scratch goes on the disk-backed build root rather than the chroot's
//!   half-RAM `/tmp` (see [`crate::aur::build`]);
//! * whichever helper the system has drives it (see
//!   [`crate::aur::helper`]).
//!
//! # What it refuses
//!
//! The LVM A/B backend, matching `deploytix remove`: running the btrfs path
//! against a root that is not the one that boots would edit the wrong slot.
//!
//! Packages that own the kernel, anything under `/boot`, or the cryptsetup and
//! initramfs pieces on an encrypted root. `/boot` and `/var` are shared across
//! every snapshot set, so a build that overwrites something there does it for
//! every set at once and no rollback can undo it. That check is
//! [`crate::immutable::remove::check_protected`], reused rather than
//! reimplemented, and it runs against what the build would actually install.

use crate::aur::build;
use crate::aur::capability::AurCapability;
use crate::aur::helper::AurHelper;
use crate::immutable::history;
use crate::immutable::remove;
use crate::immutable::update::{run_in_new_set, UpdateOptions};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use tracing::{info, warn};

/// Options for an AUR transaction.
#[derive(Debug, Clone)]
pub struct AurInstallOptions {
    pub keep_sets: usize,
    pub reboot: bool,
}

impl Default for AurInstallOptions {
    fn default() -> Self {
        let base = UpdateOptions::default();
        Self {
            keep_sets: base.keep_sets,
            reboot: base.reboot,
        }
    }
}

/// Why an AUR install cannot proceed, as a user-facing message.
///
/// Separated from the transaction so the GUI can grey out its button with the
/// same reason the CLI would print, rather than the two drifting apart.
pub fn refusal_reason(cap: &AurCapability, is_lvm_ab: bool) -> Option<String> {
    if is_lvm_ab {
        return Some(
            "Building AUR packages is not implemented for the LVM A/B backend. \
             Running the btrfs path here would build into a root that is not the \
             one that boots."
                .to_string(),
        );
    }
    cap.blockers().first().map(|b| b.to_string())
}

/// The in-chroot command that builds and installs `packages`.
///
/// Split out so the exact invocation is testable without a system to run it
/// on: this is the one place where the helper, the build user and the
/// disk-backed scratch have to line up.
pub fn build_cmd(helper: AurHelper, build_user: &str, packages: &[String]) -> String {
    helper.install_cmd(build_user, &build::build_env_prefix(), packages)
}

/// Build and install `packages` from the AUR into a new snapshot set.
///
/// Returns once the set is staged; it takes effect on the next reboot. The
/// running system is never modified, so an interrupted or failed build is a
/// no-op.
pub fn run_aur_install(
    cmd: &CommandRunner,
    cap: &AurCapability,
    packages: &[String],
    opts: &AurInstallOptions,
) -> Result<()> {
    if packages.is_empty() {
        return Err(DeploytixError::ConfigError(
            "no AUR packages given".to_string(),
        ));
    }
    if let Some(reason) = refusal_reason(cap, crate::immutable::lvm_ab::detect()) {
        return Err(DeploytixError::ConfigError(reason));
    }

    // is_ready() already established both are present; this keeps the failure a
    // clear message rather than a panic if that ever stops being true.
    let (Some(helper), Some(user)) = (cap.helper, cap.build_user.as_ref()) else {
        return Err(DeploytixError::ConfigError(
            "no AUR helper or build user available".to_string(),
        ));
    };

    let update_opts = UpdateOptions {
        keep_sets: opts.keep_sets,
        reboot: opts.reboot,
    };
    let request = history::Request::Aur(packages.to_vec());
    let packages = packages.to_vec();
    let build_user = user.name.clone();

    run_in_new_set(cmd, &update_opts, request, move |cmd, target, _set_id| {
        info!(
            "[aur] Building {} package(s) with {helper} as {build_user}",
            packages.len()
        );

        // Scratch first: without it the build lands on the chroot's tmpfs,
        // which the kernel caps at half of RAM.
        build::ensure_build_root(cmd, target, &build_user)?;

        let before = history::query_packages(cmd, target);
        cmd.run_in_chroot(target, &build_cmd(helper, &build_user, &packages))?;

        // A build that installs into /boot or replaces the kernel affects every
        // snapshot set at once, because /boot is shared and not snapshotted.
        // Checked after the build, when the file lists exist, and before the
        // set is activated -- returning Err here throws the set away.
        let lists = match cmd.run_in_chroot(target, &remove::file_list_cmd(&packages))? {
            Some(o) => remove::parse_file_lists(&String::from_utf8_lossy(&o.stdout)),
            None => Vec::new(),
        };
        let encrypted = std::path::Path::new(crate::immutable::ROOT_FS_DEVICE).exists();
        if let Err(blocked) = remove::check_protected(&lists, encrypted) {
            warn!("[aur] Refusing to activate a set that touches shared boot state");
            return Err(DeploytixError::ConfigError(format!(
                "These packages write to state shared by every snapshot set, \
                 which no rollback can undo:\n  {}",
                blocked.join("\n  ")
            )));
        }

        // The initramfs is shared too, so regenerate it inside the set: if the
        // build broke it, this fails and the transaction is discarded.
        cmd.run_in_chroot(target, "mkinitcpio -P")?;

        let after = history::query_packages(cmd, target);
        Ok(history::diff(&before, &after))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aur::capability::{BuildUser, BuildUserSource};
    use crate::aur::helper::AurHelper;

    fn ready_capability() -> AurCapability {
        AurCapability {
            helpers: vec![AurHelper::Paru],
            helper: Some(AurHelper::Paru),
            build_user: Some(BuildUser {
                name: "deck".into(),
                uid: 1000,
                source: BuildUserSource::Pkexec,
            }),
            base_devel: true,
        }
    }

    #[test]
    fn the_build_runs_as_the_build_user_not_root() {
        // makepkg exits rather than build as root, so this is the difference
        // between working and not working at all.
        let cmd = build_cmd(AurHelper::Paru, "deck", &["hhd-git".to_string()]);
        assert!(cmd.starts_with("sudo -u deck "), "{cmd}");
    }

    #[test]
    fn the_build_is_pointed_at_the_disk_backed_scratch() {
        let cmd = build_cmd(AurHelper::Yay, "deck", &["hhd-git".to_string()]);
        assert!(
            cmd.contains(&format!("BUILDDIR={}", build::BUILD_ROOT)),
            "build would land on the chroot tmpfs: {cmd}"
        );
    }

    #[test]
    fn the_build_environment_survives_the_privilege_drop() {
        let cmd = build_cmd(AurHelper::Yay, "deck", &["p".to_string()]);
        let sudo_at = cmd.find("sudo -u").unwrap();
        let env_at = cmd.find("BUILDDIR=").unwrap();
        assert!(
            env_at > sudo_at,
            "sudo would discard the environment: {cmd}"
        );
    }

    #[test]
    fn a_root_run_helper_is_not_wrapped_in_sudo() {
        let cmd = build_cmd(AurHelper::Aura, "deck", &["p".to_string()]);
        assert!(!cmd.contains("sudo"), "{cmd}");
    }

    #[test]
    fn a_ready_system_on_btrfs_is_not_refused() {
        assert!(refusal_reason(&ready_capability(), false).is_none());
    }

    #[test]
    fn the_lvm_ab_backend_is_refused_even_when_everything_else_is_ready() {
        // Matches deploytix remove: the btrfs path would build into a root
        // that is not the one that boots.
        let reason = refusal_reason(&ready_capability(), true).expect("must refuse");
        assert!(reason.contains("LVM A/B"), "{reason}");
    }

    #[test]
    fn a_missing_prerequisite_is_reported_as_the_refusal() {
        let cap = AurCapability::default();
        let reason = refusal_reason(&cap, false).expect("must refuse");
        assert!(!reason.is_empty());
    }

    #[test]
    fn an_empty_package_list_is_rejected_before_any_snapshot() {
        let cmd = CommandRunner::new(true);
        let err = run_aur_install(
            &cmd,
            &ready_capability(),
            &[],
            &AurInstallOptions::default(),
        )
        .unwrap_err();
        assert!(format!("{err}").contains("no AUR packages"));
    }

    #[test]
    fn defaults_match_the_update_transaction() {
        // An AUR build prunes like any other transaction; diverging here would
        // silently keep a different number of restore points.
        assert_eq!(
            AurInstallOptions::default().keep_sets,
            UpdateOptions::default().keep_sets
        );
        assert!(!AurInstallOptions::default().reboot);
    }
}
