//! The seam between "run a command" and "run it on this particular host".
//!
//! Everything deploytix does to a target system goes through two host-shaped
//! operations: run something inside the target's chroot, and ask whether a
//! binary is present. Both used to be calls to concrete functions that assume
//! the target is the machine deploytix is running on. That assumption is true
//! today and this module does not change it — [`LocalHost`] is the only
//! implementation, and it is exactly the old behaviour.
//!
//! What it buys is that the assumption now lives in one place. If deploytix
//! ever needs to target a container or a remote machine, that is a second
//! `impl HostAdapter` rather than an edit to every caller.

use crate::utils::command::{command_exists, run_in_artix_chroot};
use crate::utils::error::Result;
use std::process::Output;

/// A machine deploytix can install onto.
///
/// `Send + Sync` because a [`crate::utils::command::CommandRunner`] holds one
/// and gets moved onto the GUI's worker thread.
pub trait HostAdapter: Send + Sync {
    /// Human-readable host name, for logs.
    fn name(&self) -> &'static str;

    /// Run `command` inside the target root mounted at `chroot_path`.
    fn chroot_cmd(&self, chroot_path: &str, command: &str) -> Result<Output>;

    /// Whether `name` is an executable this host can run.
    fn has_binary(&self, name: &str) -> bool;

    /// Which of `required` this host is missing, in the order given.
    fn missing_binaries(&self, required: &[&'static str]) -> Vec<&'static str> {
        required
            .iter()
            .copied()
            .filter(|bin| !self.has_binary(bin))
            .collect()
    }
}

/// The machine deploytix is running on.
pub struct LocalHost;

impl HostAdapter for LocalHost {
    fn name(&self) -> &'static str {
        "local"
    }

    fn chroot_cmd(&self, chroot_path: &str, command: &str) -> Result<Output> {
        run_in_artix_chroot(chroot_path, command)
    }

    fn has_binary(&self, name: &str) -> bool {
        command_exists(name)
    }
}

/// The host this process targets.
pub fn current() -> &'static dyn HostAdapter {
    &LocalHost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_binaries_reports_only_what_is_absent() {
        // `sh` is on every host this can run on; the other cannot exist.
        let missing = current().missing_binaries(&["sh", "deploytix-not-a-real-binary"]);
        assert_eq!(missing, vec!["deploytix-not-a-real-binary"]);
    }
}
