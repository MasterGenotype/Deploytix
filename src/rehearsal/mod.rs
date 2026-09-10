//! Rehearsal system.
//!
//! Runs a real operation on the real system with every command recorded, then
//! undoes it. The result is a detailed `RehearsalReport` showing exactly what
//! happened and where it failed (if it did).
//!
//! Three operations can be rehearsed, and the undo differs for each:
//!
//! | Operation | What it does | How it is undone |
//! |-----------|--------------|------------------|
//! | install   | full installer pipeline on the target disk | wipe the disk |
//! | update    | transactional update against the live system | discard the staged set/slot |
//! | remove    | transactional removal against the live system | discard the staged set/slot |
//!
//! Update and removal are the operations where a mistake is hardest to walk
//! back — they run against a system that is already deployed and in use —
//! which is exactly why they are worth rehearsing. Wiping the disk would be
//! the wrong undo for them: there is no throwaway target, only a live machine
//! and a staged change that has not booted yet.

mod guard;
pub mod report;

pub use report::{RehearsalLogLine, RehearsalReport, Restoration};

use crate::config::DeploymentConfig;
use crate::immutable::remove::{run_remove, RemoveOptions};
use crate::immutable::update::{run_update, UpdateOptions};
use crate::install::Installer;
use crate::utils::command::{CommandRunner, OperationRecord};
use guard::{DiskWipeGuard, StagedTransactionGuard};
use report::print_live_record;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Instant;
use tracing::info;

/// What to rehearse.
pub enum RehearsalOp {
    /// A fresh install from this config. Writes to the target disk for real.
    Install(Box<DeploymentConfig>),
    /// A transactional update, optionally installing extra packages.
    Update(Vec<String>),
    /// A transactional removal of these packages.
    Remove(Vec<String>),
}

impl RehearsalOp {
    /// One line describing what is about to happen, for the operator.
    pub fn describe(&self) -> String {
        match self {
            Self::Install(config) => format!(
                "install on {} (the disk will be wiped afterwards)",
                config.disk.device
            ),
            Self::Update(packages) if packages.is_empty() => {
                "system update (the staged set will be discarded afterwards)".to_string()
            }
            Self::Update(packages) => format!(
                "system update plus {} (the staged set will be discarded afterwards)",
                packages.join(", ")
            ),
            Self::Remove(packages) => format!(
                "removal of {} (the staged set will be discarded afterwards)",
                packages.join(", ")
            ),
        }
    }
}

/// Rehearse an operation.
///
/// It:
/// 1. Creates a recording channel so every command is captured.
/// 2. Arms the undo appropriate to the operation.
/// 3. Runs the real operation (not dry-run).
/// 4. Collects all `OperationRecord`s from the channel.
/// 5. Undoes the operation and returns a `RehearsalReport`.
///
/// # Safety
/// This **performs the operation for real**. An install destroys everything on
/// the target device; an update or removal stages a real transaction against
/// the running system before discarding it.
pub fn run_rehearsal(op: RehearsalOp) -> RehearsalReport {
    info!("Starting rehearsal: {}", op.describe());

    let start = Instant::now();

    // Recording channel — the Sender goes into the CommandRunner, and we
    // consume the Receiver in a live-output thread.
    let (tx, rx) = mpsc::channel::<OperationRecord>();

    // Spawn a thread that prints each operation to stderr as it arrives
    // and collects all records for the final report.
    let consumer = thread::spawn(move || {
        let mut records = Vec::new();
        for rec in rx.iter() {
            records.push(rec);
            let idx = records.len();
            print_live_record(idx, &records[idx - 1]);
        }
        records
    });

    let (result, restoration) = match op {
        RehearsalOp::Install(config) => rehearse_install(*config, tx),
        RehearsalOp::Update(packages) => rehearse_transaction(tx, |cmd| {
            run_update(cmd, &packages, &UpdateOptions::default())
        }),
        RehearsalOp::Remove(packages) => rehearse_transaction(tx, |cmd| {
            run_remove(
                cmd,
                &packages,
                &RemoveOptions {
                    assume_yes: true,
                    ..Default::default()
                },
            )
        }),
    };

    let short_circuited_at = match &result {
        Ok(()) => None,
        Err(e) => {
            info!("Rehearsal short-circuited: {}", e);
            Some(format!("{}", e))
        }
    };

    // The Sender was moved into the operation and has been dropped by now, so
    // `rx.iter()` terminates and the consumer thread joins with all records.
    let records = consumer.join().unwrap_or_default();
    let total_duration = start.elapsed();

    info!(
        "Rehearsal complete: {} operations recorded, {} {}",
        records.len(),
        restoration.what,
        restoration.ok
    );

    RehearsalReport {
        records,
        short_circuited_at,
        restoration,
        total_duration,
    }
}

/// Run the installer for real against the target disk, then wipe it.
fn rehearse_install(
    config: DeploymentConfig,
    tx: Sender<OperationRecord>,
) -> (crate::utils::error::Result<()>, Restoration) {
    let device = config.disk.device.clone();

    // RAII guard: no matter what happens below, the disk gets wiped.
    let mut wipe_guard = DiskWipeGuard::new(&device);

    let result = Installer::new(config, false)
        .with_skip_confirm(true)
        .with_recorder(tx)
        .run();

    let ok = wipe_guard.wipe_now();
    (
        result,
        Restoration {
            what: "Disk wiped",
            ok,
        },
    )
}

/// Run a transaction against the live system for real, then discard whatever
/// it staged.
///
/// The recorder attaches to a bare `CommandRunner` — no `Installer` is
/// involved, which is the same shape the GUI update tool already uses.
fn rehearse_transaction<F>(
    tx: Sender<OperationRecord>,
    operation: F,
) -> (crate::utils::error::Result<()>, Restoration)
where
    F: FnOnce(&CommandRunner) -> crate::utils::error::Result<()>,
{
    let mut guard = StagedTransactionGuard::new();

    let result = {
        let cmd = CommandRunner::new(false).with_recorder(tx);
        operation(&cmd)
        // `cmd` drops here, dropping the Sender with it so the consumer
        // thread can finish.
    };

    let ok = guard.discard_now();
    (
        result,
        Restoration {
            what: "Staged change discarded",
            ok,
        },
    )
}
