//! Preflight check trait, context, and registry.

pub mod config;
pub mod deps;
pub mod device;
pub mod filesystem;
pub mod layout;
pub mod packages;
pub mod services;

use crate::config::DeploymentConfig;
use crate::disk::layouts::ComputedLayout;
use crate::preflight::report::CheckResult;

/// Installer phase a check corresponds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Phase 1: preparation, dependency checking, config validation
    Preparation,
    /// Phase 2: partitioning, formatting, encryption, mounting
    PartitionFormat,
    /// Phase 3: basestrap, fstab, crypttab
    BaseSystem,
    /// Phase 4: system configuration, bootloader, services
    Configure,
}

/// Context shared by all preflight checks.
///
/// Checks run in phase order.  When a check in an earlier phase fails,
/// all checks in later (dependent) phases are automatically skipped
/// because their preconditions are not met.  `failed_phases` is
/// maintained by the runner — individual checks just read it.
pub struct PreflightContext<'a> {
    pub config: &'a DeploymentConfig,
    pub layout: &'a ComputedLayout,
    pub disk_size_mib: u64,
    /// Phases that have recorded at least one Fail result.
    /// Populated by the runner between phases; checks can inspect
    /// this to understand upstream failures.
    pub failed_phases: Vec<Phase>,
}

/// A single preflight check.
///
/// Implementations are stateless and read-only — they inspect the
/// `PreflightContext` and the host system (PATH, /sys, /proc, sync DB)
/// but never mutate anything.
pub trait PreflightCheck {
    /// Short identifier (e.g. "device.exists").
    fn name(&self) -> &str;

    /// Which installer phase this check validates.
    fn phase(&self) -> Phase;

    /// Source module in src/ that would execute this operation
    /// (e.g. "disk/detection.rs").
    fn source_module(&self) -> &str;

    /// Execute the check.  May return one or more results (e.g. one
    /// per missing binary).
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult>;
}

/// Registry of all known preflight checks.
///
/// To add a new check: implement `PreflightCheck` on a struct in one of
/// the sub-modules, then add a `Box::new(YourCheck)` line here.
pub fn all_checks() -> Vec<Box<dyn PreflightCheck>> {
    vec![
        // Phase 1 — Preparation
        Box::new(device::DeviceExists),
        Box::new(device::DeviceNotMounted),
        Box::new(device::DeviceSize),
        Box::new(config::ConfigValidation),
        Box::new(deps::HostBinaries),
        // Phase 2 — Partitioning & Formatting
        Box::new(layout::LayoutFitsDisk),
        Box::new(layout::PreserveHomeCompat),
        Box::new(filesystem::FsToolsAvailable),
        Box::new(filesystem::BtrfsBootCompat),
        // Phase 3 — Base System
        Box::new(packages::PackagesResolvable),
        Box::new(packages::CustomRepoAvailable),
        // Phase 4 — Configuration
        Box::new(services::ServicePackages),
        Box::new(services::BootloaderCheck),
        Box::new(config::SecureBootTools),
    ]
}
