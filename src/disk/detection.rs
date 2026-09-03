//! Disk detection and enumeration

use crate::utils::error::Result;
use std::fs;
use std::path::Path;

/// Information about a block device
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct BlockDevice {
    /// Device path (e.g., /dev/sda)
    pub path: String,
    /// Device name (e.g., sda)
    pub name: String,
    /// Size in bytes
    pub size_bytes: u64,
    /// Device model (if available)
    pub model: Option<String>,
    /// Device type (disk, usb, nvme, etc.)
    pub device_type: String,
    /// Whether device is removable
    pub removable: bool,
    /// Whether device is read-only
    pub read_only: bool,
}

impl BlockDevice {
    /// Get human-readable size
    pub fn size_human(&self) -> String {
        human_bytes(self.size_bytes)
    }

    /// Get size in MiB
    pub fn size_mib(&self) -> u64 {
        self.size_bytes / (1024 * 1024)
    }
}

/// Read a sysfs attribute, returning None if not available
fn read_sysfs_attr(device: &str, attr: &str) -> Option<String> {
    let path = format!("/sys/block/{}/{}", device, attr);
    fs::read_to_string(&path).ok().map(|s| s.trim().to_string())
}

/// Read a numeric sysfs attribute
fn read_sysfs_u64(device: &str, attr: &str) -> Option<u64> {
    read_sysfs_attr(device, attr).and_then(|s| s.parse().ok())
}

/// Determine device type from sysfs
fn determine_device_type(device: &str) -> String {
    // Check if NVMe
    if device.starts_with("nvme") {
        return "nvme".to_string();
    }

    // Check if MMC/SD card
    if device.starts_with("mmcblk") {
        return "mmc".to_string();
    }

    // Check if loop device
    if device.starts_with("loop") {
        return "loop".to_string();
    }

    // Check if removable (USB drives, SD card readers, etc.)
    let removable = read_sysfs_u64(device, "removable").unwrap_or(0);
    if removable == 1 {
        return "removable".to_string();
    }

    // Check transport type
    if let Some(rotation) = read_sysfs_u64(device, "queue/rotational") {
        if rotation == 0 {
            return "ssd".to_string();
        } else {
            return "hdd".to_string();
        }
    }

    "disk".to_string()
}

/// Check if a block device name represents a physical whole-disk.
///
/// Only known real disk types are accepted: SCSI/SATA/USB (`sd*`),
/// NVMe (`nvme*`), MMC/SD (`mmcblk*`), virtio (`vd*`), Xen (`xvd*`),
/// and IDE (`hd*`).  Everything else — device-mapper (`dm-*`), ZRAM,
/// loop, software RAID (`md*`), optical (`sr*`), network block devices
/// (`nbd*`) — returns `false`.
fn is_physical_disk(name: &str) -> bool {
    name.starts_with("sd")
        || name.starts_with("nvme")
        || name.starts_with("mmcblk")
        || name.starts_with("vd")
        || name.starts_with("xvd")
        || name.starts_with("hd")
}

/// Check if a device (or any of its partitions) is mounted.
///
/// Matches both the whole-disk device (e.g. `/dev/sda`) and any partition
/// derived from it (e.g. `/dev/sda1`, `/dev/nvme0n1p2`).
fn is_device_mounted(device: &str) -> bool {
    let prefix = partition_prefix(device);
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    mounts.lines().any(|line| {
        if let Some(dev) = line.split_whitespace().next() {
            dev == device || dev.starts_with(&prefix)
        } else {
            false
        }
    })
}

/// List available block devices
///
/// If `all` is false, filters to only show suitable installation targets:
/// physical whole-disks (SCSI/SATA/USB, NVMe, MMC/SD, virtio, Xen, IDE).
/// Device-mapper devices (dm-crypt, LVM), ZRAM, loop, software RAID, optical
/// drives, and other virtual block devices are excluded.
/// Format a byte count as a short human-readable size (e.g. `324.4G`).
pub fn human_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;

    if bytes >= TIB {
        format!("{:.1}T", bytes as f64 / TIB as f64)
    } else if bytes >= GIB {
        format!("{:.1}G", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1}M", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1}K", bytes as f64 / KIB as f64)
    } else {
        format!("{}B", bytes)
    }
}

pub fn list_block_devices(all: bool) -> Result<Vec<BlockDevice>> {
    let mut devices = Vec::new();

    for entry in fs::read_dir("/sys/block")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip partitions (they show up in /sys/block for nvme)
        if name.contains('p') && name.starts_with("nvme") && name.len() > 7 {
            continue;
        }

        // When filtering for installation targets, only include physical
        // whole-disks.  This excludes device-mapper (dm-*), ZRAM, loop,
        // software RAID (md*), optical drives (sr*), network block
        // devices (nbd*), and any other virtual block device.
        if !all && !is_physical_disk(&name) {
            continue;
        }

        // Get device info
        let device_type = determine_device_type(&name);

        // Get size
        // /sys/block/<dev>/size always reports in 512-byte sectors
        // regardless of the device's logical block size.
        let size_sectors = read_sysfs_u64(&name, "size").unwrap_or(0);
        if size_sectors == 0 {
            continue;
        }

        let size_bytes = size_sectors * 512;

        // Skip very small devices (< 1GB) unless showing all
        if !all && size_bytes < 1024 * 1024 * 1024 {
            continue;
        }

        let removable = read_sysfs_u64(&name, "removable").unwrap_or(0) == 1;
        let read_only = read_sysfs_u64(&name, "ro").unwrap_or(0) == 1;

        // Skip read-only devices unless showing all
        if !all && read_only {
            continue;
        }

        // Get model
        let model = read_sysfs_attr(&name, "device/model")
            .or_else(|| read_sysfs_attr(&name, "device/name"));

        let path = format!("/dev/{}", name);

        // Skip mounted devices unless showing all
        if !all && is_device_mounted(&path) {
            continue;
        }

        devices.push(BlockDevice {
            path,
            name,
            size_bytes,
            model,
            device_type,
            removable,
            read_only,
        });
    }

    // Sort by path
    devices.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(devices)
}

/// Get information about a specific device
pub fn get_device_info(device_path: &str) -> Result<BlockDevice> {
    let path = Path::new(device_path);
    let name = path
        .file_name()
        .ok_or_else(|| {
            crate::utils::error::DeploytixError::DeviceNotFound(device_path.to_string())
        })?
        .to_string_lossy()
        .to_string();

    let device_type = determine_device_type(&name);
    // /sys/block/<dev>/size always reports in 512-byte sectors
    // regardless of the device's logical block size.
    let size_sectors = read_sysfs_u64(&name, "size").unwrap_or(0);
    let size_bytes = size_sectors * 512;

    let removable = read_sysfs_u64(&name, "removable").unwrap_or(0) == 1;
    let read_only = read_sysfs_u64(&name, "ro").unwrap_or(0) == 1;
    let model =
        read_sysfs_attr(&name, "device/model").or_else(|| read_sysfs_attr(&name, "device/name"));

    Ok(BlockDevice {
        path: device_path.to_string(),
        name,
        size_bytes,
        model,
        device_type,
        removable,
        read_only,
    })
}

/// Get the partition naming prefix for a device.
///
/// Mirrors the kernel's partition-naming rule (see `disk_name()` in
/// `block/genhd.c`): if the disk name ends in a digit, append a `p`
/// separator so the partition number can't be concatenated ambiguously.
///
/// Examples:
///   /dev/sda      -> /dev/sda        (sda1, sda2, ...)
///   /dev/vda      -> /dev/vda        (vda1, vda2, ...)
///   /dev/nvme0n1  -> /dev/nvme0n1p   (nvme0n1p1, ...)
///   /dev/mmcblk0  -> /dev/mmcblk0p   (mmcblk0p1, ...)
///   /dev/loop0    -> /dev/loop0p     (loop0p1, ...)
///   /dev/nbd0     -> /dev/nbd0p      (nbd0p1, ...)
///   /dev/md0      -> /dev/md0p       (md0p1, ...)
pub fn partition_prefix(device: &str) -> String {
    match device.chars().last() {
        Some(c) if c.is_ascii_digit() => format!("{}p", device),
        _ => device.to_string(),
    }
}

/// Get partition path for a device and partition number
pub fn partition_path(device: &str, partition_num: u32) -> String {
    format!("{}{}", partition_prefix(device), partition_num)
}

/// Select the swap entries in `/proc/swaps` that belong to the install target.
///
/// `swaps` is the contents of `/proc/swaps`, whose first column is a device
/// path (or a swap file path) and whose first line is a header. `device` is the
/// target disk, when known.
///
/// Blanket `swapoff -a` is wrong: run from an installed Artix host rather than
/// the live medium, it takes the host's swap down with it. But so is matching
/// only `install_root` and `Crypt-` mappers — every `swapon` in the install path
/// goes through [`partition_path`], giving a raw `/dev/sda3`, so that filter
/// matches nothing the installer actually enabled and leaves the target's swap
/// on, blocking the wipe that follows.
///
/// An entry is selected when it is:
/// - a partition of `device` (or `device` itself), or
/// - a swap file staged under `install_root`, or
/// - an encrypted swap mapper this installer would have opened.
///
/// With `device` of `None` only the last two apply — conservative on purpose:
/// leave swap enabled rather than guess at a disk.
pub fn target_swap_devices(swaps: &str, install_root: &str, device: Option<&str>) -> Vec<String> {
    let prefix = device.map(partition_prefix);

    swaps
        .lines()
        .skip(1) // header
        .filter_map(|line| line.split_whitespace().next())
        .filter(|entry| {
            let on_target_disk = match (&prefix, device) {
                (Some(p), Some(d)) => entry.starts_with(p.as_str()) || *entry == d,
                _ => false,
            };
            on_target_disk
                || entry.starts_with(install_root)
                || entry.contains("/dev/mapper/Crypt-")
        })
        .map(|entry| entry.to_string())
        .collect()
}

/// Get the amount of RAM in MiB
pub fn get_ram_mib() -> u64 {
    let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    for line in meminfo.lines() {
        if line.starts_with("MemTotal:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(kb) = parts[1].parse::<u64>() {
                    return kb / 1024;
                }
            }
        }
    }
    // Fallback: 8GB
    8192
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── partition_prefix ─────────────────────────────────────────────────────

    #[test]
    fn partition_prefix_appends_p_for_nvme_device() {
        assert_eq!(partition_prefix("/dev/nvme0n1"), "/dev/nvme0n1p");
        assert_eq!(partition_prefix("/dev/nvme1n2"), "/dev/nvme1n2p");
    }

    #[test]
    fn partition_prefix_appends_p_for_mmcblk_device() {
        assert_eq!(partition_prefix("/dev/mmcblk0"), "/dev/mmcblk0p");
    }

    #[test]
    fn partition_prefix_appends_p_for_loop_device() {
        assert_eq!(partition_prefix("/dev/loop0"), "/dev/loop0p");
    }

    #[test]
    fn partition_prefix_no_suffix_for_sata_drive() {
        assert_eq!(partition_prefix("/dev/sda"), "/dev/sda");
        assert_eq!(partition_prefix("/dev/sdb"), "/dev/sdb");
    }

    #[test]
    fn partition_prefix_no_suffix_for_virtio_drive() {
        assert_eq!(partition_prefix("/dev/vda"), "/dev/vda");
    }

    // ── partition_path ───────────────────────────────────────────────────────

    #[test]
    fn partition_path_sata_uses_direct_numbering() {
        assert_eq!(partition_path("/dev/sda", 1), "/dev/sda1");
        assert_eq!(partition_path("/dev/sda", 3), "/dev/sda3");
    }

    #[test]
    fn partition_path_nvme_uses_p_separator() {
        assert_eq!(partition_path("/dev/nvme0n1", 1), "/dev/nvme0n1p1");
        assert_eq!(partition_path("/dev/nvme0n1", 2), "/dev/nvme0n1p2");
    }

    #[test]
    fn partition_path_mmcblk_uses_p_separator() {
        assert_eq!(partition_path("/dev/mmcblk0", 1), "/dev/mmcblk0p1");
    }

    #[test]
    fn partition_prefix_appends_p_for_nbd_device() {
        // Regression: NBD devices end in a digit, so they require the `p`
        // separator just like nvme/mmcblk/loop.  Previously the installer
        // built `/dev/nbd03` for partition 3 on /dev/nbd0 and crashed with
        // `Device /dev/nbd03 does not exist or access denied.`
        assert_eq!(partition_prefix("/dev/nbd0"), "/dev/nbd0p");
        assert_eq!(partition_prefix("/dev/nbd15"), "/dev/nbd15p");
    }

    #[test]
    fn partition_path_nbd_uses_p_separator() {
        assert_eq!(partition_path("/dev/nbd0", 1), "/dev/nbd0p1");
        assert_eq!(partition_path("/dev/nbd0", 3), "/dev/nbd0p3");
        assert_eq!(partition_path("/dev/nbd15", 2), "/dev/nbd15p2");
    }

    #[test]
    fn partition_prefix_appends_p_for_md_device() {
        assert_eq!(partition_prefix("/dev/md0"), "/dev/md0p");
        assert_eq!(partition_prefix("/dev/md127"), "/dev/md127p");
    }

    #[test]
    fn partition_path_multi_digit_partition_number_on_nbd() {
        // Make sure the `p` separator keeps partition numbers unambiguous
        // even with two-digit partition numbers.
        assert_eq!(partition_path("/dev/nbd1", 10), "/dev/nbd1p10");
    }

    // ── is_physical_disk ──────────────────────────────────────────────────────

    #[test]
    fn physical_disk_accepts_real_disk_types() {
        assert!(is_physical_disk("sda"));
        assert!(is_physical_disk("sdb"));
        assert!(is_physical_disk("nvme0n1"));
        assert!(is_physical_disk("mmcblk0"));
        assert!(is_physical_disk("vda"));
        assert!(is_physical_disk("xvda"));
        assert!(is_physical_disk("hda"));
    }

    #[test]
    fn physical_disk_rejects_virtual_devices() {
        assert!(!is_physical_disk("dm-0"));
        assert!(!is_physical_disk("dm-1"));
        assert!(!is_physical_disk("loop0"));
        assert!(!is_physical_disk("loop1"));
        assert!(!is_physical_disk("zram0"));
        assert!(!is_physical_disk("md0"));
        assert!(!is_physical_disk("sr0"));
        assert!(!is_physical_disk("nbd0"));
        assert!(!is_physical_disk("ram0"));
    }

    /// Realistic `/proc/swaps`: a header, the target's swap partition, the
    /// live session's zram, and a host swapfile.
    const SWAPS: &str = "\
Filename\t\t\t\tType\t\tSize\tUsed\tPriority
/dev/sda3                               partition\t8388604\t0\t-2
/dev/zram0                              partition\t4194300\t0\t100
/swapfile                               file\t\t2097152\t0\t-3
";

    #[test]
    fn target_swap_selects_the_partition_the_installer_enabled() {
        // The case the install_root/Crypt- filter misses entirely: every
        // swapon in the install path uses partition_path(), so the entry is a
        // raw /dev/sdaN, not a mapper and not under /install.
        let got = target_swap_devices(SWAPS, "/install", Some("/dev/sda"));
        assert_eq!(got, vec!["/dev/sda3"]);
    }

    #[test]
    fn target_swap_leaves_the_live_session_alone() {
        // zram (the live swap) and the host's swapfile must survive, or
        // finalize disables the swap the ISO just gave us.
        let got = target_swap_devices(SWAPS, "/install", Some("/dev/sda"));
        assert!(!got.iter().any(|d| d == "/dev/zram0"));
        assert!(!got.iter().any(|d| d == "/swapfile"));
    }

    #[test]
    fn target_swap_does_not_confuse_neighbouring_nvme_namespaces() {
        let swaps = "Filename\tType\tSize\tUsed\tPriority
/dev/nvme0n1p3\tpartition\t8388604\t0\t-2
/dev/nvme0n2p3\tpartition\t8388604\t0\t-3
";
        let got = target_swap_devices(swaps, "/install", Some("/dev/nvme0n1"));
        assert_eq!(got, vec!["/dev/nvme0n1p3"]);
    }

    #[test]
    fn target_swap_matches_staged_files_and_encrypted_mappers() {
        let swaps = "Filename\tType\tSize\tUsed\tPriority
/install/swap/swapfile\tfile\t2097152\t0\t-2
/dev/mapper/Crypt-Swap\tpartition\t8388604\t0\t-3
/dev/sdb2\tpartition\t8388604\t0\t-4
";
        let got = target_swap_devices(swaps, "/install", Some("/dev/sda"));
        assert_eq!(
            got,
            vec!["/install/swap/swapfile", "/dev/mapper/Crypt-Swap"]
        );
    }

    /// No target disk known (`deploytix cleanup` with neither --device nor
    /// --wipe): stay conservative rather than guessing at a disk.
    #[test]
    fn target_swap_without_a_device_only_matches_install_root_and_mappers() {
        let got = target_swap_devices(SWAPS, "/install", None);
        assert!(got.is_empty(), "must not touch host swap: {got:?}");

        let swaps = "Filename\tType\tSize\tUsed\tPriority
/dev/mapper/Crypt-Swap\tpartition\t8388604\t0\t-2
/dev/sda3\tpartition\t8388604\t0\t-3
";
        assert_eq!(
            target_swap_devices(swaps, "/install", None),
            vec!["/dev/mapper/Crypt-Swap"]
        );
    }

    #[test]
    fn target_swap_handles_an_empty_or_header_only_table() {
        assert!(target_swap_devices("", "/install", Some("/dev/sda")).is_empty());
        assert!(target_swap_devices(
            "Filename\tType\tSize\tUsed\tPriority\n",
            "/install",
            Some("/dev/sda")
        )
        .is_empty());
    }
}
