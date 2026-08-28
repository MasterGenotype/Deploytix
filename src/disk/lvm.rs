//! LVM thin provisioning operations
//!
//! Provides functions for creating and managing LVM thin pools and volumes
//! on top of LUKS-encrypted devices.

use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use tracing::info;

/// LVM thin volume definition
#[derive(Debug, Clone)]
pub struct ThinVolumeDef {
    /// Logical volume name (e.g., "root", "home")
    pub name: String,
    /// Virtual size (can exceed physical storage due to thin provisioning)
    pub virtual_size: String,
    /// Mount point
    pub mount_point: String,
}

/// Default thin volumes for LvmThin layout
pub fn default_thin_volumes() -> Vec<ThinVolumeDef> {
    vec![
        ThinVolumeDef {
            name: "root".to_string(),
            virtual_size: "50G".to_string(),
            mount_point: "/".to_string(),
        },
        ThinVolumeDef {
            name: "usr".to_string(),
            virtual_size: "50G".to_string(),
            mount_point: "/usr".to_string(),
        },
        ThinVolumeDef {
            name: "var".to_string(),
            virtual_size: "30G".to_string(),
            mount_point: "/var".to_string(),
        },
        ThinVolumeDef {
            name: "home".to_string(),
            virtual_size: "200G".to_string(),
            mount_point: "/home".to_string(),
        },
    ]
}

// ── Immutable A/B dm-verity layout ───────────────────────────────────────────
//
// The LVM immutable backend uses two read-only root slots (`root_a`, `root_b`),
// a dm-verity Merkle-tree store per slot (`hash_a`, `hash_b`), persistent
// writable `var`/`home`, and a writable `/etc` overlay upper (`etc_overlay`).
// See `src/immutable/lvm_ab.rs` and `docs/IMMUTABLE_LVM_AB.md`.

/// Logical-volume names for the immutable A/B layout (shared by the installer
/// and the `deploytix update`/`rollback` LVM engine).
pub mod ab {
    /// Slot A root image (OS + `/usr`).
    pub const ROOT_A: &str = "root_a";
    /// Slot B root image (OS + `/usr`).
    pub const ROOT_B: &str = "root_b";
    /// dm-verity hash tree for slot A.
    pub const HASH_A: &str = "hash_a";
    /// dm-verity hash tree for slot B.
    pub const HASH_B: &str = "hash_b";
    /// Persistent writable `/var`.
    pub const VAR: &str = "var";
    /// Persistent writable `/home`.
    pub const HOME: &str = "home";
    /// Writable overlay upper for `/etc`.
    pub const ETC_OVERLAY: &str = "etc_overlay";

    /// The root/hash LV names for a slot letter (`"A"`/`"B"`).
    pub fn slot_lvs(slot: &str) -> Option<(&'static str, &'static str)> {
        match slot {
            "A" | "a" => Some((ROOT_A, HASH_A)),
            "B" | "b" => Some((ROOT_B, HASH_B)),
            _ => None,
        }
    }

    /// The other slot letter.
    pub fn other_slot(slot: &str) -> &'static str {
        match slot {
            "A" | "a" => "B",
            _ => "A",
        }
    }
}

/// Role of an LV in the immutable A/B layout, which governs how the installer
/// treats it (formatting/mounting/verity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbVolumeRole {
    /// Slot root image: formatted with the chosen fs. Slot A is installed into;
    /// slot B is cloned from A. Read-only + dm-verity at boot.
    SlotRoot,
    /// dm-verity hash-tree store: raw block device, never formatted or mounted.
    VerityHash,
    /// Persistent writable data volume (formatted + mounted at `mount_point`).
    Data,
    /// Writable `/etc` overlay upper (formatted; mounted via overlayfs).
    EtcOverlay,
}

/// One LV in the immutable A/B layout.
#[derive(Debug, Clone)]
pub struct AbLvDef {
    /// Logical volume name.
    pub name: String,
    /// Virtual (thin) size.
    pub virtual_size: String,
    /// Role, governing installer handling.
    pub role: AbVolumeRole,
    /// Mount point for `Data`/`EtcOverlay`/`SlotRoot`; empty for `VerityHash`.
    pub mount_point: String,
}

/// The immutable A/B dm-verity thin-volume layout.
///
/// `root_a`/`root_b` hold the OS image (including `/usr`); `hash_a`/`hash_b`
/// hold the dm-verity trees; `var`/`home` are persistent; `etc_overlay` backs
/// the writable `/etc` overlay.
pub fn immutable_ab_thin_volumes() -> Vec<AbLvDef> {
    vec![
        AbLvDef {
            name: ab::ROOT_A.to_string(),
            virtual_size: "40G".to_string(),
            role: AbVolumeRole::SlotRoot,
            mount_point: "/".to_string(),
        },
        AbLvDef {
            name: ab::ROOT_B.to_string(),
            virtual_size: "40G".to_string(),
            role: AbVolumeRole::SlotRoot,
            mount_point: "/".to_string(),
        },
        AbLvDef {
            name: ab::HASH_A.to_string(),
            virtual_size: "1G".to_string(),
            role: AbVolumeRole::VerityHash,
            mount_point: String::new(),
        },
        AbLvDef {
            name: ab::HASH_B.to_string(),
            virtual_size: "1G".to_string(),
            role: AbVolumeRole::VerityHash,
            mount_point: String::new(),
        },
        AbLvDef {
            name: ab::VAR.to_string(),
            virtual_size: "30G".to_string(),
            role: AbVolumeRole::Data,
            mount_point: "/var".to_string(),
        },
        AbLvDef {
            name: ab::HOME.to_string(),
            virtual_size: "200G".to_string(),
            role: AbVolumeRole::Data,
            mount_point: "/home".to_string(),
        },
        AbLvDef {
            name: ab::ETC_OVERLAY.to_string(),
            virtual_size: "2G".to_string(),
            role: AbVolumeRole::EtcOverlay,
            mount_point: "/etc".to_string(),
        },
    ]
}

/// Create all LVs in the immutable A/B layout (thin LVs only; formatting and
/// verity are handled by the installer).
pub fn create_ab_thin_volumes(
    cmd: &CommandRunner,
    vg_name: &str,
    pool_name: &str,
    volumes: &[AbLvDef],
) -> Result<()> {
    info!(
        "Creating {} immutable A/B thin volumes in {}/{}",
        volumes.len(),
        vg_name,
        pool_name
    );
    for vol in volumes {
        create_thin_lv(cmd, vg_name, pool_name, &vol.name, &vol.virtual_size)?;
    }
    Ok(())
}

/// Create a physical volume on a device
pub fn create_pv(cmd: &CommandRunner, device: &str) -> Result<()> {
    info!("Creating LVM physical volume on {}", device);

    if cmd.is_dry_run() {
        println!("  [dry-run] pvcreate {}", device);
        return Ok(());
    }

    cmd.run("pvcreate", &["-ff", "-y", device])
        .map(|_| ())
        .map_err(|e| DeploytixError::CommandFailed {
            command: "pvcreate".to_string(),
            stderr: e.to_string(),
        })?;

    info!("Physical volume created on {}", device);
    Ok(())
}

/// Create a volume group
pub fn create_vg(cmd: &CommandRunner, vg_name: &str, pv_device: &str) -> Result<()> {
    info!("Creating volume group '{}' on {}", vg_name, pv_device);

    if cmd.is_dry_run() {
        println!("  [dry-run] vgcreate {} {}", vg_name, pv_device);
        return Ok(());
    }

    cmd.run("vgcreate", &[vg_name, pv_device])
        .map(|_| ())
        .map_err(|e| DeploytixError::CommandFailed {
            command: "vgcreate".to_string(),
            stderr: e.to_string(),
        })?;

    info!("Volume group '{}' created", vg_name);
    Ok(())
}

/// Create a thin pool in a volume group
///
/// The thin pool is created with the specified percentage of the VG size.
/// Metadata is automatically sized (typically 1% of pool size, min 2MiB).
pub fn create_thin_pool(
    cmd: &CommandRunner,
    vg_name: &str,
    pool_name: &str,
    size_percent: u8,
) -> Result<()> {
    info!(
        "Creating thin pool '{}/{}' using {}% of VG",
        vg_name, pool_name, size_percent
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] lvcreate --type thin-pool -l {}%VG -n {} {}",
            size_percent, pool_name, vg_name
        );
        return Ok(());
    }

    // Create thin pool with percentage of VG
    // Using --type thin-pool creates both data and metadata LVs
    cmd.run(
        "lvcreate",
        &[
            "--type",
            "thin-pool",
            "-l",
            &format!("{}%VG", size_percent),
            "-n",
            pool_name,
            vg_name,
        ],
    )
    .map(|_| ())
    .map_err(|e| DeploytixError::CommandFailed {
        command: "lvcreate thin-pool".to_string(),
        stderr: e.to_string(),
    })?;

    info!("Thin pool '{}/{}' created", vg_name, pool_name);
    Ok(())
}

/// Create a thin logical volume from a thin pool
///
/// Thin LVs can have virtual sizes larger than the physical pool size
/// (overprovisioning), as space is only allocated on write.
pub fn create_thin_lv(
    cmd: &CommandRunner,
    vg_name: &str,
    pool_name: &str,
    lv_name: &str,
    virtual_size: &str,
) -> Result<()> {
    info!(
        "Creating thin LV '{}/{}' (virtual size: {}) from pool '{}'",
        vg_name, lv_name, virtual_size, pool_name
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] lvcreate -V {} --thin -n {} {}/{}",
            virtual_size, lv_name, vg_name, pool_name
        );
        return Ok(());
    }

    cmd.run(
        "lvcreate",
        &[
            "-V",
            virtual_size,
            "--thin",
            "-n",
            lv_name,
            &format!("{}/{}", vg_name, pool_name),
        ],
    )
    .map(|_| ())
    .map_err(|e| DeploytixError::CommandFailed {
        command: "lvcreate thin".to_string(),
        stderr: e.to_string(),
    })?;

    info!("Thin LV '{}/{}' created", vg_name, lv_name);
    Ok(())
}

/// Create all thin volumes from definitions
pub fn create_all_thin_volumes(
    cmd: &CommandRunner,
    vg_name: &str,
    pool_name: &str,
    volumes: &[ThinVolumeDef],
) -> Result<()> {
    info!(
        "Creating {} thin volumes in {}/{}",
        volumes.len(),
        vg_name,
        pool_name
    );

    for vol in volumes {
        create_thin_lv(cmd, vg_name, pool_name, &vol.name, &vol.virtual_size)?;
    }

    info!("All thin volumes created successfully");
    Ok(())
}

/// Activate a volume group
pub fn activate_vg(cmd: &CommandRunner, vg_name: &str) -> Result<()> {
    info!("Activating volume group '{}'", vg_name);

    if cmd.is_dry_run() {
        println!("  [dry-run] vgchange -ay {}", vg_name);
        return Ok(());
    }

    cmd.run("vgchange", &["-ay", vg_name])
        .map(|_| ())
        .map_err(|e| DeploytixError::CommandFailed {
            command: "vgchange -ay".to_string(),
            stderr: e.to_string(),
        })?;

    // Wait for device nodes to appear
    let _ = cmd.run("udevadm", &["settle"]);

    info!("Volume group '{}' activated", vg_name);
    Ok(())
}

/// Deactivate a volume group
pub fn deactivate_vg(cmd: &CommandRunner, vg_name: &str) -> Result<()> {
    info!("Deactivating volume group '{}'", vg_name);

    if cmd.is_dry_run() {
        println!("  [dry-run] vgchange -an {}", vg_name);
        return Ok(());
    }

    cmd.run("vgchange", &["-an", vg_name])
        .map(|_| ())
        .map_err(|e| DeploytixError::CommandFailed {
            command: "vgchange -an".to_string(),
            stderr: e.to_string(),
        })?;

    info!("Volume group '{}' deactivated", vg_name);
    Ok(())
}

/// Get the device mapper path for a logical volume
pub fn lv_path(vg_name: &str, lv_name: &str) -> String {
    format!("/dev/{}/{}", vg_name, lv_name)
}

/// Get the device mapper path (alternative format)
///
/// Some tools prefer `/dev/mapper/vg-lv` format over `/dev/vg/lv`.
/// Both formats work, but mapper paths are canonical for device-mapper.
/// Device-mapper doubles hyphens in VG/LV names (e.g. `my-vg` becomes `my--vg`).
pub fn lv_mapper_path(vg_name: &str, lv_name: &str) -> String {
    let escaped_vg = vg_name.replace('-', "--");
    let escaped_lv = lv_name.replace('-', "--");
    format!("/dev/mapper/{}-{}", escaped_vg, escaped_lv)
}

/// Get LV path in either format
///
/// Returns (standard_path, mapper_path) tuple.
pub fn lv_paths(vg_name: &str, lv_name: &str) -> (String, String) {
    (lv_path(vg_name, lv_name), lv_mapper_path(vg_name, lv_name))
}

/// Scan for and activate all volume groups
///
/// Useful for recovery scenarios where VGs need to be discovered.
pub fn scan_and_activate(cmd: &CommandRunner) -> Result<()> {
    info!("Scanning for LVM volume groups");

    if cmd.is_dry_run() {
        println!("  [dry-run] vgscan");
        println!("  [dry-run] vgchange -ay");
        return Ok(());
    }

    cmd.run("vgscan", &[])
        .map_err(|e| DeploytixError::CommandFailed {
            command: "vgscan".to_string(),
            stderr: e.to_string(),
        })?;

    cmd.run("vgchange", &["-ay"])
        .map_err(|e| DeploytixError::CommandFailed {
            command: "vgchange -ay".to_string(),
            stderr: e.to_string(),
        })?;

    // Wait for device nodes
    let _ = cmd.run("udevadm", &["settle"]);

    Ok(())
}

/// Get thin pool usage information
#[allow(dead_code)]
pub fn get_thin_pool_usage(vg_name: &str, pool_name: &str) -> Result<(f64, f64)> {
    use std::process::Command;

    let output = Command::new("lvs")
        .args([
            "--noheadings",
            "--units",
            "b",
            "-o",
            "data_percent,metadata_percent",
            &format!("{}/{}", vg_name, pool_name),
        ])
        .output()
        .map_err(|e| DeploytixError::CommandFailed {
            command: "lvs".to_string(),
            stderr: e.to_string(),
        })?;

    if !output.status.success() {
        return Err(DeploytixError::CommandFailed {
            command: "lvs".to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.split_whitespace().collect();

    if parts.len() >= 2 {
        let data_percent = parts[0].parse::<f64>().unwrap_or(0.0);
        let meta_percent = parts[1].parse::<f64>().unwrap_or(0.0);
        Ok((data_percent, meta_percent))
    } else {
        Ok((0.0, 0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapper_path_doubles_hyphens() {
        assert_eq!(
            lv_mapper_path("my-vg", "root_a"),
            "/dev/mapper/my--vg-root_a"
        );
        assert_eq!(lv_path("vg0", "root_a"), "/dev/vg0/root_a");
    }

    #[test]
    fn ab_slot_lvs_and_other() {
        assert_eq!(ab::slot_lvs("A"), Some((ab::ROOT_A, ab::HASH_A)));
        assert_eq!(ab::slot_lvs("b"), Some((ab::ROOT_B, ab::HASH_B)));
        assert_eq!(ab::slot_lvs("C"), None);
        assert_eq!(ab::other_slot("A"), "B");
        assert_eq!(ab::other_slot("B"), "A");
    }

    #[test]
    fn ab_layout_has_both_slots_hashes_and_shared_state() {
        let vols = immutable_ab_thin_volumes();
        let roots: Vec<_> = vols
            .iter()
            .filter(|v| v.role == AbVolumeRole::SlotRoot)
            .map(|v| v.name.as_str())
            .collect();
        assert_eq!(roots, vec![ab::ROOT_A, ab::ROOT_B]);

        let hashes: Vec<_> = vols
            .iter()
            .filter(|v| v.role == AbVolumeRole::VerityHash)
            .collect();
        assert_eq!(hashes.len(), 2);
        // Hash stores are raw block devices — never mounted.
        assert!(hashes.iter().all(|v| v.mount_point.is_empty()));

        // Persistent + overlay state present exactly once each.
        assert!(vols
            .iter()
            .any(|v| v.name == ab::VAR && v.mount_point == "/var"));
        assert!(vols
            .iter()
            .any(|v| v.name == ab::HOME && v.mount_point == "/home"));
        assert!(vols
            .iter()
            .any(|v| v.role == AbVolumeRole::EtcOverlay && v.mount_point == "/etc"));
    }
}
