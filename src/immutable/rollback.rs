//! `deploytix rollback` — return to a previous snapshot set.
//!
//! Rollback is just a boot-pointer move: point the default boot entry at an
//! **older** set's root subvolume (or the pristine `@`) and regenerate the boot
//! configuration. The `.deploytix-pair` marker in that set makes the initramfs
//! mount the matching `/usr` and `/etc`, so the trio comes back together.
//!
//! Rollback moves **backwards only**. Nothing is deleted, so the set you left
//! is still there, but returning to it is not a rollback and this command will
//! not do it: moving forward is `deploytix update`'s job, which builds a new
//! set from the running system and activates it. A command named `rollback`
//! that also rolls forward is a command whose name lies about what it does.
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

/// Whether `target` sits after `current` in the ordered target list — i.e.
/// selecting it would move the boot pointer *forward*, which is not a rollback.
///
/// `targets` is oldest-first (`@`, then set ids ascending). A `current` that is
/// no longer in the list (its set was pruned) is treated as the oldest, so
/// every remaining target counts as forward.
fn is_forward(targets: &[String], current: &str, target: &str) -> bool {
    let pos = |id: &str| targets.iter().position(|t| t == id);
    match (pos(current), pos(target)) {
        (Some(c), Some(t)) => t > c,
        (None, Some(_)) => true,
        _ => false,
    }
}

/// Resolve a rollback selection to a boot pointer subvolume path.
///
/// - `None` → the previous set relative to the current pointer (one step back).
/// - `Some("@")` → the pristine base.
/// - `Some(id)` → that specific set, **provided it is older** than the current
///   target. Rollback moves backwards only; moving forward is what
///   `deploytix update` does, by building a new set from the running system.
pub fn resolve_target(cmd: &CommandRunner, selection: Option<&str>) -> Result<String> {
    match selection {
        Some("@") => Ok("@".to_string()),
        Some(id) => {
            let targets = list_targets(cmd)?;
            if !targets.iter().any(|t| t == id) {
                return Err(DeploytixError::ConfigError(format!(
                    "no snapshot set '{id}' (see `deploytix rollback --list`)"
                )));
            }
            let current = boot::pointer_set_id(&boot::current_boot_pointer(cmd)?)
                .unwrap_or_else(|| "@".to_string());
            if is_forward(&targets, &current, id) {
                return Err(DeploytixError::ConfigError(format!(
                    "set '{id}' is newer than the current target '{current}': rollback only \
                     moves backwards. Use `deploytix update` to move forward — it builds a \
                     new set from the running system and activates it."
                )));
            }
            Ok(snapshot::set_root_subvol(id))
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
    let devices = detect_devices();
    boot::activate_target(cmd, &devices, &pointer)?;
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

    /// Rollback moves backwards. Selecting a newer set is `update`'s job, and
    /// letting `rollback` do it is what made the command's name a lie.
    #[test]
    fn direction_is_decided_by_position_in_the_ordered_targets() {
        let targets: Vec<String> = ["@", "100", "200", "300"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Forward: rejected.
        assert!(is_forward(&targets, "200", "300"));
        assert!(is_forward(&targets, "@", "100"));
        // Backward and staying put: allowed.
        assert!(!is_forward(&targets, "300", "200"));
        assert!(!is_forward(&targets, "300", "@"));
        assert!(!is_forward(&targets, "200", "200"));
        // A current target that no longer exists (its set was pruned) leaves
        // nothing to move back from, so every remaining target is forward.
        assert!(is_forward(&targets, "999", "100"));
    }

    #[test]
    fn the_base_install_is_always_a_valid_rollback_target() {
        // `@` is the oldest target, so it can never be a forward move.
        let cmd = CommandRunner::new(true);
        assert_eq!(resolve_target(&cmd, Some("@")).unwrap(), "@");
    }
}
