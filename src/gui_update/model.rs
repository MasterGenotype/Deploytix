//! Data model for the update GUI — everything that is not egui.
//!
//! The panels stay thin on purpose: all the joining and formatting lives here
//! as pure functions so it can be tested without a display, a root shell or an
//! immutable filesystem.

use crate::immutable::history::{self, UpdateRecord};
use crate::utils::command::OperationRecord;
use std::collections::HashMap;

/// Which transactional backend the running system uses.
///
/// Detection mirrors the dispatch in `main.rs`: the LVM A/B slot-state file on
/// `/boot`, else the btrfs pairing marker at `/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Btrfs,
    LvmAb,
}

impl Backend {
    /// Detect the backend, or `None` on a system that is not immutable at all.
    pub fn detect() -> Option<Self> {
        Self::detect_at("/boot/deploytix-slots.conf", "/.deploytix-pair")
    }

    /// Testable core of [`detect`](Self::detect).
    pub fn detect_at(slots_state: &str, pair_marker: &str) -> Option<Self> {
        if std::path::Path::new(slots_state).exists() {
            Some(Self::LvmAb)
        } else if std::path::Path::new(pair_marker).exists() {
            Some(Self::Btrfs)
        } else {
            None
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Btrfs => "btrfs snapshot sets",
            Self::LvmAb => "LVM A/B (dm-verity)",
        }
    }

    /// What a restore point is called on this backend, for UI labels.
    pub fn target_noun(self) -> &'static str {
        match self {
            Self::Btrfs => "snapshot",
            Self::LvmAb => "slot",
        }
    }
}

/// A restore point as shown in the Snapshots tab: the on-disk set or slot,
/// joined with whatever update produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRow {
    /// btrfs set id, `"@"` for the pristine base, or `"A"`/`"B"` for a slot.
    pub target: String,
    /// The update that built it, if one was recorded.
    pub record: Option<UpdateRecord>,
    /// Currently booted.
    pub is_running: bool,
    /// The boot pointer selects this, but the system has not rebooted into it.
    pub is_staged: bool,
    /// Newer updates exist than the one that built this. Rolling back here
    /// leaves the shared pacman DB describing packages this set does not have.
    pub has_newer_updates: bool,
}

impl SnapshotRow {
    /// Human label for the row's identity.
    pub fn title(&self) -> String {
        match self.target.as_str() {
            "@" => "Base install".to_string(),
            "A" | "B" => format!("Slot {}", self.target),
            id => match &self.record {
                Some(r) => history::format_timestamp(r.started_at),
                // A set with no record still sorts and displays by its id,
                // which is the unix second it was created.
                None => id
                    .parse::<u64>()
                    .map(history::format_timestamp)
                    .unwrap_or_else(|_| id.to_string()),
            },
        }
    }

    /// Short status badge, or empty when the row is just history.
    pub fn badge(&self) -> &'static str {
        if self.is_running {
            "running"
        } else if self.is_staged {
            "staged for next boot"
        } else {
            ""
        }
    }

    /// One-line description of what this update did.
    pub fn detail(&self) -> String {
        match &self.record {
            None => "No record — created before update history was kept".to_string(),
            Some(r) => {
                let mut s = r.request.summary();
                if let history::Outcome::Failed(ref why) = r.outcome {
                    s.push_str(&format!("  —  FAILED: {why}"));
                } else {
                    let changes = r.changes.summary();
                    if !changes.is_empty() {
                        s.push_str(&format!("  ({changes})"));
                    }
                }
                s
            }
        }
    }
}

/// Join the on-disk restore points with the recorded update history.
///
/// `targets` is the backend's list (`snapshot::list_sets` plus `"@"`, or the
/// two slot letters). Rows come back newest first so the most relevant restore
/// point is at the top.
pub fn build_snapshot_rows(
    targets: &[String],
    records: &[UpdateRecord],
    running: &str,
    pointer_target: &str,
) -> Vec<SnapshotRow> {
    let by_target: HashMap<String, UpdateRecord> = history::records_by_target(records);
    let newest_update = records.iter().map(|r| r.started_at).max().unwrap_or(0);

    let mut rows: Vec<SnapshotRow> = targets
        .iter()
        .map(|target| {
            let record = by_target.get(target).cloned();
            // A set with no record is treated as older than every recorded
            // update, which is exactly right: it predates history keeping.
            let built_at = record.as_ref().map(|r| r.started_at).unwrap_or(0);
            SnapshotRow {
                target: target.clone(),
                is_running: target == running,
                is_staged: target == pointer_target && target != running,
                has_newer_updates: built_at < newest_update,
                record,
            }
        })
        .collect();

    // Newest first. Sets are named by creation time, so the record time (or the
    // id itself) orders them; "@" is the original install and sorts last.
    rows.sort_by_key(|row| std::cmp::Reverse(sort_key(row)));
    rows
}

/// Sort key for a row: when it came into being, in unix seconds.
fn sort_key(row: &SnapshotRow) -> u64 {
    if row.target == "@" {
        return 0;
    }
    if let Some(ref r) = row.record {
        return r.started_at;
    }
    row.target.parse::<u64>().unwrap_or(0)
}

/// The btrfs set currently mounted at `/`, from a `findmnt -no FSROOT /` value.
///
/// The boot pointer says what will boot *next*; this says what is booted *now*,
/// and the two differ whenever an update is staged. FSROOT is the subvolume
/// path with a leading slash: `/@deploytix-sets/123/root` or `/@`.
pub fn running_set_from_fsroot(fsroot: &str) -> String {
    let subvol = fsroot.trim().trim_start_matches('/');
    crate::immutable::boot::pointer_set_id(subvol).unwrap_or_else(|| "@".to_string())
}

/// The A/B slot currently booted, from `/proc/cmdline`.
///
/// The `verity-ab` hook is handed `deploytix.slot=<X>`; the slot-state file
/// records what boots next, which is not the same thing after an update.
pub fn running_slot_from_cmdline(cmdline: &str) -> Option<String> {
    cmdline
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("deploytix.slot="))
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
}

/// Render a completed command into a log line for the progress view.
///
/// Commands can produce a lot of output, so the body is trimmed and only the
/// tail is kept — enough to see what pacman said without flooding the pane.
pub fn format_op(record: &OperationRecord, tail_lines: usize) -> String {
    let mut out = format!(
        "$ {}  ({}s{})",
        record.command,
        record.duration.as_secs(),
        if record.success { "" } else { ", FAILED" }
    );
    let body = if record.stderr.trim().is_empty() {
        &record.stdout
    } else {
        &record.stderr
    };
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(tail_lines);
    for line in &lines[start..] {
        out.push('\n');
        out.push_str("    ");
        out.push_str(line);
    }
    out
}

/// Split a whitespace-separated package entry into names.
pub fn parse_package_names(input: &str) -> Vec<String> {
    input.split_whitespace().map(str::to_string).collect()
}

/// Whether a filename looks like a pacman package the updater can install.
pub fn is_package_file(name: &str) -> bool {
    // `pacman -U` accepts any compression; `.sig` files sit beside them in a
    // cache directory and must not be offered.
    name.contains(".pkg.tar.") && !name.ends_with(".sig")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::immutable::history::{Backend as HBackend, Outcome, PackageChanges, Request};
    use std::time::Duration;

    fn record(started_at: u64, target: &str) -> UpdateRecord {
        UpdateRecord {
            started_at,
            duration_secs: 5,
            backend: HBackend::Btrfs,
            target: target.to_string(),
            request: Request::Packages(vec!["vim".into()]),
            outcome: Outcome::Succeeded,
            changes: PackageChanges::default(),
        }
    }

    // ── backend detection ──

    #[test]
    fn detection_prefers_the_slot_state_file() {
        let dir = std::env::temp_dir().join(format!("dtx-det-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let slots = dir.join("slots.conf");
        let pair = dir.join("pair");
        std::fs::write(&slots, "").unwrap();
        std::fs::write(&pair, "").unwrap();

        // Both present: A/B wins, matching the dispatch order in main.rs.
        assert_eq!(
            Backend::detect_at(slots.to_str().unwrap(), pair.to_str().unwrap()),
            Some(Backend::LvmAb)
        );
        std::fs::remove_file(&slots).unwrap();
        assert_eq!(
            Backend::detect_at(slots.to_str().unwrap(), pair.to_str().unwrap()),
            Some(Backend::Btrfs)
        );
        std::fs::remove_file(&pair).unwrap();
        assert_eq!(
            Backend::detect_at(slots.to_str().unwrap(), pair.to_str().unwrap()),
            None,
            "a mutable system must report no backend"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── the join ──

    #[test]
    fn rows_join_sets_to_their_records_newest_first() {
        let targets = vec!["@".into(), "100".into(), "300".into()];
        let records = vec![record(100, "100"), record(300, "300")];
        let rows = build_snapshot_rows(&targets, &records, "300", "300");

        assert_eq!(
            rows.iter().map(|r| r.target.as_str()).collect::<Vec<_>>(),
            vec!["300", "100", "@"],
            "newest first, with the base install last"
        );
        assert!(rows[0].record.is_some());
        assert!(rows[0].is_running);
        assert!(!rows[0].is_staged);
    }

    #[test]
    fn a_set_with_no_record_is_shown_rather_than_hidden() {
        // Sets created before history keeping must still be selectable.
        let targets = vec!["100".into(), "200".into()];
        let rows = build_snapshot_rows(&targets, &[record(200, "200")], "200", "200");

        let orphan = rows.iter().find(|r| r.target == "100").unwrap();
        assert!(orphan.record.is_none());
        assert!(orphan.detail().contains("No record"));
        // Its id is a unix second, so it can still be dated for the user.
        assert_eq!(orphan.title(), history::format_timestamp(100));
    }

    #[test]
    fn a_record_whose_set_is_gone_produces_no_row() {
        // Pruning deletes sets but leaves their history entries behind.
        let rows = build_snapshot_rows(
            &["200".into()],
            &[record(100, "100"), record(200, "200")],
            "200",
            "200",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target, "200");
    }

    #[test]
    fn staged_is_distinct_from_running() {
        // After an update the pointer moves but the system has not rebooted.
        let targets = vec!["100".into(), "200".into()];
        let rows = build_snapshot_rows(
            &targets,
            &[record(100, "100"), record(200, "200")],
            "100",
            "200",
        );

        let staged = rows.iter().find(|r| r.target == "200").unwrap();
        let running = rows.iter().find(|r| r.target == "100").unwrap();
        assert!(staged.is_staged && !staged.is_running);
        assert!(running.is_running && !running.is_staged);
        assert_eq!(staged.badge(), "staged for next boot");
        assert_eq!(running.badge(), "running");
    }

    /// The documented rollback caveat: `/var` is shared, so rolling back to an
    /// older set leaves the pacman DB describing newer packages. Rows have to
    /// carry that so the UI can warn before the user commits.
    #[test]
    fn older_sets_are_flagged_as_predating_newer_updates() {
        let targets = vec!["100".into(), "200".into()];
        let rows = build_snapshot_rows(
            &targets,
            &[record(100, "100"), record(200, "200")],
            "200",
            "200",
        );

        assert!(
            rows.iter()
                .find(|r| r.target == "100")
                .unwrap()
                .has_newer_updates
        );
        assert!(
            !rows
                .iter()
                .find(|r| r.target == "200")
                .unwrap()
                .has_newer_updates
        );
    }

    #[test]
    fn slot_rows_are_labelled_by_letter() {
        let rows = build_snapshot_rows(&["A".into(), "B".into()], &[record(300, "B")], "A", "A");
        assert_eq!(rows[0].title(), "Slot B");
        assert!(rows.iter().any(|r| r.title() == "Slot A"));
    }

    // ── detail rendering ──

    #[test]
    fn detail_shows_the_change_summary_for_a_successful_update() {
        let mut r = record(100, "100");
        r.changes.added.push(history::PkgVersion {
            name: "vim".into(),
            version: "9.1".into(),
        });
        let rows = build_snapshot_rows(&["100".into()], &[r], "100", "100");
        let detail = rows[0].detail();
        assert!(detail.contains("Install: vim"), "got {detail}");
        assert!(detail.contains("+1"), "got {detail}");
    }

    #[test]
    fn detail_surfaces_the_reason_a_failed_update_failed() {
        let mut r = record(100, "100");
        r.outcome = Outcome::Failed("target not found: nosuchpkg".into());
        let rows = build_snapshot_rows(&["100".into()], &[r], "100", "100");
        assert!(rows[0].detail().contains("FAILED: target not found"));
    }

    // ── misc helpers ──

    #[test]
    fn command_output_is_tailed_not_dumped() {
        let op = OperationRecord {
            command: "pacman -Syu".into(),
            stdout: (1..=50).map(|i| format!("line {i}\n")).collect(),
            stderr: String::new(),
            exit_code: 0,
            duration: Duration::from_secs(7),
            success: true,
        };
        let text = format_op(&op, 3);
        assert!(text.starts_with("$ pacman -Syu  (7s)"));
        assert!(text.contains("line 50"));
        assert!(!text.contains("line 46"), "only the tail should survive");
    }

    #[test]
    fn a_failed_command_is_marked_and_shows_stderr() {
        let op = OperationRecord {
            command: "pacman -U bad".into(),
            stdout: "ignored".into(),
            stderr: "error: could not open file".into(),
            exit_code: 1,
            duration: Duration::from_secs(1),
            success: false,
        };
        let text = format_op(&op, 5);
        assert!(text.contains("FAILED"));
        assert!(text.contains("could not open file"));
        assert!(!text.contains("ignored"), "stderr wins when present");
    }

    #[test]
    fn running_set_is_read_from_the_mounted_subvolume() {
        assert_eq!(running_set_from_fsroot("/@deploytix-sets/123/root"), "123");
        // The pristine base install.
        assert_eq!(running_set_from_fsroot("/@"), "@");
        // findmnt output carries a trailing newline.
        assert_eq!(running_set_from_fsroot("/@deploytix-sets/9/root\n"), "9");
    }

    #[test]
    fn running_slot_is_read_from_the_kernel_cmdline() {
        let cmdline =
            "BOOT_IMAGE=/vmlinuz root=/dev/mapper/x deploytix.slot=b deploytix.roothash=ab12 rw";
        assert_eq!(running_slot_from_cmdline(cmdline), Some("B".to_string()));
        assert_eq!(running_slot_from_cmdline("root=/dev/sda1 rw"), None);
        // A truncated parameter must not yield an empty slot name.
        assert_eq!(running_slot_from_cmdline("deploytix.slot="), None);
    }

    #[test]
    fn package_entry_splits_on_any_whitespace() {
        assert_eq!(
            parse_package_names("  vim   git\nneovim\t"),
            vec!["vim", "git", "neovim"]
        );
        assert!(parse_package_names("   ").is_empty());
    }

    #[test]
    fn package_files_are_recognised_across_compressions_but_not_signatures() {
        assert!(is_package_file("vim-9.1-1-x86_64.pkg.tar.zst"));
        assert!(is_package_file("vim-9.1-1-x86_64.pkg.tar.xz"));
        // Signatures sit beside packages in a cache dir and are not installable.
        assert!(!is_package_file("vim-9.1-1-x86_64.pkg.tar.zst.sig"));
        assert!(!is_package_file("notes.txt"));
    }
}
