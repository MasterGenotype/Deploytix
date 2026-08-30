//! Per-update package history for the transactional backends.
//!
//! A snapshot set is just an id and three subvolumes, and the pacman database
//! lives on `/var`, which neither backend snapshots (see the subvolume table in
//! [`crate::immutable`] and the shared-state notes in
//! [`crate::immutable::lvm_ab`]). So nothing on disk records *what a given
//! update changed* — the DB only ever describes the newest transaction.
//!
//! This module adds that record. Each `deploytix update` brackets its pacman
//! run with two `pacman -Q` reads inside the target chroot and stores the diff
//! as JSON under [`HISTORY_DIR`], keyed by the set id (btrfs) or slot letter
//! (LVM A/B). Both the CLI and the GUI updater go through the same code path,
//! so a terminal update shows up in the GUI and vice versa.
//!
//! Recording is strictly best-effort: a failure here must never fail an update
//! that otherwise succeeded, so every entry point swallows its errors after
//! logging them.
//!
//! ## The rollback caveat this makes visible
//! Because the pacman DB is shared, rolling back restores a set's `/usr` but
//! *not* the DB — afterwards pacman still describes the newer packages. The
//! stored records are what let a UI say which updates a given set predates.

use crate::utils::command::CommandRunner;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

/// Directory holding one JSON record per update, on the shared `/var`.
pub const HISTORY_DIR: &str = "/var/lib/deploytix/history";

/// Which transactional backend produced a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    /// btrfs paired snapshot sets.
    Btrfs,
    /// LVM A/B dual-slot with dm-verity.
    LvmAb,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Btrfs => "btrfs snapshot set",
            Self::LvmAb => "LVM A/B slot",
        }
    }
}

/// What the user asked for, preserved even when the transaction failed before
/// changing anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// `deploytix update` with no arguments.
    FullUpgrade,
    /// Repo package names.
    Packages(Vec<String>),
    /// Local `.pkg.tar.*` files (stored as the basenames actually installed).
    LocalFiles(Vec<String>),
    /// Both in one transaction.
    Mixed {
        packages: Vec<String>,
        files: Vec<String>,
    },
}

impl Request {
    /// Build a request from the split produced by
    /// [`crate::immutable::update::classify_args`].
    pub fn classify(packages: &[String], files: &[String]) -> Self {
        let files: Vec<String> = files.iter().map(|f| basename(f)).collect();
        match (packages.is_empty(), files.is_empty()) {
            (true, true) => Self::FullUpgrade,
            (false, true) => Self::Packages(packages.to_vec()),
            (true, false) => Self::LocalFiles(files),
            (false, false) => Self::Mixed {
                packages: packages.to_vec(),
                files,
            },
        }
    }

    /// One-line description for a list row.
    pub fn summary(&self) -> String {
        match self {
            Self::FullUpgrade => "Full system upgrade".to_string(),
            Self::Packages(p) => format!("Install: {}", p.join(", ")),
            Self::LocalFiles(f) => format!("Local package: {}", f.join(", ")),
            Self::Mixed { packages, files } => {
                format!(
                    "Install: {} + local: {}",
                    packages.join(", "),
                    files.join(", ")
                )
            }
        }
    }
}

/// Whether the transaction completed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Succeeded,
    Failed(String),
}

impl Outcome {
    pub fn succeeded(&self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

/// A package at a version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PkgVersion {
    pub name: String,
    pub version: String,
}

/// A package that changed version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PkgUpgrade {
    pub name: String,
    pub from: String,
    pub to: String,
}

/// What a transaction did to the package set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageChanges {
    pub added: Vec<PkgVersion>,
    pub removed: Vec<PkgVersion>,
    pub upgraded: Vec<PkgUpgrade>,
}

impl PackageChanges {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.upgraded.is_empty()
    }

    /// Compact `+2 ~15 -1` style summary; empty string when nothing changed.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut parts = Vec::new();
        if !self.added.is_empty() {
            parts.push(format!("+{}", self.added.len()));
        }
        if !self.upgraded.is_empty() {
            parts.push(format!("~{}", self.upgraded.len()));
        }
        if !self.removed.is_empty() {
            parts.push(format!("-{}", self.removed.len()));
        }
        parts.join(" ")
    }
}

/// One recorded update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRecord {
    /// Unix seconds when the update started; also the filename prefix and the
    /// sort key.
    pub started_at: u64,
    pub duration_secs: u64,
    pub backend: Backend,
    /// btrfs set id, or `"A"`/`"B"` for a slot.
    pub target: String,
    pub request: Request,
    pub outcome: Outcome,
    /// Empty when the transaction never got as far as changing packages.
    #[serde(default)]
    pub changes: PackageChanges,
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Final path component of `p`, or `p` itself when it has none.
fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(p)
        .to_string()
}

// ── Package queries ────────────────────────────────────────────────────────

/// Parse `pacman -Q` output (`name version` per line) into a name→version map.
///
/// Tolerates blank lines, padding and trailing fields; anything without at
/// least two whitespace-separated fields is skipped.
pub fn parse_package_list(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?;
            let version = fields.next()?;
            Some((name.to_string(), version.to_string()))
        })
        .collect()
}

/// Query the installed packages inside `chroot`.
///
/// Returns an empty map on failure rather than erroring: a missing history
/// entry is never worth failing an update over.
pub fn query_packages(cmd: &CommandRunner, chroot: &str) -> BTreeMap<String, String> {
    if cmd.is_dry_run() {
        return BTreeMap::new();
    }
    match cmd.run_in_chroot(chroot, "pacman -Q") {
        Ok(Some(out)) => parse_package_list(&String::from_utf8_lossy(&out.stdout)),
        Ok(None) => BTreeMap::new(),
        Err(e) => {
            warn!("[history] Could not query packages in {}: {}", chroot, e);
            BTreeMap::new()
        }
    }
}

/// Diff two package maps into added / removed / upgraded.
///
/// A name in both with the same version is unchanged and contributes nothing.
pub fn diff(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> PackageChanges {
    let mut changes = PackageChanges::default();

    for (name, version) in after {
        match before.get(name) {
            None => changes.added.push(PkgVersion {
                name: name.clone(),
                version: version.clone(),
            }),
            Some(old) if old != version => changes.upgraded.push(PkgUpgrade {
                name: name.clone(),
                from: old.clone(),
                to: version.clone(),
            }),
            Some(_) => {}
        }
    }
    for (name, version) in before {
        if !after.contains_key(name) {
            changes.removed.push(PkgVersion {
                name: name.clone(),
                version: version.clone(),
            });
        }
    }

    changes
}

// ── On-disk store ──────────────────────────────────────────────────────────

/// Filename for a record. Targets are sanitised so a slot letter or set id can
/// never escape the history directory.
fn record_filename(started_at: u64, target: &str) -> String {
    let safe: String = target
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{started_at}-{safe}.json")
}

/// Write `record` into `dir`, creating it if needed.
pub fn write_record_in(dir: &Path, record: &UpdateRecord) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(record_filename(record.started_at, &record.target));
    let json = serde_json::to_string_pretty(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// Write `record` to [`HISTORY_DIR`], best-effort.
///
/// Never returns an error: history is a convenience, and an update that
/// succeeded must not be reported as failed because a log line could not be
/// written.
pub fn write_record(record: &UpdateRecord) {
    match write_record_in(Path::new(HISTORY_DIR), record) {
        Ok(path) => {
            tracing::info!("[history] Recorded update at {}", path.display());
        }
        Err(e) => {
            warn!("[history] Could not write update record: {}", e);
        }
    }
}

/// Read every record in `dir`, newest first.
///
/// Unreadable or malformed files are skipped with a warning — a single corrupt
/// record must not hide the rest of the history.
pub fn list_records_in(dir: &Path) -> Vec<UpdateRecord> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut records: Vec<UpdateRecord> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .filter_map(|p| match std::fs::read_to_string(&p) {
            Ok(text) => match serde_json::from_str::<UpdateRecord>(&text) {
                Ok(r) => Some(r),
                Err(e) => {
                    warn!("[history] Skipping malformed record {}: {}", p.display(), e);
                    None
                }
            },
            Err(e) => {
                warn!(
                    "[history] Skipping unreadable record {}: {}",
                    p.display(),
                    e
                );
                None
            }
        })
        .collect();
    records.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    records
}

/// Read every record from [`HISTORY_DIR`], newest first.
pub fn list_records() -> Vec<UpdateRecord> {
    list_records_in(Path::new(HISTORY_DIR))
}

/// Index records by target, keeping the newest for each.
///
/// A/B slots are reused across updates, so a slot maps to its most recent
/// build; btrfs set ids are unique and map one-to-one.
pub fn records_by_target(records: &[UpdateRecord]) -> HashMap<String, UpdateRecord> {
    let mut map: HashMap<String, UpdateRecord> = HashMap::new();
    for r in records {
        map.entry(r.target.clone())
            .and_modify(|existing| {
                if r.started_at > existing.started_at {
                    *existing = r.clone();
                }
            })
            .or_insert_with(|| r.clone());
    }
    map
}

// ── Display helpers ────────────────────────────────────────────────────────

/// Format unix seconds as `YYYY-MM-DD HH:MM` (UTC).
///
/// Hand-rolled rather than pulling in a date crate for one label: this is
/// Howard Hinnant's civil-from-days algorithm, valid across the range any
/// filesystem timestamp can hold.
pub fn format_timestamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, minute) = (rem / 3600, (rem % 3600) / 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02} {hour:02}:{minute:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    /// A scratch directory that cleans itself up; `tempfile` is only a
    /// transitive dependency, so this uses `uuid`, which is a direct one.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("deploytix-hist-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn record(started_at: u64, target: &str) -> UpdateRecord {
        UpdateRecord {
            started_at,
            duration_secs: 12,
            backend: Backend::Btrfs,
            target: target.to_string(),
            request: Request::FullUpgrade,
            outcome: Outcome::Succeeded,
            changes: PackageChanges::default(),
        }
    }

    // ── parsing ──

    #[test]
    fn parses_pacman_q_output() {
        let text = "linux 6.12.1-1\nvim 9.1.0-2\n";
        let pkgs = parse_package_list(text);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs["linux"], "6.12.1-1");
        assert_eq!(pkgs["vim"], "9.1.0-2");
    }

    #[test]
    fn parsing_skips_blank_and_malformed_lines() {
        // Blank lines, a name with no version, and padding all appear in real
        // output when a locale or a warning leaks into stdout.
        let text = "\n  linux   6.12.1-1  \n\nbroken\n\nvim 9.1.0-2\n   \n";
        let pkgs = parse_package_list(text);
        assert_eq!(pkgs.len(), 2);
        assert_eq!(pkgs["linux"], "6.12.1-1");
        assert!(!pkgs.contains_key("broken"));
    }

    // ── diff ──

    #[test]
    fn diff_reports_added_removed_and_upgraded() {
        let before = map(&[("keep", "1"), ("bump", "1.0"), ("gone", "3")]);
        let after = map(&[("keep", "1"), ("bump", "2.0"), ("new", "0.1")]);
        let d = diff(&before, &after);

        assert_eq!(
            d.added,
            vec![PkgVersion {
                name: "new".into(),
                version: "0.1".into()
            }]
        );
        assert_eq!(
            d.removed,
            vec![PkgVersion {
                name: "gone".into(),
                version: "3".into()
            }]
        );
        assert_eq!(
            d.upgraded,
            vec![PkgUpgrade {
                name: "bump".into(),
                from: "1.0".into(),
                to: "2.0".into()
            }]
        );
    }

    #[test]
    fn identical_package_sets_produce_no_changes() {
        let m = map(&[("a", "1"), ("b", "2")]);
        assert!(diff(&m, &m).is_empty());
        assert_eq!(diff(&m, &m).summary(), "");
    }

    #[test]
    fn empty_before_makes_everything_an_addition() {
        // The state when a query failed: treat it as "nothing known before"
        // rather than inventing upgrades.
        let after = map(&[("a", "1"), ("b", "2")]);
        let d = diff(&BTreeMap::new(), &after);
        assert_eq!(d.added.len(), 2);
        assert!(d.removed.is_empty() && d.upgraded.is_empty());
    }

    #[test]
    fn changes_summary_is_compact_and_ordered() {
        let before = map(&[("bump", "1"), ("gone", "1")]);
        let after = map(&[("bump", "2"), ("new", "1")]);
        assert_eq!(diff(&before, &after).summary(), "+1 ~1 -1");
    }

    // ── request classification ──

    #[test]
    fn request_classification_covers_each_shape() {
        let none: Vec<String> = vec![];
        assert_eq!(Request::classify(&none, &none), Request::FullUpgrade);
        assert_eq!(
            Request::classify(&["vim".into()], &none),
            Request::Packages(vec!["vim".into()])
        );
        // Files are stored as basenames — the staging path is an implementation
        // detail the user never typed.
        assert_eq!(
            Request::classify(&none, &["/var/cache/deploytix-update/a.pkg.tar.zst".into()]),
            Request::LocalFiles(vec!["a.pkg.tar.zst".into()])
        );
        assert_eq!(
            Request::classify(&["vim".into()], &["/tmp/b.pkg.tar.zst".into()]),
            Request::Mixed {
                packages: vec!["vim".into()],
                files: vec!["b.pkg.tar.zst".into()]
            }
        );
    }

    // ── store ──

    #[test]
    fn record_round_trips_through_json() {
        let mut r = record(1_700_000_000, "42");
        r.request = Request::Packages(vec!["vim".into()]);
        r.outcome = Outcome::Failed("target not found".into());
        r.changes.added.push(PkgVersion {
            name: "vim".into(),
            version: "9.1".into(),
        });

        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<UpdateRecord>(&json).unwrap(), r);
    }

    #[test]
    fn records_are_listed_newest_first() {
        let dir = TempDir::new();
        for at in [100u64, 300, 200] {
            write_record_in(dir.path(), &record(at, &at.to_string())).unwrap();
        }
        let listed = list_records_in(dir.path());
        assert_eq!(
            listed.iter().map(|r| r.started_at).collect::<Vec<_>>(),
            vec![300, 200, 100]
        );
    }

    #[test]
    fn a_malformed_record_is_skipped_not_fatal() {
        let dir = TempDir::new();
        write_record_in(dir.path(), &record(100, "a")).unwrap();
        std::fs::write(dir.path().join("999-b.json"), "{ this is not json").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();

        let listed = list_records_in(dir.path());
        assert_eq!(listed.len(), 1, "the good record must still be readable");
        assert_eq!(listed[0].started_at, 100);
    }

    #[test]
    fn a_missing_history_dir_lists_empty_rather_than_erroring() {
        assert!(list_records_in(Path::new("/nonexistent/deploytix/history")).is_empty());
    }

    #[test]
    fn target_names_cannot_escape_the_history_dir() {
        let name = record_filename(100, "../../etc/passwd");
        assert!(!name.contains('/'), "got {name}");
        assert!(!name.contains(".."), "got {name}");
    }

    #[test]
    fn records_by_target_keeps_the_newest_per_slot() {
        // A/B slots are rebuilt repeatedly, so the map must hold the latest
        // build of each rather than whichever was read first.
        let records = vec![record(100, "A"), record(300, "A"), record(200, "B")];
        let by_target = records_by_target(&records);
        assert_eq!(by_target["A"].started_at, 300);
        assert_eq!(by_target["B"].started_at, 200);
    }

    // ── display ──

    #[test]
    fn timestamps_format_as_utc_date_and_time() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00");
        assert_eq!(format_timestamp(1_700_000_000), "2023-11-14 22:13");
        // A leap day, which the civil-from-days shift is easy to get wrong on.
        assert_eq!(format_timestamp(1_709_164_800), "2024-02-29 00:00");
    }
}
