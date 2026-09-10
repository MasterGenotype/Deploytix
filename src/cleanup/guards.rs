//! RAII tracking for the resources an install opens.
//!
//! An install mounts filesystems, opens LUKS containers and activates a volume
//! group. Every one of those has to be released again, and until now the only
//! thing that released them was someone remembering to call
//! `Installer::emergency_cleanup()`, which works out what to tear down by
//! re-reading `/proc/mounts` and `/dev/mapper` after the fact.
//!
//! [`ResourceStack`] records each resource as it is opened and releases them in
//! reverse order when it is dropped, so an install that panics — or returns
//! down a path nobody thought to add a cleanup call to — still releases what it
//! took. `DiskWipeGuard` in `crate::rehearsal::guard` is the same idea applied
//! to the rehearsal disk.
//!
//! The stack is the *backstop*, not the main path: `emergency_cleanup` still
//! runs first on an ordinary failure, because its rescan also catches things no
//! guard can know about (cryptsetup's own `temporary-cryptsetup-*` mappings)
//! and its commands go through the `CommandRunner`, so a rehearsal records
//! them. When it has done its work it disarms the stack.

use std::process::{Command, Stdio};
use tracing::{info, warn};

/// Something an install opened and must release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    /// Everything mounted under this path, released deepest-first.
    ///
    /// The mount *tree* rather than each individual mount: filesystems are
    /// mounted from half a dozen places in `disk/` and `install/`, and what
    /// has to come back off is "whatever ended up under the install root",
    /// which `/proc/mounts` answers exactly.
    MountTree(String),
    /// An open LUKS container, by mapper name (no `/dev/mapper/` prefix).
    Luks(String),
    /// An active LVM volume group, by name.
    VolumeGroup(String),
}

/// The resources an install has opened, in the order it opened them.
pub struct ResourceStack {
    entries: Vec<Resource>,
    dry_run: bool,
    armed: bool,
}

impl ResourceStack {
    pub fn new(dry_run: bool) -> Self {
        Self {
            entries: Vec::new(),
            dry_run,
            armed: true,
        }
    }

    /// Record a resource. Recording the same one twice is a no-op, so callers
    /// can register on every path that might open it.
    pub fn register(&mut self, resource: Resource) {
        if self.entries.contains(&resource) {
            return;
        }
        info!("[cleanup] tracking {:?}", resource);
        self.entries.push(resource);
    }

    /// Stop tracking: dropping the stack becomes a no-op.
    ///
    /// Called when the resources have been released deliberately — at the end
    /// of a successful install, or after `emergency_cleanup` has done the same
    /// work more thoroughly.
    pub fn disarm(&mut self) {
        self.armed = false;
        self.entries.clear();
    }

    /// What is currently tracked, outermost first.
    pub fn tracked(&self) -> &[Resource] {
        &self.entries
    }

    /// Release everything, most recently opened first.
    ///
    /// Best-effort by construction: this runs from `Drop`, where propagating
    /// an error is not an option and panicking is worse.
    fn release_all(&mut self) {
        for resource in self.entries.drain(..).rev() {
            match resource {
                Resource::MountTree(ref root) => release_mount_tree(root, self.dry_run),
                Resource::Luks(ref name) => {
                    run_quiet("cryptsetup", &["close", name], self.dry_run);
                }
                Resource::VolumeGroup(ref vg) => {
                    run_quiet("vgchange", &["-an", vg], self.dry_run);
                }
            }
        }
    }
}

impl Drop for ResourceStack {
    fn drop(&mut self) {
        if !self.armed || self.entries.is_empty() {
            return;
        }
        warn!(
            "[cleanup] install dropped with {} resource(s) still open — releasing",
            self.entries.len()
        );
        self.release_all();
    }
}

/// Unmount everything under `root`, deepest mount point first.
fn release_mount_tree(root: &str, dry_run: bool) {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return;
    };
    let mut points: Vec<&str> = mounts
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            (parts.len() >= 2 && parts[1].starts_with(root)).then_some(parts[1])
        })
        .collect();
    points.sort_by_key(|p| std::cmp::Reverse(p.matches('/').count()));

    for mp in points {
        if !run_quiet("umount", &[mp], dry_run) {
            run_quiet("umount", &["-l", mp], dry_run);
        }
    }
}

/// Run a release command, swallowing every failure. Returns whether it worked.
fn run_quiet(program: &str, args: &[&str], dry_run: bool) -> bool {
    if dry_run {
        println!("  [dry-run] {} {}", program, args.join(" "));
        return true;
    }
    info!("[cleanup] {} {}", program, args.join(" "));
    Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registering_the_same_resource_twice_tracks_it_once() {
        let mut stack = ResourceStack::new(true);
        stack.register(Resource::Luks("Crypt-Root".into()));
        stack.register(Resource::Luks("Crypt-Root".into()));
        stack.register(Resource::Luks("Crypt-Home".into()));
        assert_eq!(stack.tracked().len(), 2);
        stack.disarm();
    }

    #[test]
    fn disarming_clears_what_drop_would_release() {
        let mut stack = ResourceStack::new(true);
        stack.register(Resource::VolumeGroup("deploytix".into()));
        stack.disarm();
        assert!(stack.tracked().is_empty());
    }
}
