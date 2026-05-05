//! Partition layout preflight checks.

use super::{Phase, PreflightCheck, PreflightContext};
use crate::preflight::report::{CheckResult, CheckStatus};

// ── LayoutFitsDisk ────────────────────────────────────────────────────

pub struct LayoutFitsDisk;

impl PreflightCheck for LayoutFitsDisk {
    fn name(&self) -> &str {
        "layout.fits_disk"
    }
    fn phase(&self) -> Phase {
        Phase::PartitionFormat
    }
    fn source_module(&self) -> &str {
        "disk/layouts.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        let parts = &ctx.layout.partitions;

        // Check for remainder partition (size_mib == 0)
        let has_remainder = parts.iter().any(|p| p.size_mib == 0);

        // Sum all fixed-size partitions
        let fixed_total: u64 = parts.iter().map(|p| p.size_mib).sum();

        // If there is a remainder partition, it needs at least some space
        let min_remainder = if has_remainder { 1024 } else { 0 }; // 1 GiB minimum
        let required = fixed_total + min_remainder;

        if ctx.disk_size_mib > 0 && required > ctx.disk_size_mib {
            vec![CheckResult {
                status: CheckStatus::Fail,
                operation: "Layout fits disk".to_string(),
                source: self.source_module().to_string(),
                detail: format!(
                    "{} partitions totalling {} MiB (+ {} MiB remainder) exceeds {} MiB disk",
                    parts.len(),
                    fixed_total,
                    min_remainder,
                    ctx.disk_size_mib
                ),
            }]
        } else {
            vec![CheckResult {
                status: CheckStatus::Pass,
                operation: "Layout fits disk".to_string(),
                source: self.source_module().to_string(),
                detail: format!(
                    "{} partitions, {} MiB fixed of {} MiB total",
                    parts.len(),
                    fixed_total,
                    ctx.disk_size_mib
                ),
            }]
        }
    }
}

// ── PreserveHomeCompat ────────────────────────────────────────────────

pub struct PreserveHomeCompat;

impl PreflightCheck for PreserveHomeCompat {
    fn name(&self) -> &str {
        "layout.preserve_home"
    }
    fn phase(&self) -> Phase {
        Phase::PartitionFormat
    }
    fn source_module(&self) -> &str {
        "install/installer.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        if !ctx.config.disk.preserve_home {
            return vec![CheckResult {
                status: CheckStatus::Pass,
                operation: "preserve_home compatibility".to_string(),
                source: self.source_module().to_string(),
                detail: "preserve_home not enabled, skipped".to_string(),
            }];
        }

        let mut results = Vec::new();

        // Verify expected partitions exist on disk
        let device = &ctx.config.disk.device;
        for part in &ctx.layout.partitions {
            let part_path = crate::disk::detection::partition_path(device, part.number);
            if !std::path::Path::new(&part_path).exists() {
                results.push(CheckResult {
                    status: CheckStatus::Fail,
                    operation: format!("Partition {} exists", part_path),
                    source: self.source_module().to_string(),
                    detail: format!(
                        "preserve_home: partition {} ({}) not found on disk",
                        part.number, part.name
                    ),
                });
            }
        }

        if results.is_empty() {
            results.push(CheckResult {
                status: CheckStatus::Pass,
                operation: "preserve_home compatibility".to_string(),
                source: self.source_module().to_string(),
                detail: format!(
                    "All {} expected partitions present on {}",
                    ctx.layout.partitions.len(),
                    device
                ),
            });
        }

        results
    }
}
