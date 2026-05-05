//! Configuration validation preflight checks.
//!
//! Delegates to `DeploymentConfig::validate()` — the same validation
//! the installer runs.  Also checks SecureBoot tool availability when
//! the feature is enabled.

use super::{Phase, PreflightCheck, PreflightContext};
use crate::config::SecureBootMethod;
use crate::preflight::report::{CheckResult, CheckStatus};
use crate::utils::command::command_exists;

// ── ConfigValidation ──────────────────────────────────────────────────

pub struct ConfigValidation;

impl PreflightCheck for ConfigValidation {
    fn name(&self) -> &str {
        "config.validate"
    }
    fn phase(&self) -> Phase {
        Phase::Preparation
    }
    fn source_module(&self) -> &str {
        "config/deployment.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        match ctx.config.validate() {
            Ok(()) => vec![CheckResult {
                status: CheckStatus::Pass,
                operation: "Configuration validation".to_string(),
                source: self.source_module().to_string(),
                detail: "All config constraints satisfied".to_string(),
            }],
            Err(e) => vec![CheckResult {
                status: CheckStatus::Fail,
                operation: "Configuration validation".to_string(),
                source: self.source_module().to_string(),
                detail: format!("{}", e),
            }],
        }
    }
}

// ── SecureBootTools ───────────────────────────────────────────────────

pub struct SecureBootTools;

impl PreflightCheck for SecureBootTools {
    fn name(&self) -> &str {
        "config.secureboot"
    }
    fn phase(&self) -> Phase {
        Phase::Configure
    }
    fn source_module(&self) -> &str {
        "configure/secureboot.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        if !ctx.config.system.secureboot {
            return vec![CheckResult {
                status: CheckStatus::Pass,
                operation: "SecureBoot tools".to_string(),
                source: self.source_module().to_string(),
                detail: "SecureBoot not enabled, skipped".to_string(),
            }];
        }

        let (required_bins, method_name) = match ctx.config.system.secureboot_method {
            SecureBootMethod::Sbctl => (vec!["sbctl"], "sbctl"),
            SecureBootMethod::ManualKeys | SecureBootMethod::Shim => {
                (vec!["sbsign", "efi-readvar"], "sbsigntools+efitools")
            }
        };

        let missing: Vec<&&str> = required_bins
            .iter()
            .filter(|b| !command_exists(b))
            .collect();

        if missing.is_empty() {
            vec![CheckResult {
                status: CheckStatus::Pass,
                operation: format!("SecureBoot ({})", method_name),
                source: self.source_module().to_string(),
                detail: "Required signing tools found".to_string(),
            }]
        } else {
            let names: Vec<String> = missing.iter().map(|b| b.to_string()).collect();
            vec![CheckResult {
                status: CheckStatus::Fail,
                operation: format!("SecureBoot ({})", method_name),
                source: self.source_module().to_string(),
                detail: format!("Missing: {}", names.join(", ")),
            }]
        }
    }
}
