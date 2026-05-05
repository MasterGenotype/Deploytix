//! Rehearsal installation system.
//!
//! Runs the full installer pipeline on the target disk with every command
//! recorded, then wipes the disk to restore pristine state.  The result is
//! a detailed `RehearsalReport` showing exactly what happened and where it
//! failed (if it did).

mod guard;
pub mod report;

pub use report::{RehearsalLogLine, RehearsalReport};

use crate::config::DeploymentConfig;
use crate::install::Installer;
use crate::utils::command::OperationRecord;
use guard::DiskWipeGuard;
use std::sync::mpsc;
use std::time::Instant;
use tracing::info;

/// Run a full rehearsal installation.
///
/// This is the primary entry point.  It:
/// 1. Creates a recording channel so every command is captured.
/// 2. Arms a `DiskWipeGuard` that guarantees the disk is wiped on exit.
/// 3. Runs the real `Installer` (not dry-run) with confirmation skipped.
/// 4. Collects all `OperationRecord`s from the channel.
/// 5. Wipes the disk (via the guard) and returns a `RehearsalReport`.
///
/// # Safety
/// This function **writes to the target disk for real**.  The disk is wiped
/// afterward, but data on the target device **will be destroyed**.
pub fn run_rehearsal(config: &DeploymentConfig) -> RehearsalReport {
    let device = config.disk.device.clone();

    info!(
        "Starting rehearsal installation on {} (disk will be wiped afterward)",
        device
    );

    let start = Instant::now();

    // Recording channel — the Sender goes into the CommandRunner, and we
    // drain the Receiver after the installer finishes.
    let (tx, rx) = mpsc::channel::<OperationRecord>();

    // RAII guard: no matter what happens below, the disk gets wiped.
    let mut wipe_guard = DiskWipeGuard::new(&device);

    // Build the installer in real mode (dry_run = false) with recording
    // enabled and interactive confirmation skipped.
    let installer = Installer::new(config.clone(), false)
        .with_skip_confirm(true)
        .with_recorder(tx);

    // Run the installer.  If it errors, we capture the error as the
    // short-circuit point.
    let result = installer.run();

    let short_circuited_at = match &result {
        Ok(()) => None,
        Err(e) => {
            info!("Rehearsal short-circuited: {}", e);
            Some(format!("{}", e))
        }
    };

    // Collect all recorded operations.  The Sender was moved into the
    // installer and is now dropped (installer consumed by `run()`), so
    // `rx.iter()` will terminate.
    let records: Vec<OperationRecord> = rx.iter().collect();

    // Wipe the disk (cleanup + wipefs + blank GPT).
    let disk_wiped = wipe_guard.wipe_now();

    let total_duration = start.elapsed();

    info!(
        "Rehearsal complete: {} operations recorded, disk_wiped={}",
        records.len(),
        disk_wiped
    );

    RehearsalReport {
        records,
        short_circuited_at,
        disk_wiped,
        total_duration,
    }
}
