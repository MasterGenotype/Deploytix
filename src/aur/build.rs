//! Disk-backed build scratch for package builds inside a chroot.
//!
//! # Why this exists
//!
//! Both chroot paths give the target a RAM-backed `/tmp`. `artix-chroot`
//! mounts a tmpfs there itself (noted at the `install_yay` call site, which
//! works around it by doing everything in one invocation), and the plain-chroot
//! fallback reproduces that in
//! [`crate::utils::command::chroot_api_setup_cmd`]. Neither passes `size=`, so
//! the kernel caps the tmpfs at **half of physical RAM**.
//!
//! That is the same trap `docs/TMP_DISK_BACKED.md` describes for the booted
//! system: a build tree large enough to matter (a kernel, a browser) hits
//! `ENOSPC` while the root volume still has tens of gigabytes free. The `@tmp`
//! subvolume fixed it for the booted system and does nothing for a chroot,
//! which gets a fresh tmpfs regardless.
//!
//! Note what the bug is *not*: `makepkg`'s default `BUILDDIR` is the directory
//! holding the PKGBUILD, so a helper that clones into the user's cache already
//! builds on disk. The exposure is anything that names `/tmp` explicitly —
//! [`crate::install::packages::install_yay`] built in `/tmp/yay-build` before
//! this module existed, and PKGBUILDs and vendor tools do the same.
//!
//! # What this does
//!
//! Puts the scratch on `/var`, which [`crate::immutable::update::mount_set_cmd`]
//! already rbinds into every set, so it needs no new mount plumbing and the
//! same absolute path is valid on the host and inside the chroot. `/var` is
//! shared across snapshot sets, which is the right lifetime for a build cache:
//! it should outlive the transaction that produced it.
//!
//! The environment variables come from `makepkg` itself rather than any
//! helper's flags. Every AUR helper ends up invoking `makepkg`, and `makepkg`
//! reads these from the environment in preference to `makepkg.conf`, so one
//! mechanism covers every helper — including ones whose own flags differ or
//! do not exist.

use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use tracing::info;

/// Root of the disk-backed build scratch, on the shared writable `/var`.
///
/// The same absolute path on the host and inside a mounted set, because `/var`
/// is rbound into the set at the same place.
pub const BUILD_ROOT: &str = "/var/cache/deploytix/build";

/// Subdirectories of [`BUILD_ROOT`], one per `makepkg` output kind.
///
/// Kept apart so a cleanup can drop compiled trees (`build`, `log`) while
/// keeping the expensive downloads (`src`) and the built packages (`pkg`).
const SUBDIRS: &[&str] = &["build", "src", "pkg", "srcpkg", "log"];

/// Absolute path of one subdirectory.
fn subdir(name: &str) -> String {
    format!("{BUILD_ROOT}/{name}")
}

/// The `makepkg` environment that redirects every scratch and output path onto
/// [`BUILD_ROOT`].
///
/// Returned as `KEY=value` pairs rather than a shell string so callers can
/// quote them however their invocation needs.
///
/// `TMPDIR` is included because it is the one knob that catches build steps
/// which never consult `makepkg.conf` at all — a PKGBUILD calling `mktemp -d`,
/// or a vendored build tool. It does not help anything that hardcodes the
/// literal `/tmp`; nothing in the environment can.
pub fn build_env() -> Vec<(String, String)> {
    vec![
        ("BUILDDIR".to_string(), subdir("build")),
        ("SRCDEST".to_string(), subdir("src")),
        ("PKGDEST".to_string(), subdir("pkg")),
        ("SRCPKGDEST".to_string(), subdir("srcpkg")),
        ("LOGDEST".to_string(), subdir("log")),
        ("TMPDIR".to_string(), subdir("build")),
    ]
}

/// [`build_env`] rendered as a shell prefix, e.g. `BUILDDIR=... SRCDEST=... `.
///
/// Ends with a trailing space when non-empty so it can be prepended directly to
/// a command. Values are paths this module owns — no user input reaches them —
/// so they need no quoting beyond the fixed form.
pub fn build_env_prefix() -> String {
    build_env()
        .into_iter()
        .map(|(k, v)| format!("{k}={v} "))
        .collect()
}

/// Shell that creates [`BUILD_ROOT`] and its subdirectories inside `target`,
/// owned by `user` so an unprivileged `makepkg` can write to them.
///
/// Idempotent: `mkdir -p` and `chown` both no-op on an existing tree, so it is
/// safe to run before every build.
pub fn ensure_build_root_cmd(user: &str) -> String {
    let dirs: Vec<String> = SUBDIRS.iter().map(|d| subdir(d)).collect();
    format!(
        "mkdir -p {dirs} && chown -R {user}:{user} {root}",
        dirs = dirs.join(" "),
        user = user,
        root = BUILD_ROOT,
    )
}

/// Create the build scratch inside a mounted set, owned by `user`.
///
/// `target` is the chroot root; pass `""` to act on the live system.
pub fn ensure_build_root(cmd: &CommandRunner, target: &str, user: &str) -> Result<()> {
    info!("[aur] Preparing disk-backed build scratch at {BUILD_ROOT}");
    if cmd.is_dry_run() {
        println!("  [dry-run] Would create {BUILD_ROOT} owned by {user}");
        return Ok(());
    }
    cmd.run_in_chroot(target, &ensure_build_root_cmd(user))
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_root_is_under_var_so_it_is_rbound_into_a_set() {
        // /var is the only writable tree mount_set_cmd rbinds that is shared
        // across sets. Moving this off /var means the path inside the chroot
        // stops matching the path on the host.
        assert!(BUILD_ROOT.starts_with("/var/"));
    }

    #[test]
    fn build_root_is_not_under_tmp() {
        // The whole point: /tmp is a half-RAM tmpfs in both chroot paths.
        assert!(!BUILD_ROOT.starts_with("/tmp"));
    }

    #[test]
    fn env_redirects_every_makepkg_output_path() {
        let env = build_env();
        for key in ["BUILDDIR", "SRCDEST", "PKGDEST", "SRCPKGDEST", "LOGDEST"] {
            let (_, value) = env
                .iter()
                .find(|(k, _)| k == key)
                .unwrap_or_else(|| panic!("{key} missing from build_env()"));
            assert!(
                value.starts_with(BUILD_ROOT),
                "{key} points outside the build root: {value}"
            );
        }
    }

    #[test]
    fn env_sets_tmpdir_away_from_tmp() {
        let env = build_env();
        let (_, tmpdir) = env.iter().find(|(k, _)| k == "TMPDIR").expect("TMPDIR");
        assert!(!tmpdir.starts_with("/tmp"));
    }

    #[test]
    fn prefix_is_prependable() {
        let prefix = build_env_prefix();
        assert!(
            prefix.ends_with(' '),
            "prefix must separate from the command"
        );
        assert!(prefix.contains("BUILDDIR=/var/cache/deploytix/build/build"));
        // Prepending it must produce something that still parses as a command.
        let rendered = format!("{prefix}makepkg -si");
        assert!(rendered.ends_with("makepkg -si"));
    }

    #[test]
    fn ensure_cmd_creates_every_subdir_and_chowns_the_root() {
        let sh = ensure_build_root_cmd("builder");
        for d in SUBDIRS {
            assert!(sh.contains(&subdir(d)), "missing subdir {d}");
        }
        assert!(sh.contains("chown -R builder:builder /var/cache/deploytix/build"));
    }

    #[test]
    fn ensure_cmd_is_idempotent_in_shape() {
        // mkdir -p and chown -R both no-op on an existing tree; a build that
        // runs this every time must not fail the second time.
        let sh = ensure_build_root_cmd("builder");
        assert!(sh.contains("mkdir -p"));
        assert!(!sh.contains("mkdir ") || sh.contains("mkdir -p"));
    }
}
