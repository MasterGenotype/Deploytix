//! Preflight report types and table rendering.

use colored::Colorize;

/// Outcome status of a single preflight check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    fn symbol(&self) -> &'static str {
        match self {
            Self::Pass => "✓",
            Self::Warn => "⚠",
            Self::Fail => "✗",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

/// Result of a single preflight check.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// Pass / Warn / Fail
    pub status: CheckStatus,
    /// Human-readable operation name (e.g. "sfdisk /dev/nvme0n1")
    pub operation: String,
    /// Source module in the codebase (e.g. "disk/partitioning.rs")
    pub source: String,
    /// Explanation of the result
    pub detail: String,
}

/// A single preflight result formatted for GUI display.
#[derive(Debug, Clone)]
pub struct PreflightLine {
    pub status: CheckStatus,
    pub text: String,
}

/// Aggregated preflight results.
#[derive(Debug, Default)]
pub struct PreflightReport {
    pub results: Vec<CheckResult>,
}

impl PreflightReport {
    /// True when at least one check failed.
    pub fn has_failures(&self) -> bool {
        self.results.iter().any(|r| r.status == CheckStatus::Fail)
    }

    /// Count of each status category.
    fn counts(&self) -> (usize, usize, usize) {
        let mut pass = 0usize;
        let mut warn = 0usize;
        let mut fail = 0usize;
        for r in &self.results {
            match r.status {
                CheckStatus::Pass => pass += 1,
                CheckStatus::Warn => warn += 1,
                CheckStatus::Fail => fail += 1,
            }
        }
        (pass, warn, fail)
    }

    /// Convert results into flat log lines suitable for GUI display.
    pub fn to_log_lines(&self) -> Vec<PreflightLine> {
        self.results
            .iter()
            .map(|r| PreflightLine {
                status: r.status,
                text: format!(
                    "[{}] {} — {} ({})",
                    r.status.label(),
                    r.operation,
                    r.detail,
                    r.source
                ),
            })
            .collect()
    }

    /// Render the report as a colored table to stdout.
    pub fn print_table(&self) {
        // Column widths
        let w_status = 9;
        let w_op = 34;
        let w_src = 28;
        let w_detail = 38;

        let line_w = w_status + w_op + w_src + w_detail + 5; // 5 separators

        // Top border
        println!(
            "┌{}┬{}┬{}┬{}┐",
            "─".repeat(w_status),
            "─".repeat(w_op),
            "─".repeat(w_src),
            "─".repeat(w_detail)
        );

        // Header
        println!(
            "│{:^ws$}│{:^wo$}│{:^wsrc$}│{:^wd$}│",
            "Status",
            "Operation",
            "Source Module",
            "Detail",
            ws = w_status,
            wo = w_op,
            wsrc = w_src,
            wd = w_detail
        );

        // Header separator
        println!(
            "├{}┼{}┼{}┼{}┤",
            "─".repeat(w_status),
            "─".repeat(w_op),
            "─".repeat(w_src),
            "─".repeat(w_detail)
        );

        for r in &self.results {
            let status_str = format!("{} {}", r.status.symbol(), r.status.label());
            let colored_status = match r.status {
                CheckStatus::Pass => format!(" {} ", status_str.green()),
                CheckStatus::Warn => format!(" {} ", status_str.yellow()),
                CheckStatus::Fail => format!(" {} ", status_str.red()),
            };

            // Truncate fields to fit columns
            let op = truncate(&r.operation, w_op - 2);
            let src = truncate(&r.source, w_src - 2);
            let detail = truncate(&r.detail, w_detail - 2);

            // Print the colored status cell separately since ANSI codes
            // break formatting width.  Pad the plain-text fields normally.
            println!(
                "│{}│ {:<wo$}│ {:<wsrc$}│ {:<wd$}│",
                colored_status,
                op,
                src,
                detail,
                wo = w_op - 2,
                wsrc = w_src - 2,
                wd = w_detail - 2
            );
        }

        // Bottom border
        println!(
            "└{}┴{}┴{}┴{}┘",
            "─".repeat(w_status),
            "─".repeat(w_op),
            "─".repeat(w_src),
            "─".repeat(w_detail)
        );

        // Summary line
        let (pass, warn, fail) = self.counts();
        let summary = if fail > 0 {
            format!(
                "Preflight: {} passed, {} warning(s), {} {}",
                pass,
                warn,
                fail,
                "FAILURE(S)".red().bold()
            )
        } else if warn > 0 {
            format!(
                "Preflight: {} passed, {} {}",
                pass,
                warn,
                "warning(s)".yellow()
            )
        } else {
            format!("Preflight: {} {}", pass, "all passed".green().bold())
        };
        let _ = line_w; // suppress unused
        println!("{}", summary);
    }
}

/// Truncate a string to `max` chars, appending "…" when shortened.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", truncated)
    }
}
