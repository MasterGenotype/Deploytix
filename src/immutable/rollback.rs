//! `deploytix rollback` — return to a previous snapshot set.
//!
//! Rollback is just a boot-pointer move: point the default boot entry at an
//! older set's root subvolume (or the pristine `@`) and regenerate grub.cfg. The
//! `.deploytix-pair` marker in that set makes the initramfs mount the matching
//! `/usr` and `/etc`, so `{@, @usr, @etc}` come back together. Nothing is
//! deleted, so a rollback is itself reversible (roll "forward" to a newer set).
//!
//! The interactive grub-btrfs menu remains available as a recovery path if the
//! pointer ever selects an unbootable set.

use crate::immutable::{boot, detect_devices, snapshot};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use tracing::info;

/// Available rollback targets, newest last. `"@"` (the pristine base) is always
/// offered first.
pub fn list_targets(cmd: &CommandRunner) -> Result<Vec<String>> {
    let devices = detect_devices();
    let mut targets = vec!["@".to_string()];
    targets.extend(snapshot::list_sets(cmd, &devices.root_fs)?);
    Ok(targets)
}

/// Print the rollback targets, marking the one currently selected for boot.
pub fn print_targets(cmd: &CommandRunner) -> Result<()> {
    let current = boot::pointer_set_id(&boot::current_boot_pointer(cmd)?);
    let current = current.unwrap_or_else(|| "@".to_string());
    println!("Available boot targets (current marked *):");
    for t in list_targets(cmd)? {
        let marker = if t == current { " *" } else { "" };
        let label = if t == "@" { "@ (base install)" } else { &t };
        println!("  {label}{marker}");
    }
    Ok(())
}

/// Resolve a rollback selection to a boot pointer subvolume path.
///
/// - `None` → the previous set relative to the current pointer (one step back).
/// - `Some("@")` → the pristine base.
/// - `Some(id)` → that specific set.
pub fn resolve_target(cmd: &CommandRunner, selection: Option<&str>) -> Result<String> {
    match selection {
        Some("@") => Ok("@".to_string()),
        Some(id) => {
            let devices = detect_devices();
            let sets = snapshot::list_sets(cmd, &devices.root_fs)?;
            if sets.iter().any(|s| s == id) {
                Ok(snapshot::set_root_subvol(id))
            } else {
                Err(DeploytixError::ConfigError(format!(
                    "no snapshot set '{id}' (see `deploytix rollback --list`)"
                )))
            }
        }
        None => {
            // One step back from the current pointer in the ordered target list.
            let targets = list_targets(cmd)?;
            let current = boot::pointer_set_id(&boot::current_boot_pointer(cmd)?)
                .unwrap_or_else(|| "@".to_string());
            let idx = targets.iter().position(|t| *t == current).unwrap_or(0);
            if idx == 0 {
                return Err(DeploytixError::ConfigError(
                    "already at the oldest target (@); nothing to roll back to".to_string(),
                ));
            }
            let prev = &targets[idx - 1];
            Ok(if prev == "@" {
                "@".to_string()
            } else {
                snapshot::set_root_subvol(prev)
            })
        }
    }
}

/// Perform a rollback to `selection` (see [`resolve_target`]).
pub fn run_rollback(cmd: &CommandRunner, selection: Option<&str>, reboot: bool) -> Result<()> {
    let pointer = resolve_target(cmd, selection)?;
    info!("[immutable] Rolling back: default boot -> {}", pointer);
    boot::set_boot_pointer(cmd, &pointer)?;
    info!(
        "[immutable] Rollback staged. Reboot to activate {}.",
        pointer
    );
    if reboot {
        cmd.run("reboot", &[])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_target_resolves_to_at() {
        let cmd = CommandRunner::new(true);
        assert_eq!(resolve_target(&cmd, Some("@")).unwrap(), "@");
    }

    #[test]
    fn unknown_set_is_rejected() {
        // Dry-run list_sets returns empty, so any specific id is unknown.
        let cmd = CommandRunner::new(true);
        assert!(resolve_target(&cmd, Some("999")).is_err());
    }
}
