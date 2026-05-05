//! Device-level preflight checks: existence, mount status, size.

use super::{Phase, PreflightCheck, PreflightContext};
use crate::preflight::report::{CheckResult, CheckStatus};
use std::fs;
use std::path::Path;

// ── DeviceExists ──────────────────────────────────────────────────────

pub struct DeviceExists;

impl PreflightCheck for DeviceExists {
    fn name(&self) -> &str {
        "device.exists"
    }
    fn phase(&self) -> Phase {
        Phase::Preparation
    }
    fn source_module(&self) -> &str {
        "disk/detection.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        let device = &ctx.config.disk.device;
        let path = Path::new(device);

        if !path.exists() {
            return vec![CheckResult {
                status: CheckStatus::Fail,
                operation: format!("Device {} exists", device),
                source: self.source_module().to_string(),
                detail: "Block device not found".to_string(),
            }];
        }

        // Verify it is actually a block device
        let meta = match fs::metadata(device) {
            Ok(m) => m,
            Err(e) => {
                return vec![CheckResult {
                    status: CheckStatus::Fail,
                    operation: format!("Device {} accessible", device),
                    source: self.source_module().to_string(),
                    detail: format!("Cannot stat: {}", e),
                }];
            }
        };

        use std::os::unix::fs::FileTypeExt;
        if !meta.file_type().is_block_device() {
            return vec![CheckResult {
                status: CheckStatus::Fail,
                operation: format!("Device {} is block device", device),
                source: self.source_module().to_string(),
                detail: "Path exists but is not a block device".to_string(),
            }];
        }

        vec![CheckResult {
            status: CheckStatus::Pass,
            operation: format!("Device {} exists", device),
            source: self.source_module().to_string(),
            detail: format!("Block device found, {} MiB", ctx.disk_size_mib),
        }]
    }
}

// ── DeviceNotMounted ──────────────────────────────────────────────────

pub struct DeviceNotMounted;

impl PreflightCheck for DeviceNotMounted {
    fn name(&self) -> &str {
        "device.not_mounted"
    }
    fn phase(&self) -> Phase {
        Phase::Preparation
    }
    fn source_module(&self) -> &str {
        "disk/detection.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        let device = &ctx.config.disk.device;
        let prefix = crate::disk::detection::partition_prefix(device);
        let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();

        let mounted: Vec<&str> = mounts
            .lines()
            .filter_map(|line| {
                let dev = line.split_whitespace().next()?;
                if dev == device || dev.starts_with(&prefix) {
                    Some(dev)
                } else {
                    None
                }
            })
            .collect();

        if mounted.is_empty() {
            vec![CheckResult {
                status: CheckStatus::Pass,
                operation: format!("{} not mounted", device),
                source: self.source_module().to_string(),
                detail: "No partitions currently mounted".to_string(),
            }]
        } else {
            vec![CheckResult {
                status: CheckStatus::Warn,
                operation: format!("{} not mounted", device),
                source: self.source_module().to_string(),
                detail: format!(
                    "{} partition(s) mounted: {}",
                    mounted.len(),
                    mounted.join(", ")
                ),
            }]
        }
    }
}

// ── DeviceSize ────────────────────────────────────────────────────────

pub struct DeviceSize;

impl PreflightCheck for DeviceSize {
    fn name(&self) -> &str {
        "device.size"
    }
    fn phase(&self) -> Phase {
        Phase::Preparation
    }
    fn source_module(&self) -> &str {
        "disk/detection.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        let device = &ctx.config.disk.device;

        // Sum fixed partition sizes from the computed layout
        let fixed_total: u64 = ctx.layout.partitions.iter().map(|p| p.size_mib).sum();

        if ctx.disk_size_mib == 0 {
            return vec![CheckResult {
                status: CheckStatus::Warn,
                operation: format!("{} size check", device),
                source: self.source_module().to_string(),
                detail: "Could not determine disk size".to_string(),
            }];
        }

        if fixed_total > ctx.disk_size_mib {
            vec![CheckResult {
                status: CheckStatus::Fail,
                operation: format!("{} capacity", device),
                source: self.source_module().to_string(),
                detail: format!(
                    "Layout requires {} MiB but disk is only {} MiB",
                    fixed_total, ctx.disk_size_mib
                ),
            }]
        } else {
            let remaining = ctx.disk_size_mib - fixed_total;
            vec![CheckResult {
                status: CheckStatus::Pass,
                operation: format!("{} capacity", device),
                source: self.source_module().to_string(),
                detail: format!(
                    "{} MiB required, {} MiB available ({} MiB free)",
                    fixed_total, ctx.disk_size_mib, remaining
                ),
            }]
        }
    }
}
