//! The seam between the two transactional backends.
//!
//! deploytix has two ways of doing a transactional update: btrfs snapshot sets
//! ([`super::update`], [`super::rollback`], [`super::snapshot`]) and LVM A/B
//! slots with dm-verity roots ([`super::lvm_ab`]). They are mutually exclusive
//! — the disk layout picks one at install time — and the `deploytix
//! update`/`rollback`/`remove` commands drive whichever one is live.
//!
//! Before this trait existed, `main.rs` made that choice itself, three times,
//! by calling [`super::lvm_ab::detect`] and branching. Every capability
//! difference between the backends was a fact the command layer had to know:
//! most visibly, that transactional removal is not implemented for A/B. Now
//! [`active`] answers "which backend" once and the backend answers for its own
//! capabilities.
//!
//! Snapshot *creation* is deliberately not part of this trait. The two
//! backends are not symmetric there — A/B has no standalone create step, it is
//! inlined in the installer — and forcing a shared shape onto that asymmetry
//! would cost more than it explains.

use super::remove::RemoveOptions;
use super::update::UpdateOptions;
use super::{lvm_ab, remove, rollback, update};
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;

/// A transactional update/rollback backend.
pub trait SnapshotBackend {
    /// Human-readable backend name, for logs and error messages.
    fn name(&self) -> &'static str;

    /// Stage a system update: build the next set/slot and point boot at it.
    fn update(&self, cmd: &CommandRunner, packages: &[String], opts: &UpdateOptions) -> Result<()>;

    /// Return to a previous set/slot. `target` is backend-specific (a set id
    /// or `@` for btrfs, `A`/`B` for LVM A/B); `None` means "the obvious one".
    fn rollback(&self, cmd: &CommandRunner, target: Option<&str>, reboot: bool) -> Result<()>;

    /// Print what `rollback` could be pointed at.
    fn print_rollback_targets(&self, cmd: &CommandRunner) -> Result<()>;

    /// The set or slot staged for the next boot but not yet booted, if any.
    ///
    /// This is what an argument-less [`Self::rollback`] would discard, and so
    /// what a rehearsed transaction has to undo. `Ok(None)` means the system
    /// is running what it will boot: nothing is pending.
    fn staged_change(&self, cmd: &CommandRunner) -> Result<Option<String>>;

    /// `None` when this backend can remove packages transactionally;
    /// otherwise the reason to show the user instead.
    fn remove_refusal(&self) -> Option<&'static str>;

    /// Remove packages transactionally. Only called when
    /// [`Self::remove_refusal`] returned `None`.
    fn remove(&self, cmd: &CommandRunner, packages: &[String], opts: &RemoveOptions) -> Result<()>;
}

/// The btrfs snapshot-set backend.
pub struct Btrfs;

impl SnapshotBackend for Btrfs {
    fn name(&self) -> &'static str {
        "btrfs"
    }

    fn update(&self, cmd: &CommandRunner, packages: &[String], opts: &UpdateOptions) -> Result<()> {
        update::run_update(cmd, packages, opts)
    }

    fn rollback(&self, cmd: &CommandRunner, target: Option<&str>, reboot: bool) -> Result<()> {
        rollback::run_rollback(cmd, target, reboot)
    }

    fn print_rollback_targets(&self, cmd: &CommandRunner) -> Result<()> {
        rollback::print_targets(cmd)
    }

    fn staged_change(&self, cmd: &CommandRunner) -> Result<Option<String>> {
        let staged = super::boot::pointer_set_id(&super::boot::current_boot_pointer(cmd)?)
            .unwrap_or_else(|| super::ROOT_SUBVOL.to_string());
        let session = super::SessionState {
            running: super::boot::running_set_id(),
            staged,
        };
        Ok(session.pending().map(str::to_string))
    }

    fn remove_refusal(&self) -> Option<&'static str> {
        None
    }

    fn remove(&self, cmd: &CommandRunner, packages: &[String], opts: &RemoveOptions) -> Result<()> {
        remove::run_remove(cmd, packages, opts)
    }
}

/// Why the A/B backend turns down `deploytix remove`.
///
/// It could express the same operation (rsync the active root into the
/// inactive slot, `pacman -R` in a chroot, `veritysetup format` a fresh hash,
/// repoint), but that is not implemented — and silently running the btrfs path
/// on an A/B system would edit a root that is not the one that boots.
const AB_REMOVE_REFUSAL: &str = "`deploytix remove` is not implemented for the LVM A/B backend. \
     Remove packages by staging a full `deploytix update` into the inactive \
     slot instead.";

/// The LVM A/B dual-slot backend with dm-verity roots.
pub struct LvmAb;

impl SnapshotBackend for LvmAb {
    fn name(&self) -> &'static str {
        "LVM A/B"
    }

    fn update(&self, cmd: &CommandRunner, packages: &[String], opts: &UpdateOptions) -> Result<()> {
        lvm_ab::run_update(cmd, packages, opts)
    }

    fn rollback(&self, cmd: &CommandRunner, target: Option<&str>, reboot: bool) -> Result<()> {
        lvm_ab::run_rollback(cmd, target, reboot)
    }

    fn print_rollback_targets(&self, cmd: &CommandRunner) -> Result<()> {
        lvm_ab::print_slots(cmd)
    }

    fn staged_change(&self, _cmd: &CommandRunner) -> Result<Option<String>> {
        let state = lvm_ab::read_state()?;
        let session = super::SessionState {
            running: lvm_ab::running_slot(&state),
            staged: state.active.to_uppercase(),
        };
        Ok(session.pending().map(str::to_string))
    }

    fn remove_refusal(&self) -> Option<&'static str> {
        Some(AB_REMOVE_REFUSAL)
    }

    fn remove(
        &self,
        _cmd: &CommandRunner,
        _packages: &[String],
        _opts: &RemoveOptions,
    ) -> Result<()> {
        Err(crate::utils::error::DeploytixError::ConfigError(
            AB_REMOVE_REFUSAL.to_string(),
        ))
    }
}

/// The backend this system is running.
///
/// LVM A/B systems carry the slot-state file on `/boot`; the btrfs backend is
/// the fallback, signalled by the `.deploytix-pair` marker at `/` (which the
/// btrfs operations check for themselves).
pub fn active() -> Box<dyn SnapshotBackend> {
    if lvm_ab::detect() {
        Box::new(LvmAb)
    } else {
        Box::new(Btrfs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_ab_backend_refuses_removal() {
        assert!(Btrfs.remove_refusal().is_none());
        let refusal = LvmAb.remove_refusal().expect("A/B must refuse removal");
        assert!(refusal.contains("not implemented for the LVM A/B backend"));
    }
}
