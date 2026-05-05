//! Filesystem-related preflight checks.
//!
//! Derives which mkfs tools are needed from `config.disk.filesystem` and
//! `config.disk.boot_filesystem` — adding a new FS variant to the enum
//! extends the check automatically.

use super::{Phase, PreflightCheck, PreflightContext};
use crate::config::Filesystem;
use crate::preflight::report::{CheckResult, CheckStatus};
use crate::utils::command::command_exists;

/// Map a filesystem enum to its mkfs binary name.
fn mkfs_binary(fs: &Filesystem) -> &'static str {
    match fs {
        Filesystem::Ext4 => "mkfs.ext4",
        Filesystem::Btrfs => "mkfs.btrfs",
        Filesystem::Xfs => "mkfs.xfs",
        Filesystem::F2fs => "mkfs.f2fs",
        Filesystem::Zfs => "zpool",
    }
}

// ── FsToolsAvailable ──────────────────────────────────────────────────

pub struct FsToolsAvailable;

impl PreflightCheck for FsToolsAvailable {
    fn name(&self) -> &str {
        "fs.tools_available"
    }
    fn phase(&self) -> Phase {
        Phase::PartitionFormat
    }
    fn source_module(&self) -> &str {
        "disk/formatting.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        let mut results = Vec::new();
        let mut checked = std::collections::HashSet::new();

        // Data filesystem
        let data_bin = mkfs_binary(&ctx.config.disk.filesystem);
        checked.insert(data_bin);

        if command_exists(data_bin) {
            results.push(CheckResult {
                status: CheckStatus::Pass,
                operation: format!("{} (data fs)", data_bin),
                source: self.source_module().to_string(),
                detail: format!("Available for {}", ctx.config.disk.filesystem),
            });
        } else {
            results.push(CheckResult {
                status: CheckStatus::Fail,
                operation: format!("{} (data fs)", data_bin),
                source: self.source_module().to_string(),
                detail: format!(
                    "Required for {} but not in PATH",
                    ctx.config.disk.filesystem
                ),
            });
        }

        // Boot filesystem (skip if same tool already checked)
        let boot_bin = mkfs_binary(&ctx.config.disk.boot_filesystem);
        if checked.insert(boot_bin) {
            if command_exists(boot_bin) {
                results.push(CheckResult {
                    status: CheckStatus::Pass,
                    operation: format!("{} (boot fs)", boot_bin),
                    source: self.source_module().to_string(),
                    detail: format!("Available for {}", ctx.config.disk.boot_filesystem),
                });
            } else {
                results.push(CheckResult {
                    status: CheckStatus::Fail,
                    operation: format!("{} (boot fs)", boot_bin),
                    source: self.source_module().to_string(),
                    detail: format!(
                        "Required for {} but not in PATH",
                        ctx.config.disk.boot_filesystem
                    ),
                });
            }
        }

        // EFI is always FAT32
        if !command_exists("mkfs.vfat") {
            results.push(CheckResult {
                status: CheckStatus::Fail,
                operation: "mkfs.vfat (EFI)".to_string(),
                source: self.source_module().to_string(),
                detail: "Required for FAT32 EFI partition".to_string(),
            });
        }

        results
    }
}

// ── BtrfsBootCompat ───────────────────────────────────────────────────

pub struct BtrfsBootCompat;

impl PreflightCheck for BtrfsBootCompat {
    fn name(&self) -> &str {
        "fs.btrfs_boot_compat"
    }
    fn phase(&self) -> Phase {
        Phase::PartitionFormat
    }
    fn source_module(&self) -> &str {
        "disk/formatting.rs"
    }
    fn run(&self, ctx: &PreflightContext) -> Vec<CheckResult> {
        if ctx.config.disk.boot_filesystem != Filesystem::Btrfs {
            return vec![CheckResult {
                status: CheckStatus::Pass,
                operation: "Btrfs boot compatibility".to_string(),
                source: self.source_module().to_string(),
                detail: "Boot filesystem is not btrfs, skipped".to_string(),
            }];
        }

        // Btrfs boot requires the @boot subvolume workflow
        // Verify the boot partition exists in the layout
        let has_boot = ctx.layout.partitions.iter().any(|p| p.is_boot_fs);

        if has_boot {
            vec![CheckResult {
                status: CheckStatus::Pass,
                operation: "Btrfs boot @boot subvolume".to_string(),
                source: self.source_module().to_string(),
                detail: "Boot partition present; @boot subvolume will be created".to_string(),
            }]
        } else {
            vec![CheckResult {
                status: CheckStatus::Fail,
                operation: "Btrfs boot @boot subvolume".to_string(),
                source: self.source_module().to_string(),
                detail: "Btrfs boot requires a dedicated boot partition".to_string(),
            }]
        }
    }
}
