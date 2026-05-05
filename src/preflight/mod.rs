//! Dry-run preflight verification system.
//!
//! Runs all registered checks in installer-phase order.  When a check
//! in an earlier phase fails, all checks in later (dependent) phases
//! are automatically skipped — because the operations they validate
//! depend on the success of the operations that came before.
//!
//! Checks may create temporary resources (scratch DBs, generated
//! scripts) but all state is cleaned up when the run completes via
//! RAII guards, leaving the host and target device untouched.

pub mod checks;
pub mod report;

use crate::config::DeploymentConfig;
use crate::disk::detection::get_device_info;
use crate::disk::layouts::compute_layout_from_config;
use checks::{all_checks, Phase, PreflightContext};
use report::{CheckResult, CheckStatus, PreflightReport};
use tracing::info;

/// Phase execution order.  Checks are grouped by phase and run
/// sequentially; a failure in any phase blocks all subsequent phases.
const PHASE_ORDER: &[Phase] = &[
    Phase::Preparation,
    Phase::PartitionFormat,
    Phase::BaseSystem,
    Phase::Configure,
];

/// Run all preflight checks against the given configuration.
///
/// Returns a `PreflightReport` containing per-operation results.
/// The caller decides whether to print it, abort, or continue.
pub fn run_preflight(config: &DeploymentConfig) -> PreflightReport {
    info!("Running preflight verification...");

    // ── Build context ────────────────────────────────────────────────
    // Probe disk size (best-effort; 0 if device doesn't exist yet)
    let disk_size_mib = get_device_info(&config.disk.device)
        .map(|d| d.size_mib())
        .unwrap_or(0);

    // Compute layout (best-effort; use a minimal fallback on error so
    // that config/device checks can still run)
    let layout = compute_layout_from_config(&config.disk, disk_size_mib);
    let fallback_layout;
    let layout_ref = match &layout {
        Ok(l) => l,
        Err(_) => {
            fallback_layout = crate::disk::layouts::ComputedLayout {
                partitions: Vec::new(),
                total_mib: disk_size_mib,
                subvolumes: None,
                planned_thin_volumes: None,
            };
            &fallback_layout
        }
    };

    let mut ctx = PreflightContext {
        config,
        layout: layout_ref,
        disk_size_mib,
        failed_phases: Vec::new(),
    };

    // If layout computation itself failed, record it
    let mut all_results: Vec<CheckResult> = Vec::new();
    if let Err(ref e) = layout {
        all_results.push(CheckResult {
            status: CheckStatus::Fail,
            operation: "Compute partition layout".to_string(),
            source: "disk/layouts.rs".to_string(),
            detail: format!("{}", e),
        });
        ctx.failed_phases.push(Phase::Preparation);
    }

    // ── Run checks in phase order ────────────────────────────────────
    let checks = all_checks();

    for &phase in PHASE_ORDER {
        // If any prerequisite phase has failed, skip this entire phase
        let dominated = match phase {
            Phase::Preparation => false,
            Phase::PartitionFormat => ctx.failed_phases.contains(&Phase::Preparation),
            Phase::BaseSystem => {
                ctx.failed_phases.contains(&Phase::Preparation)
                    || ctx.failed_phases.contains(&Phase::PartitionFormat)
            }
            Phase::Configure => {
                ctx.failed_phases.contains(&Phase::Preparation)
                    || ctx.failed_phases.contains(&Phase::PartitionFormat)
                    || ctx.failed_phases.contains(&Phase::BaseSystem)
            }
        };

        // Collect checks belonging to this phase
        let phase_checks: Vec<&Box<dyn checks::PreflightCheck>> =
            checks.iter().filter(|c| c.phase() == phase).collect();

        if dominated {
            // Emit a single SKIP entry for the entire blocked phase
            let phase_name = match phase {
                Phase::Preparation => "Preparation",
                Phase::PartitionFormat => "Partitioning & Formatting",
                Phase::BaseSystem => "Base System",
                Phase::Configure => "Configuration",
            };
            for check in &phase_checks {
                all_results.push(CheckResult {
                    status: CheckStatus::Warn,
                    operation: format!("[SKIP] {}", check.name()),
                    source: check.source_module().to_string(),
                    detail: format!("Skipped — upstream {} phase failed", phase_name),
                });
            }
            continue;
        }

        // Run each check in this phase
        let mut phase_had_failure = false;
        for check in &phase_checks {
            let results = check.run(&ctx);
            for r in &results {
                if r.status == CheckStatus::Fail {
                    phase_had_failure = true;
                }
            }
            all_results.extend(results);
        }

        if phase_had_failure {
            ctx.failed_phases.push(phase);
        }
    }

    info!(
        "Preflight complete: {} check results collected",
        all_results.len()
    );

    PreflightReport {
        results: all_results,
    }
}
