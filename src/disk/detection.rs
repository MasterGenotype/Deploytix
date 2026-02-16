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
        const KIB: u64 = 1024;
        const MIB: u64 = KIB * 1024;
        const GIB: u64 = MIB * 1024;
        const TIB: u64 = GIB * 1024;

        if self.size_bytes >= TIB {
            format!("{:.1}T", self.size_bytes as f64 / TIB as f64)
        } else if self.size_bytes >= GIB {
            format!("{:.1}G", self.size_bytes as f64 / GIB as f64)
        } else if self.size_bytes >= MIB {
            format!("{:.1}M", self.size_bytes as f64 / MIB as f64)
        } else if self.size_bytes >= KIB {
            format!("{:.1}K", self.size_bytes as f64 / KIB as f64)
        } else {
            format!("{}B", self.size_bytes)
        }
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

    // Check if USB
    let removable = read_sysfs_u64(device, "removable").unwrap_or(0);
    if removable == 1 {
        return "usb".to_string();
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

/// Check if a device is mounted
fn is_device_mounted(device: &str) -> bool {
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    mounts.lines().any(|line| line.starts_with(device))
}

/// List available block devices
///
/// If `all` is false, filters to only show suitable installation targets
/// (excludes mounted devices, read-only devices, loop devices, etc.)
pub fn list_block_devices(all: bool) -> Result<Vec<BlockDevice>> {
    let mut devices = Vec::new();

    for entry in fs::read_dir("/sys/block")? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();

        // Skip partitions (they show up in /sys/block for nvme)
        if name.contains('p') && name.starts_with("nvme") && name.len() > 7 {
            continue;
        }

        // Get device info
        let device_type = determine_device_type(&name);

        // Skip loop devices unless showing all
        if !all && device_type == "loop" {
            continue;
        }

        // Get size
        let size_sectors = read_sysfs_u64(&name, "size").unwrap_or(0);
        if size_sectors == 0 {
            continue;
        }

        // Get sector size (default to 512)
        let sector_size = read_sysfs_u64(&name, "queue/logical_block_size").unwrap_or(512);
        let size_bytes = size_sectors * sector_size;

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
    let size_sectors = read_sysfs_u64(&name, "size").unwrap_or(0);
    let sector_size = read_sysfs_u64(&name, "queue/logical_block_size").unwrap_or(512);
    let size_bytes = size_sectors * sector_size;

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

/// Get the partition naming prefix for a device
/// e.g., /dev/sda -> /dev/sda, /dev/nvme0n1 -> /dev/nvme0n1p
pub fn partition_prefix(device: &str) -> String {
    if device.contains("nvme") || device.contains("mmcblk") || device.contains("loop") {
        format!("{}p", device)
    } else {
        device.to_string()
    }
}

/// Get partition path for a device and partition number
pub fn partition_path(device: &str, partition_num: u32) -> String {
    format!("{}{}", partition_prefix(device), partition_num)
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
