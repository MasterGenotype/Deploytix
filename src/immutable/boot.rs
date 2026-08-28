//! Boot pointer for the transactional immutable model.
//!
//! The "current" system is selected by the `rootflags=subvol=<path>` value in
//! `GRUB_CMDLINE_LINUX_DEFAULT` (written by the bootloader configurator). The
//! initramfs `mountcrypt` hook reads that subvol from the kernel cmdline, mounts
//! it read-only as `/`, and consults its `.deploytix-pair` marker for the
//! matching `/usr` and `/etc`. Switching the pointer + regenerating grub.cfg is
//! therefore all it takes to make an update or rollback take effect on reboot.

use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use tracing::info;

/// Path to GRUB's environment file on a deploytix system.
pub const GRUB_DEFAULT: &str = "/etc/default/grub";
/// Where grub.cfg is regenerated.
pub const GRUB_CFG: &str = "/boot/grub/grub.cfg";

/// The `sed` program that rewrites the `rootflags=subvol=` pointer in
/// `GRUB_DEFAULT` to `root_subvol`. A `|` delimiter avoids escaping the
/// slashes in set subvolume paths (e.g. `@deploytix-sets/<id>/root`).
pub fn set_boot_pointer_sed(root_subvol: &str) -> String {
    format!(
        "sed -i 's|rootflags=subvol=[^ \"]*|rootflags=subvol={}|' {}",
        root_subvol, GRUB_DEFAULT
    )
}

/// Point the default boot entry at `root_subvol` (e.g. `@` or
/// `@deploytix-sets/<id>/root`) and regenerate grub.cfg. Runs on the live system.
pub fn set_boot_pointer(cmd: &CommandRunner, root_subvol: &str) -> Result<()> {
    info!(
        "[immutable] Setting default boot pointer to {}",
        root_subvol
    );
    cmd.run("sh", &["-c", &set_boot_pointer_sed(root_subvol)])?;
    cmd.run("grub-mkconfig", &["-o", GRUB_CFG])?;
    Ok(())
}

/// Read the current `rootflags=subvol=` pointer from `GRUB_DEFAULT`.
/// Returns `@` when no pointer is present (a freshly installed system).
pub fn current_boot_pointer(cmd: &CommandRunner) -> Result<String> {
    if cmd.is_dry_run() {
        return Ok("@".to_string());
    }
    let script = format!(
        "grep -o 'rootflags=subvol=[^ \"]*' {} | head -n1 | sed 's/rootflags=subvol=//'",
        GRUB_DEFAULT
    );
    let out = cmd.run("sh", &["-c", &script])?;
    let ptr = out
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    Ok(if ptr.is_empty() { "@".to_string() } else { ptr })
}

/// The set id embedded in a boot pointer, if it points at a snapshot set.
/// `@deploytix-sets/<id>/root` → `Some(<id>)`; `@` → `None`.
pub fn pointer_set_id(pointer: &str) -> Option<String> {
    let rest = pointer.strip_prefix("@deploytix-sets/")?;
    rest.strip_suffix("/root").map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sed_uses_pipe_delimiter_and_targets_grub_default() {
        let s = set_boot_pointer_sed("@deploytix-sets/123/root");
        assert!(s.contains("s|rootflags=subvol=[^ \"]*|rootflags=subvol=@deploytix-sets/123/root|"));
        assert!(s.ends_with("/etc/default/grub"));
        if let Ok(status) = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(&s)
            .status()
        {
            assert!(status.success());
        }
    }

    #[test]
    fn pointer_set_id_roundtrips() {
        assert_eq!(
            pointer_set_id("@deploytix-sets/123/root"),
            Some("123".to_string())
        );
        assert_eq!(pointer_set_id("@"), None);
        assert_eq!(pointer_set_id("@deploytix-sets/123/usr"), None);
    }
}
