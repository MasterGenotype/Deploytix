//! Friendly "use `deploytix update`" nudge for the immutable model.
//!
//! **Enforcement is the read-only `/usr` mount, not this file.** On an immutable
//! system `pacman -Syu` physically cannot modify `/usr`, so it fails — the point
//! here is only to fail *early and helpfully*.
//!
//! We deliberately do **not** use a pacman `PreTransaction` hook: `basestrap`/
//! `pacstrap` run `pacman -r <newroot>`, which reads hooks from the *host's*
//! hookdir but runs each hook's `Exec` chrooted into the (possibly empty) new
//! root. A `Target = *` hook execing any binary therefore aborts every
//! install-to-another-root — breaking ISO builds and deploytix's own deploys
//! from an immutable machine. Instead we install a `/etc/profile.d` snippet that
//! intercepts *interactive* `pacman` upgrade/install/remove and points the user
//! at `deploytix update`. It never affects scripts, `sudo`, or `pacman -r`.

use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// Path (relative to a root) of the interactive nudge sourced by login shells.
pub const PROFILE_REL: &str = "etc/profile.d/deploytix-immutable.sh";

/// The `/etc/profile.d` snippet. Guarded so it only activates on a live
/// immutable system (pairing marker present *and* `/usr` mounted read-only), and
/// only wraps interactive shells — `command pacman` bypasses it.
pub fn profile_snippet() -> &'static str {
    r#"# deploytix: nudge interactive pacman toward `deploytix update` on immutable
# systems. Enforcement is the read-only /usr mount; this is only a friendly,
# interactive-only hint (never affects scripts, sudo, or `pacman -r`).
if [ -e /.deploytix-pair ]; then
    _dpx_usr_ro=0
    if command -v findmnt >/dev/null 2>&1; then
        case ",$(findmnt -no OPTIONS /usr 2>/dev/null)," in *,ro,*) _dpx_usr_ro=1 ;; esac
    fi
    if [ "$_dpx_usr_ro" = 1 ]; then
        pacman() {
            case "$1" in
                -S*|-U*|-R*|--sync|--upgrade|--remove)
                    echo "deploytix: / and /usr are read-only (immutable system)." >&2
                    echo "           Update transactionally with:  deploytix update" >&2
                    echo "           (bypass for this shell:        command pacman ...)" >&2
                    return 1 ;;
            esac
            command pacman "$@"
        }
    fi
    unset _dpx_usr_ro
fi
"#
}

/// Install the interactive nudge under `install_root`.
pub fn install(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("[immutable] Installing immutable-root shell nudge (/etc/profile.d)");
    if cmd.is_dry_run() {
        println!("  [dry-run] Would write /{PROFILE_REL}");
        return Ok(());
    }
    let path = format!("{install_root}/{PROFILE_REL}");
    if let Some(parent) = std::path::Path::new(&path).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, profile_snippet())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_is_interactive_only_and_guarded() {
        let s = profile_snippet();
        // Guarded on the live immutable signals.
        assert!(s.contains("/.deploytix-pair"));
        assert!(s.contains("findmnt -no OPTIONS /usr"));
        assert!(s.contains("*,ro,*"));
        // Wraps the shell builtin (interactive only) and offers a bypass.
        assert!(s.contains("pacman() {"));
        assert!(s.contains("command pacman \"$@\""));
        assert!(s.contains("deploytix update"));
    }

    #[test]
    fn snippet_is_valid_shell() {
        if let Ok(status) = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(profile_snippet())
            .status()
        {
            assert!(status.success(), "profile snippet is not valid shell");
        }
    }

    #[test]
    fn install_is_dry_run_safe() {
        let cmd = CommandRunner::new(true);
        install(&cmd, "/mnt/target").unwrap();
    }
}
