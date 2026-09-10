//! Where deploytix puts its own working files.
//!
//! Everything here used to be a `/tmp/...` literal, which quietly assumed a
//! writable `/tmp`. On a deployed **immutable** system that assumption does not
//! hold: the LVM A/B backend mounts `/` read-only from a dm-verity slot and
//! gives only `/etc` an overlay, so `/tmp` is a read-only directory inside the
//! sealed image. Running the installer, the ISO build, or anything else from
//! such a machine failed at the first `mkdir`.
//!
//! There are two kinds of working file and they want different homes:
//!
//! - **[`runtime_dir`]** — mount points and generated scripts. Small,
//!   short-lived, root-only, and worthless after a reboot. `/run` is tmpfs, is
//!   writable on every layout deploytix supports (both immutable backends
//!   included), and is already where the rest of the immutable machinery puts
//!   its scratch mounts.
//!
//! - **[`cache_dir`]** — the local `[deploytix]` package repository and the
//!   generated `pacman.conf`. Hundreds of megabytes, so not RAM; and it has to
//!   be reachable from inside a transactional chroot, which rbinds only `/var`,
//!   `/home` and `/boot`. A repo under `/tmp` or `/run` is invisible to
//!   `deploytix update`, so `/var` is not merely the writable choice here — it
//!   is the only one that can serve a transactional install.
//!
//! Both fall back through alternatives rather than assuming, and both cache the
//! choice so a run cannot end up with its repo split across two directories.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::{debug, warn};

/// Candidates for [`runtime_dir`], best first.
const RUNTIME_CANDIDATES: &[&str] = &["/run/deploytix", "/var/run/deploytix", "/tmp/deploytix"];

/// Candidates for [`cache_dir`], best first. All under `/var` except the last
/// resort, because only `/var` is rbound into a transactional chroot.
const CACHE_CANDIDATES: &[&str] = &[
    "/var/cache/deploytix",
    "/var/tmp/deploytix",
    "/tmp/deploytix",
];

/// Whether `dir` exists (or can be created) *and* can be written to.
///
/// Existence is not enough: a live medium can carry a perfectly good
/// `/var/lib/deploytix-repo` on a read-only mount, where `create_dir_all`
/// succeeds trivially and the first `repo-add` then fails with EROFS. The
/// probe file is what actually answers the question.
pub fn dir_is_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".deploytix-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// First usable candidate, or the last one as a last resort so callers always
/// get a path and fail at the operation that actually needs it — with an error
/// naming the real problem rather than a panic here.
fn first_usable(candidates: &'static [&'static str], what: &str) -> PathBuf {
    for c in candidates {
        let p = PathBuf::from(c);
        if dir_is_writable(&p) {
            debug!("[paths] Using {} for {}", p.display(), what);
            return p;
        }
    }
    let fallback = PathBuf::from(candidates[candidates.len() - 1]);
    warn!(
        "[paths] No writable directory for {} (tried {}); falling back to {}",
        what,
        candidates.join(", "),
        fallback.display()
    );
    fallback
}

/// Root-only scratch for mount points and generated scripts (`/run/deploytix`).
///
/// tmpfs, so it is writable even when `/` is a read-only verity image, and it
/// is gone after a reboot — which is the right lifetime for a mount point or an
/// sfdisk script.
pub fn runtime_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| first_usable(RUNTIME_CANDIDATES, "runtime scratch"))
}

/// Disk-backed working space under `/var` (`/var/cache/deploytix`).
///
/// Large enough for a package repository, and on the one filesystem that a
/// transactional chroot can see.
pub fn cache_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| first_usable(CACHE_CANDIDATES, "package cache"))
}

/// A named path inside [`runtime_dir`], as a `String` for the many callers that
/// pass paths straight to a command.
pub fn runtime_path(name: &str) -> String {
    runtime_dir().join(name).to_string_lossy().into_owned()
}

/// A named path inside [`cache_dir`].
pub fn cache_path(name: &str) -> String {
    cache_dir().join(name).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo has to be somewhere a transactional chroot can see, and that
    /// chroot rbinds `/var` and nothing else that could hold a package cache.
    /// A `/tmp` or `/run` repo is invisible to `deploytix update`.
    #[test]
    fn the_package_cache_prefers_var() {
        assert!(
            CACHE_CANDIDATES[0].starts_with("/var/"),
            "the first choice must be on /var, got {}",
            CACHE_CANDIDATES[0]
        );
        assert!(
            CACHE_CANDIDATES[..CACHE_CANDIDATES.len() - 1]
                .iter()
                .all(|c| c.starts_with("/var/")),
            "only the last resort may leave /var: {CACHE_CANDIDATES:?}"
        );
    }

    /// `/run` is tmpfs on every layout, including a dm-verity root where `/` and
    /// `/tmp` are read-only. That is the whole reason these moved.
    #[test]
    fn runtime_scratch_prefers_run() {
        assert_eq!(RUNTIME_CANDIDATES[0], "/run/deploytix");
    }

    /// A directory that exists on a read-only mount is not usable, and
    /// `create_dir_all` returning Ok on it is exactly the trap this avoids.
    #[test]
    fn usability_is_decided_by_writing_not_by_existing() {
        let dir = std::env::temp_dir().join(format!("deploytix-paths-{}", std::process::id()));
        assert!(dir_is_writable(&dir), "a fresh temp dir must be usable");
        // The probe must not be left behind for the caller to trip over.
        assert!(!dir.join(".deploytix-write-probe").exists());
        assert!(!dir_is_writable(Path::new("/proc/deploytix-cannot-exist")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn named_paths_sit_inside_their_directory() {
        assert!(runtime_path("partition_script").ends_with("/partition_script"));
        assert!(cache_path("repo").ends_with("/repo"));
        assert!(runtime_path("x").starts_with(&*runtime_dir().to_string_lossy()));
        assert!(cache_path("x").starts_with(&*cache_dir().to_string_lossy()));
    }
}
