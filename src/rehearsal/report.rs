//! Rehearsal report types and rendering.
//!
//! Provides three output modes:
//! - `print_table()` — colored terminal table grouped by operation
//! - `to_log_lines()` — structured lines for GUI consumption
//! - `write_to_file()` — full detail log suitable for issue reports

use crate::utils::command::OperationRecord;
use colored::Colorize;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

/// A single line for GUI display.
#[derive(Debug, Clone)]
pub struct RehearsalLogLine {
    pub success: bool,
    pub text: String,
}

/// Aggregated results of a rehearsal installation run.
pub struct RehearsalReport {
    /// Every command invocation recorded during the rehearsal.
    pub records: Vec<OperationRecord>,
    /// If the installer short-circuited, the error description.
    pub short_circuited_at: Option<String>,
    /// Whether the disk was successfully wiped after the rehearsal.
    pub disk_wiped: bool,
    /// Wall-clock duration of the entire rehearsal.
    pub total_duration: Duration,
}

impl RehearsalReport {
    /// Count of successful operations.
    pub fn pass_count(&self) -> usize {
        self.records.iter().filter(|r| r.success).count()
    }

    /// Count of failed operations.
    pub fn fail_count(&self) -> usize {
        self.records.iter().filter(|r| !r.success).count()
    }

    /// True if any operation failed or the installer short-circuited.
    pub fn has_failures(&self) -> bool {
        self.fail_count() > 0 || self.short_circuited_at.is_some()
    }

    // ── CLI table ──────────────────────────────────────────────────

    /// Print a colored table to stdout.
    #[allow(clippy::print_literal)]
    pub fn print_table(&self) {
        let top    = "┌─────────┬────────────┬──────────────────────────────────────────────────────────┐";
        let mid    = "├─────────┼────────────┼──────────────────────────────────────────────────────────┤";
        let bottom = "└─────────┴────────────┴──────────────────────────────────────────────────────────┘";

        println!();
        println!("{}", top);
        println!(
            "│ {} │ {} │ {} │",
            "Status ".bold(),
            "Duration  ".bold(),
            format!("{:<56}", "Command").bold()
        );
        println!("{}", mid);

        for rec in &self.records {
            let status = if rec.success {
                "✓ PASS ".green().to_string()
            } else {
                "✗ FAIL ".red().to_string()
            };
            let dur = format_duration(rec.duration);
            let cmd = truncate(&rec.command, 56);
            println!("│ {} │ {:>10} │ {:<56} │", status, dur, cmd);
        }

        println!("{}", bottom);

        // Summary
        let total = self.records.len();
        let passed = self.pass_count();
        let failed = self.fail_count();

        let summary = format!(
            "Rehearsal: {} operations — {} passed, {} failed ({})",
            total,
            passed,
            failed,
            format_duration(self.total_duration),
        );

        if failed > 0 {
            println!("{}", summary.red().bold());
        } else {
            println!("{}", summary.green().bold());
        }

        if let Some(ref err) = self.short_circuited_at {
            println!(
                "{}",
                format!("Short-circuited at: {}", err).yellow().bold()
            );
        }

        if self.disk_wiped {
            println!("{}", "Disk wiped: ✓ (restored to pristine state)".dimmed());
        } else {
            println!(
                "{}",
                "Disk wiped: ✗ (WARNING: disk may be in partial state)"
                    .red()
                    .bold()
            );
        }
        println!();
    }

    // ── GUI lines ──────────────────────────────────────────────────

    /// Convert the report into lines suitable for GUI display.
    pub fn to_log_lines(&self) -> Vec<RehearsalLogLine> {
        let mut lines = Vec::with_capacity(self.records.len() + 4);

        for rec in &self.records {
            let prefix = if rec.success { "✓" } else { "✗" };
            let dur = format_duration(rec.duration);
            lines.push(RehearsalLogLine {
                success: rec.success,
                text: format!("{} [{}] {}", prefix, dur, rec.command),
            });

            // Show stderr for failed ops
            if !rec.success && !rec.stderr.is_empty() {
                let err_preview = rec.stderr.lines().next().unwrap_or("").trim();
                let err_text = truncate(err_preview, 80);
                lines.push(RehearsalLogLine {
                    success: false,
                    text: format!("    └─ {}", err_text),
                });
            }
        }

        // Summary line
        let summary = format!(
            "Total: {} ops, {} passed, {} failed ({})",
            self.records.len(),
            self.pass_count(),
            self.fail_count(),
            format_duration(self.total_duration),
        );
        lines.push(RehearsalLogLine {
            success: !self.has_failures(),
            text: summary,
        });

        if let Some(ref err) = self.short_circuited_at {
            lines.push(RehearsalLogLine {
                success: false,
                text: format!("Short-circuited at: {}", err),
            });
        }

        lines.push(RehearsalLogLine {
            success: self.disk_wiped,
            text: if self.disk_wiped {
                "Disk wiped: restored to pristine state".to_string()
            } else {
                "WARNING: disk wipe failed — disk may be in partial state".to_string()
            },
        });

        lines
    }

    // ── File output ────────────────────────────────────────────────

    /// Write a detailed log file containing every command, its output, and
    /// timing information.
    pub fn write_to_file(&self, path: &Path) -> std::io::Result<()> {
        let mut f = std::fs::File::create(path)?;

        writeln!(f, "# Deploytix Rehearsal Report")?;
        writeln!(
            f,
            "# Total duration: {}",
            format_duration(self.total_duration)
        )?;
        writeln!(
            f,
            "# Operations: {} total, {} passed, {} failed",
            self.records.len(),
            self.pass_count(),
            self.fail_count()
        )?;
        if let Some(ref err) = self.short_circuited_at {
            writeln!(f, "# Short-circuited at: {}", err)?;
        }
        writeln!(f, "# Disk wiped: {}", self.disk_wiped)?;
        writeln!(f)?;

        for (i, rec) in self.records.iter().enumerate() {
            let status = if rec.success { "PASS" } else { "FAIL" };
            writeln!(
                f,
                "── Operation {}/{} [{}] ({}) ──",
                i + 1,
                self.records.len(),
                status,
                format_duration(rec.duration)
            )?;
            writeln!(f, "Command: {}", rec.command)?;
            writeln!(f, "Exit code: {}", rec.exit_code)?;

            if !rec.stdout.is_empty() {
                writeln!(f, "--- stdout ---")?;
                writeln!(f, "{}", rec.stdout.trim())?;
            }
            if !rec.stderr.is_empty() {
                writeln!(f, "--- stderr ---")?;
                writeln!(f, "{}", rec.stderr.trim())?;
            }
            writeln!(f)?;
        }

        Ok(())
    }
}

// ── helpers ────────────────────────────────────────────────────────────

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else if secs > 0 {
        format!("{}.{:01}s", secs, d.subsec_millis() / 100)
    } else {
        format!("{}ms", d.as_millis())
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}
