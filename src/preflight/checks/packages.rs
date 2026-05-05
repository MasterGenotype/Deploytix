//! Package resolvability preflight checks.
//!
//! Exercises real pacman resolution via `pkgdeps::preflight` (which
//! creates a scratch DB directory cleaned up via RAII on drop) and
//! builds the package list from the config using
//! `basestrap::build_package_list()`.

use super::{Phase, PreflightCheck, PreflightContext};
use crate::install::build_package_list;
use crate::preflight::report::{CheckResult, CheckStatus};

// ── PackagesResolvable ────────────────────────────────────────────────

pub struct PackagesResolvable;

impl PreflightCheck for PackagesResolvable {
    fn name(&self) -> &str {
        "packages.resolvable"
    }
    fn phase(&self) -> Phase {
        Phase::BaseSystem
    }
    fn source_module(&self) -> &str {
        "install/basestrap.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        let packages = build_package_list(ctx.config);

        // Create a temporary root directory for pacman --root.
        // /install doesn't exist during preflight; pacman needs a
        // real path to resolve against.  Cleaned up when dropped.
        let temp_root = match std::fs::create_dir_all("/tmp/deploytix-preflight-root") {
            Ok(()) => "/tmp/deploytix-preflight-root",
            Err(_) => "/tmp",
        };

        // Use the existing preflight infrastructure which creates a
        // scratch pacman DB, resolves the transaction, and cleans up.
        let report = match crate::pkgdeps::preflight::preflight_host(
            None,                               // no custom pacman.conf override
            temp_root,                           // temporary root (cleaned up below)
            &packages,
            false,                               // not dry-run — actually probe
        ) {
            Ok(r) => r,
            Err(e) => {
                let _ = std::fs::remove_dir_all("/tmp/deploytix-preflight-root");
                return vec![CheckResult {
                    status: CheckStatus::Warn,
                    operation: "Package resolution".to_string(),
                    source: self.source_module().to_string(),
                    detail: format!("Preflight could not run: {}", e),
                }];
            }
        };

        let mut results = Vec::new();

        if report.skipped {
            results.push(CheckResult {
                status: CheckStatus::Warn,
                operation: "Package resolution".to_string(),
                source: self.source_module().to_string(),
                detail: "Skipped (pacman tooling unavailable or empty sync DB)".to_string(),
            });
            return results;
        }

        if report.is_resolvable() {
            results.push(CheckResult {
                status: CheckStatus::Pass,
                operation: format!("Resolve {} packages", packages.len()),
                source: self.source_module().to_string(),
                detail: format!(
                    "{} packages planned for install",
                    report.planned_install_count
                ),
            });
        } else {
            // Emit one result per unresolved target
            for target in &report.unresolved {
                results.push(CheckResult {
                    status: CheckStatus::Fail,
                    operation: format!("Package: {}", target),
                    source: self.source_module().to_string(),
                    detail: "Not found in any configured repository".to_string(),
                });
            }
        }

        // Surface warnings from the resolver
        for warn in &report.warnings {
            results.push(CheckResult {
                status: CheckStatus::Warn,
                operation: "Package resolution".to_string(),
                source: self.source_module().to_string(),
                detail: warn.clone(),
            });
        }

        // Surface conflict-driven removals
        for rm in &report.to_remove {
            results.push(CheckResult {
                status: CheckStatus::Warn,
                operation: format!("Conflict removal: {}", rm),
                source: self.source_module().to_string(),
                detail: "Would be removed due to package conflict".to_string(),
            });
        }

        // Clean up temp root
        let _ = std::fs::remove_dir_all("/tmp/deploytix-preflight-root");

        results
    }
}

// ── CustomRepoAvailable ───────────────────────────────────────────────

pub struct CustomRepoAvailable;

impl PreflightCheck for CustomRepoAvailable {
    fn name(&self) -> &str {
        "packages.custom_repo"
    }
    fn phase(&self) -> Phase {
        Phase::BaseSystem
    }
    fn source_module(&self) -> &str {
        "install/basestrap.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        let packages = build_package_list(ctx.config);

        // The custom packages that need special repo handling
        let custom_names = [
            "deploytix-git",
            "deploytix-gui-git",
            "gamescope-git",
            "tkg-gui-git",
            "modular-git",
        ];

        let needed: Vec<&&str> = custom_names
            .iter()
            .filter(|name| packages.iter().any(|p| p == **name))
            .collect();

        if needed.is_empty() {
            return vec![CheckResult {
                status: CheckStatus::Pass,
                operation: "Custom repo packages".to_string(),
                source: self.source_module().to_string(),
                detail: "No custom packages in package list".to_string(),
            }];
        }

        // Check if [deploytix] repo is configured in pacman.conf
        let pacman_conf = std::fs::read_to_string("/etc/pacman.conf").unwrap_or_default();
        let has_repo = pacman_conf.lines().any(|line| line.trim() == "[deploytix]");

        // Check if ISO-embedded repo exists
        let iso_repo = std::path::Path::new("/var/lib/deploytix-repo/deploytix.db.tar.zst");

        if has_repo {
            vec![CheckResult {
                status: CheckStatus::Pass,
                operation: "Custom [deploytix] repo".to_string(),
                source: self.source_module().to_string(),
                detail: format!(
                    "Configured in pacman.conf ({} custom packages needed)",
                    needed.len()
                ),
            }]
        } else if iso_repo.exists() {
            vec![CheckResult {
                status: CheckStatus::Pass,
                operation: "Custom [deploytix] repo".to_string(),
                source: self.source_module().to_string(),
                detail: "ISO-embedded repo found".to_string(),
            }]
        } else {
            vec![CheckResult {
                status: CheckStatus::Warn,
                operation: "Custom [deploytix] repo".to_string(),
                source: self.source_module().to_string(),
                detail: format!(
                    "{} custom package(s) needed but no [deploytix] repo configured; \
                     installer will search for pre-built .pkg.tar.zst files",
                    needed.len()
                ),
            }]
        }
    }
}
