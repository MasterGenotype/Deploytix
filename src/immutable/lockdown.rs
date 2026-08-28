//! Pacman lockdown for the immutable model.
//!
//! On an immutable system `/usr` is read-only, so a direct `pacman -Syu` would
//! fail partway with confusing errors. This installs a `PreTransaction` pacman
//! hook that aborts install/upgrade/remove transactions while `/usr` is mounted
//! read-only, pointing the user at `deploytix update` instead.
//!
//! The guard keys off the actual read-only state of `/usr`, so it correctly
//! *allows* the `pacman` that `deploytix update` runs inside a writable set
//! chroot (where `/usr` is mounted read-write).

use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::info;

/// Path (relative to a root) of the pacman lockdown hook. The `00-` prefix runs
/// it before every other hook.
pub const HOOK_REL: &str = "etc/pacman.d/hooks/00-deploytix-immutable.hook";
/// Path (relative to a root) of the guard script the hook invokes.
pub const GUARD_REL: &str = "usr/local/bin/deploytix-immutable-guard";

/// The pacman hook file contents.
pub fn hook_contents() -> &'static str {
    r#"[Trigger]
Operation = Install
Operation = Upgrade
Operation = Remove
Type = Package
Target = *

[Action]
Description = Enforcing immutable root (use `deploytix update`)...
When = PreTransaction
Exec = /usr/local/bin/deploytix-immutable-guard
AbortOnFail
"#
}

/// The guard script contents. Exits non-zero (aborting the transaction) only
/// when `/usr` is read-only — i.e. on the live immutable system, not inside a
/// `deploytix update` chroot.
pub fn guard_script() -> &'static str {
    r#"#!/bin/sh
# deploytix immutable-root guard. Blocks direct pacman on the read-only live
# system; allows it inside a writable transactional-update chroot.
usr_opts=""
if command -v findmnt >/dev/null 2>&1; then
    usr_opts=$(findmnt -no OPTIONS /usr 2>/dev/null)
else
    usr_opts=$(awk '$2=="/usr"{print $4}' /proc/mounts 2>/dev/null)
fi
case ",$usr_opts," in
    *,ro,*)
        echo "deploytix: / and /usr are read-only (immutable system)." >&2
        echo "           Run system updates transactionally with:" >&2
        echo "               deploytix update" >&2
        echo "           (or, to install one-off packages: deploytix update <pkg>...)" >&2
        exit 1
        ;;
esac
exit 0
"#
}

/// Install the pacman lockdown hook and guard script under `install_root`.
pub fn install(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("[immutable] Installing pacman lockdown hook");
    if cmd.is_dry_run() {
        println!("  [dry-run] Would write /{HOOK_REL} and /{GUARD_REL}");
        return Ok(());
    }

    let guard_path = format!("{install_root}/{GUARD_REL}");
    let hook_path = format!("{install_root}/{HOOK_REL}");
    if let Some(parent) = std::path::Path::new(&guard_path).parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = std::path::Path::new(&hook_path).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&guard_path, guard_script())?;
    fs::set_permissions(&guard_path, fs::Permissions::from_mode(0o755))?;
    fs::write(&hook_path, hook_contents())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_runs_pretransaction_and_aborts() {
        let h = hook_contents();
        assert!(h.contains("When = PreTransaction"));
        assert!(h.contains("AbortOnFail"));
        assert!(h.contains("Exec = /usr/local/bin/deploytix-immutable-guard"));
    }

    #[test]
    fn guard_blocks_on_ro_usr_and_is_valid_shell() {
        let g = guard_script();
        assert!(g.contains("findmnt -no OPTIONS /usr"));
        assert!(g.contains("deploytix update"));
        assert!(g.contains("*,ro,*)"));
        if let Ok(status) = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(g)
            .status()
        {
            assert!(status.success(), "guard script is not valid shell");
        }
    }
}
