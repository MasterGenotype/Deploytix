//! Service and bootloader preflight checks.
//!
//! Derives the service list from the config via the same
//! `build_service_list()` / `build_service_packages()` logic the
//! installer uses.

use super::{Phase, PreflightCheck, PreflightContext};
use crate::config::{DesktopEnvironment, NetworkBackend};
use crate::preflight::report::{CheckResult, CheckStatus};
use crate::utils::command::command_exists;

// ── ServicePackages ───────────────────────────────────────────────────

pub struct ServicePackages;

/// Build the expected service list from config (mirrors
/// configure::services::build_service_list).
fn expected_services(ctx: &PreflightContext) -> Vec<String> {
    let config = ctx.config;
    let mut services = Vec::new();

    if config.desktop.environment != DesktopEnvironment::None {
        services.push("seatd".to_string());
    }

    match config.network.backend {
        NetworkBackend::Iwd => services.push("iwd".to_string()),
        NetworkBackend::NetworkManager => {
            services.push("NetworkManager".to_string());
            services.push("iwd".to_string());
        }
    }

    if config.desktop.environment != DesktopEnvironment::None {
        services.push("greetd".to_string());
    }

    services
}

/// Map service name to its base package (mirrors
/// configure::services::service_base_package).
fn service_base_package(service: &str) -> &str {
    match service {
        "NetworkManager" => "networkmanager",
        other => other,
    }
}

impl PreflightCheck for ServicePackages {
    fn name(&self) -> &str {
        "services.packages"
    }
    fn phase(&self) -> Phase {
        Phase::Configure
    }
    fn source_module(&self) -> &str {
        "configure/services.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        let services = expected_services(ctx);
        if services.is_empty() {
            return vec![CheckResult {
                status: CheckStatus::Pass,
                operation: "Service packages".to_string(),
                source: self.source_module().to_string(),
                detail: "No services to enable".to_string(),
            }];
        }

        let init = &ctx.config.system.init;
        let mut results = Vec::new();

        // Build expected init-specific package names
        let mut expected_pkgs: Vec<String> = Vec::new();
        for svc in &services {
            let base = service_base_package(svc);
            expected_pkgs.push(base.to_string());
            // greetd-s6 doesn't exist; elogind-<init> is blacklisted
            if base == "greetd" && *init == crate::config::InitSystem::S6 {
                continue;
            }
            if base == "elogind" {
                continue;
            }
            expected_pkgs.push(format!("{}-{}", base, init));
        }

        // Check if these packages are resolvable via pacman -Si
        // (read-only query against sync DB)
        let mut missing = Vec::new();
        for pkg in &expected_pkgs {
            let output = std::process::Command::new("pacman")
                .args(["-Si", pkg])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            let found = output.map(|s| s.success()).unwrap_or(false);
            if !found {
                missing.push(pkg.clone());
            }
        }

        if missing.is_empty() {
            results.push(CheckResult {
                status: CheckStatus::Pass,
                operation: format!("{} service packages", expected_pkgs.len()),
                source: self.source_module().to_string(),
                detail: "All service packages resolvable in sync DB".to_string(),
            });
        } else {
            for pkg in &missing {
                results.push(CheckResult {
                    status: CheckStatus::Warn,
                    operation: format!("Service pkg: {}", pkg),
                    source: self.source_module().to_string(),
                    detail: "Not in sync DB (will be installed in chroot)".to_string(),
                });
            }
        }

        results
    }
}

// ── BootloaderCheck ───────────────────────────────────────────────────

pub struct BootloaderCheck;

impl PreflightCheck for BootloaderCheck {
    fn name(&self) -> &str {
        "services.bootloader"
    }
    fn phase(&self) -> Phase {
        Phase::Configure
    }
    fn source_module(&self) -> &str {
        "configure/bootloader.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        let mut results = Vec::new();

        // GRUB binaries
        let grub_bins = ["grub-install", "grub-mkconfig"];
        let missing: Vec<&&str> = grub_bins.iter().filter(|b| !command_exists(b)).collect();

        if missing.is_empty() {
            results.push(CheckResult {
                status: CheckStatus::Pass,
                operation: "GRUB binaries".to_string(),
                source: self.source_module().to_string(),
                detail: "grub-install and grub-mkconfig available".to_string(),
            });
        } else {
            for bin in &missing {
                results.push(CheckResult {
                    status: CheckStatus::Fail,
                    operation: format!("Binary: {}", bin),
                    source: self.source_module().to_string(),
                    detail: "Required for GRUB bootloader".to_string(),
                });
            }
        }

        // For encrypted systems, GRUB needs crypto modules
        if ctx.config.disk.encryption {
            results.push(CheckResult {
                status: CheckStatus::Pass,
                operation: "GRUB crypto modules".to_string(),
                source: self.source_module().to_string(),
                detail: "Encryption enabled; standalone GRUB EFI with crypto modules will be built"
                    .to_string(),
            });
        }

        results
    }
}
