//! Host binary dependency checks.
//!
//! Derives the required binary list from the config at runtime via
//! `utils::deps::required_binaries()` — the same function the installer
//! uses.  Adding a new filesystem or bootloader to the config
//! automatically extends the preflight check without edits here.

use super::{Phase, PreflightCheck, PreflightContext};
use crate::preflight::report::{CheckResult, CheckStatus};
use crate::utils::command::command_exists;
use crate::utils::deps::required_binaries;

pub struct HostBinaries;

impl PreflightCheck for HostBinaries {
    fn name(&self) -> &str {
        "deps.host_binaries"
    }
    fn phase(&self) -> Phase {
        Phase::Preparation
    }
    fn source_module(&self) -> &str {
        "utils/deps.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        let bins = required_binaries(
            &ctx.config.disk.filesystem,
            &ctx.config.disk.boot_filesystem,
            ctx.config.disk.encryption,
            ctx.config.disk.use_lvm_thin,
            &ctx.config.system.bootloader,
        );

        let mut results = Vec::new();
        let mut missing: Vec<&str> = Vec::new();

        for bin in &bins {
            if !command_exists(bin) {
                missing.push(bin);
            }
        }

        if missing.is_empty() {
            results.push(CheckResult {
                status: CheckStatus::Pass,
                operation: "Host binary dependencies".to_string(),
                source: self.source_module().to_string(),
                detail: format!("All {} required binaries found", bins.len()),
            });
        } else {
            // Emit one FAIL per missing binary for clear attribution
            for bin in &missing {
                results.push(CheckResult {
                    status: CheckStatus::Fail,
                    operation: format!("Binary: {}", bin),
                    source: self.source_module().to_string(),
                    detail: "Not found in PATH".to_string(),
                });
            }
        }

        results
    }
}
