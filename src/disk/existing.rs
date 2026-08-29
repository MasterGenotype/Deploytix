//! Reading the partition table that is already on a disk.
//!
//! Everything else in [`crate::disk`] describes a layout deploytix is about
//! to *write*. This module describes what is on the device *now*, which is
//! what a home-preserving recovery install has to reason about before it
//! touches anything.
//!
//! Parsing is split from probing so the interesting logic is testable
//! without a disk: [`parse_sfdisk_json`] is pure, and
//! [`read_partition_table`] is the thin shell that runs `sfdisk --json`
//! and enriches each entry with `blkid`.

use crate::utils::error::{DeploytixError, Result};
use serde::Deserialize;
use std::process::Command;
use tracing::debug;

/// GPT partition name deploytix writes for the home volume.
///
/// `generate_sfdisk_script` emits `name="{part.name}"`, and the layout
/// builder uppercases the mount point's last component — so any disk
/// deploytix installed identifies its own home partition.
const HOME_PARTITION_NAME: &str = "HOME";

/// One partition as it exists on disk right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingPartition {
    /// Device node, e.g. `/dev/sda6`.
    pub node: String,
    /// Partition number parsed from the node (1-based).
    pub number: u32,
    /// First LBA of the partition.
    pub start_sector: u64,
    /// Length in sectors.
    pub size_sectors: u64,
    /// GPT type GUID.
    pub type_guid: String,
    /// GPT partition UUID.
    pub part_uuid: Option<String>,
    /// GPT partition name (the label written into the table, not the
    /// filesystem label).
    pub name: Option<String>,
    /// `blkid` TYPE — `crypto_LUKS`, `btrfs`, `ext4`, … `None` when the
    /// partition holds no recognisable signature.
    pub fs_type: Option<String>,
    /// `blkid` UUID of the filesystem or LUKS container.
    pub fs_uuid: Option<String>,
}

impl ExistingPartition {
    /// Size in bytes, given the table's sector size.
    pub fn size_bytes(&self, sector_size: u64) -> u64 {
        self.size_sectors * sector_size
    }

    /// Whether this partition holds a LUKS container.
    pub fn is_luks(&self) -> bool {
        self.fs_type.as_deref() == Some("crypto_LUKS")
    }

    /// Whether the GPT name marks this as the home volume.
    pub fn is_named_home(&self) -> bool {
        self.name
            .as_deref()
            .is_some_and(|n| n.eq_ignore_ascii_case(HOME_PARTITION_NAME))
    }
}

/// A device's existing partition table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingTable {
    /// The device the table belongs to, e.g. `/dev/sda`.
    pub device: String,
    /// Table type, e.g. `gpt` or `dos`.
    pub label: String,
    /// Logical sector size in bytes.
    pub sector_size: u64,
    /// First usable LBA.
    pub first_lba: Option<u64>,
    /// Last usable LBA.
    pub last_lba: Option<u64>,
    /// Partitions in on-disk order.
    pub partitions: Vec<ExistingPartition>,
}

impl ExistingTable {
    /// Look up a partition by its 1-based number.
    pub fn by_number(&self, number: u32) -> Option<&ExistingPartition> {
        self.partitions.iter().find(|p| p.number == number)
    }
}

/// Outcome of looking for the home partition on an existing table.
///
/// Ambiguity is reported rather than resolved: picking the wrong partition
/// destroys the data a recovery install exists to preserve, so the caller
/// must ask rather than guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeMatch<'a> {
    /// Exactly one candidate.
    Found(&'a ExistingPartition),
    /// No partition looks like a home volume.
    NotFound,
    /// Several candidates — the caller must disambiguate.
    Ambiguous(Vec<&'a ExistingPartition>),
}

/// Find the home partition on an existing table.
///
/// Matches on the GPT partition name (`HOME`), which is what deploytix
/// writes for `/home`. Filesystem-level identification (matching a LUKS
/// UUID against a crypttab recovered from the old root) needs the old root
/// mounted and is deliberately not attempted here.
pub fn find_home_partition(table: &ExistingTable) -> HomeMatch<'_> {
    let candidates: Vec<&ExistingPartition> = table
        .partitions
        .iter()
        .filter(|p| p.is_named_home())
        .collect();

    match candidates.len() {
        0 => HomeMatch::NotFound,
        1 => HomeMatch::Found(candidates[0]),
        _ => HomeMatch::Ambiguous(candidates),
    }
}

// ======================== sfdisk --json parsing ========================

#[derive(Debug, Deserialize)]
struct SfdiskOutput {
    partitiontable: SfdiskTable,
}

#[derive(Debug, Deserialize)]
struct SfdiskTable {
    #[serde(default)]
    label: String,
    device: Option<String>,
    #[serde(default)]
    unit: String,
    #[serde(default)]
    sectorsize: Option<u64>,
    #[serde(default)]
    firstlba: Option<u64>,
    #[serde(default)]
    lastlba: Option<u64>,
    #[serde(default)]
    partitions: Vec<SfdiskPartition>,
}

#[derive(Debug, Deserialize)]
struct SfdiskPartition {
    node: String,
    start: u64,
    size: u64,
    #[serde(rename = "type", default)]
    type_guid: String,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// Parse `sfdisk --json <device>` output into an [`ExistingTable`].
///
/// Rejects a table reported in any unit other than sectors: every extent
/// this feature preserves is expressed in sectors, and silently
/// misinterpreting the unit would place a partition over live data.
pub fn parse_sfdisk_json(json: &str) -> Result<ExistingTable> {
    let parsed: SfdiskOutput = serde_json::from_str(json).map_err(|e| {
        DeploytixError::PartitionError(format!("cannot parse sfdisk --json output: {}", e))
    })?;
    let table = parsed.partitiontable;

    if !table.unit.is_empty() && table.unit != "sectors" {
        return Err(DeploytixError::PartitionError(format!(
            "sfdisk reported the table in '{}' rather than sectors; refusing to \
             interpret partition extents",
            table.unit
        )));
    }

    let device = table.device.clone().unwrap_or_default();

    let partitions = table
        .partitions
        .into_iter()
        .map(|p| {
            let number = partition_number(&p.node).ok_or_else(|| {
                DeploytixError::PartitionError(format!(
                    "cannot determine a partition number from node '{}'",
                    p.node
                ))
            })?;
            Ok(ExistingPartition {
                number,
                start_sector: p.start,
                size_sectors: p.size,
                type_guid: p.type_guid,
                part_uuid: p.uuid,
                name: p.name.filter(|n| !n.is_empty()),
                node: p.node,
                fs_type: None,
                fs_uuid: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(ExistingTable {
        device,
        label: table.label,
        sector_size: table.sectorsize.unwrap_or(512),
        first_lba: table.firstlba,
        last_lba: table.lastlba,
        partitions,
    })
}

/// Extract the trailing partition number from a device node.
///
/// Handles both `/dev/sda6` and `/dev/nvme0n1p6`.
fn partition_number(node: &str) -> Option<u32> {
    let digits: String = node
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

// ======================== Device probing ========================

/// Read the partition table currently on `device`.
///
/// Runs `sfdisk --json`, then fills in each partition's filesystem type and
/// UUID from `blkid`. A partition `blkid` cannot identify is reported with
/// `fs_type: None` rather than failing the read — an unformatted or
/// scrubbed partition is a legitimate thing to find.
pub fn read_partition_table(device: &str) -> Result<ExistingTable> {
    let output = Command::new("sfdisk")
        .args(["--json", device])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                DeploytixError::CommandNotFound("sfdisk".to_string())
            } else {
                DeploytixError::CommandFailed {
                    command: format!("sfdisk --json {}", device),
                    stderr: e.to_string(),
                }
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DeploytixError::PartitionError(format!(
            "sfdisk --json {} failed: {}",
            device,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        return Err(DeploytixError::PartitionError(format!(
            "{} has no partition table",
            device
        )));
    }

    let mut table = parse_sfdisk_json(&stdout)?;
    if table.device.is_empty() {
        table.device = device.to_string();
    }

    for part in &mut table.partitions {
        let (fs_type, fs_uuid) = probe_filesystem(&part.node);
        part.fs_type = fs_type;
        part.fs_uuid = fs_uuid;
    }

    Ok(table)
}

/// Probe a partition's filesystem type and UUID with `blkid`.
///
/// Returns `(None, None)` when blkid cannot identify the partition, which
/// is the normal answer for an unformatted one — this is a best-effort
/// enrichment, not a validation step.
fn probe_filesystem(node: &str) -> (Option<String>, Option<String>) {
    let Ok(output) = Command::new("blkid").args(["-o", "export", node]).output() else {
        debug!("blkid unavailable while probing {}", node);
        return (None, None);
    };
    if !output.status.success() {
        return (None, None);
    }
    parse_blkid_export(&String::from_utf8_lossy(&output.stdout))
}

/// Extract `(TYPE, UUID)` from `blkid -o export` output.
fn parse_blkid_export(text: &str) -> (Option<String>, Option<String>) {
    let mut fs_type = None;
    let mut fs_uuid = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "TYPE" => fs_type = Some(value.trim().to_string()),
            // PARTUUID also ends in "UUID"; match exactly.
            "UUID" => fs_uuid = Some(value.trim().to_string()),
            _ => {}
        }
    }
    (fs_type, fs_uuid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deploytix multi-LUKS + btrfs install: EFI, BOOT, ROOT, USR, VAR, HOME.
    const SAMPLE: &str = r#"{
       "partitiontable": {
          "label": "gpt",
          "id": "9C1E4E4B-1A2B-4C3D-8E9F-0A1B2C3D4E5F",
          "device": "/dev/sda",
          "unit": "sectors",
          "firstlba": 2048,
          "lastlba": 1000215182,
          "sectorsize": 512,
          "partitions": [
             {"node":"/dev/sda1","start":2048,"size":1048576,"type":"C12A7328-F81F-11D2-BA4B-00A0C93EC93B","uuid":"AAA","name":"EFI"},
             {"node":"/dev/sda2","start":1050624,"size":4194304,"type":"0FC63DAF-8483-4772-8E79-3D69D8477DE4","uuid":"BBB","name":"BOOT"},
             {"node":"/dev/sda3","start":5244928,"size":94371840,"type":"0FC63DAF-8483-4772-8E79-3D69D8477DE4","uuid":"CCC","name":"ROOT"},
             {"node":"/dev/sda4","start":99616768,"size":136314880,"type":"0FC63DAF-8483-4772-8E79-3D69D8477DE4","uuid":"DDD","name":"USR"},
             {"node":"/dev/sda5","start":235931648,"size":83886080,"type":"0FC63DAF-8483-4772-8E79-3D69D8477DE4","uuid":"EEE","name":"VAR"},
             {"node":"/dev/sda6","start":319817728,"size":680397422,"type":"0FC63DAF-8483-4772-8E79-3D69D8477DE4","uuid":"FFF","name":"HOME"}
          ]
       }
    }"#;

    fn sample_table() -> ExistingTable {
        parse_sfdisk_json(SAMPLE).expect("sample parses")
    }

    #[test]
    fn parses_table_metadata() {
        let t = sample_table();
        assert_eq!(t.device, "/dev/sda");
        assert_eq!(t.label, "gpt");
        assert_eq!(t.sector_size, 512);
        assert_eq!(t.first_lba, Some(2048));
        assert_eq!(t.last_lba, Some(1000215182));
        assert_eq!(t.partitions.len(), 6);
    }

    #[test]
    fn parses_partition_extents() {
        let t = sample_table();
        let home = t.by_number(6).expect("sda6 present");
        assert_eq!(home.node, "/dev/sda6");
        assert_eq!(home.start_sector, 319817728);
        assert_eq!(home.size_sectors, 680397422);
        assert_eq!(home.name.as_deref(), Some("HOME"));
        assert_eq!(home.part_uuid.as_deref(), Some("FFF"));
        assert_eq!(home.size_bytes(512), 680397422 * 512);
    }

    #[test]
    fn finds_the_home_partition_by_gpt_name() {
        let t = sample_table();
        match find_home_partition(&t) {
            HomeMatch::Found(p) => assert_eq!(p.node, "/dev/sda6"),
            other => panic!("expected a single match, got {:?}", other),
        }
    }

    #[test]
    fn reports_no_home_partition_rather_than_guessing() {
        let mut t = sample_table();
        t.partitions.retain(|p| !p.is_named_home());
        assert_eq!(find_home_partition(&t), HomeMatch::NotFound);
    }

    /// Two HOME-named partitions must never be silently resolved: the loser
    /// would be reformatted.
    #[test]
    fn reports_ambiguity_rather_than_picking_one() {
        let mut t = sample_table();
        let mut second = t.by_number(5).unwrap().clone();
        second.name = Some("home".to_string()); // case-insensitive match
        t.partitions.push(second);
        match find_home_partition(&t) {
            HomeMatch::Ambiguous(c) => assert_eq!(c.len(), 2),
            other => panic!("expected ambiguity, got {:?}", other),
        }
    }

    #[test]
    fn partition_numbers_come_from_nvme_and_sata_nodes() {
        assert_eq!(partition_number("/dev/sda6"), Some(6));
        assert_eq!(partition_number("/dev/nvme0n1p12"), Some(12));
        assert_eq!(partition_number("/dev/mmcblk0p3"), Some(3));
        assert_eq!(partition_number("/dev/sda"), None);
    }

    /// A table reported in a non-sector unit must be refused, not
    /// reinterpreted — extents drive where the installer writes.
    #[test]
    fn rejects_a_table_not_measured_in_sectors() {
        let json = SAMPLE.replace("\"unit\": \"sectors\"", "\"unit\": \"cylinders\"");
        let err = parse_sfdisk_json(&json).unwrap_err().to_string();
        assert!(err.contains("cylinders"), "unexpected error: {}", err);
    }

    #[test]
    fn rejects_output_that_is_not_an_sfdisk_table() {
        assert!(parse_sfdisk_json("{}").is_err());
        assert!(parse_sfdisk_json("not json").is_err());
    }

    #[test]
    fn blkid_export_yields_type_and_fs_uuid_not_partuuid() {
        let export = "DEVNAME=/dev/sda6\n\
                      UUID=1111-2222\n\
                      TYPE=crypto_LUKS\n\
                      PARTUUID=9999-8888\n";
        let (fs_type, fs_uuid) = parse_blkid_export(export);
        assert_eq!(fs_type.as_deref(), Some("crypto_LUKS"));
        assert_eq!(fs_uuid.as_deref(), Some("1111-2222"));
    }

    #[test]
    fn unidentifiable_partitions_probe_to_none() {
        let (fs_type, fs_uuid) = parse_blkid_export("");
        assert!(fs_type.is_none() && fs_uuid.is_none());
    }

    #[test]
    fn luks_containers_are_recognised() {
        let mut p = sample_table().by_number(6).unwrap().clone();
        assert!(!p.is_luks());
        p.fs_type = Some("crypto_LUKS".to_string());
        assert!(p.is_luks());
    }
}
