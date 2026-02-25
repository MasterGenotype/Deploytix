//! Unified volume representation after all storage feature layers are applied.
//!
//! The [`VolumeSet`] is built from a [`ComputedLayout`] and progressively
//! updated as encryption, LVM thin, and subvolume layers are applied.
//! Downstream consumers (fstab, crypttab, hooks, bootloader, mount/format)
//! operate on the `VolumeSet` uniformly without layout-specific branching.

use crate::configure::encryption::LuksContainer;
use crate::disk::detection::partition_path;
use crate::disk::layouts::{ComputedLayout, SubvolumeDef};
use crate::disk::lvm::ThinVolumeDef;

/// A single mountable volume in the final resolved state.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct VolumeEntry {
    /// Human-readable name (e.g. "Root", "Home", "Usr").
    pub name: String,
    /// Mount point (e.g. "/", "/home").
    pub mount_point: String,
    /// Current device path to format/mount. Updated as layers are applied:
    ///   raw partition → `/dev/mapper/Crypt-*` → `/dev/vg/lv`
    pub device_path: String,
    /// Whether this volume sits on a LUKS container.
    pub encrypted: bool,
    /// Optional LUKS container info (populated after encryption layer).
    pub luks_container: Option<LuksContainer>,
}

/// Boot volume — either a plain partition or LUKS1-mapped device.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct BootVolume {
    /// Raw partition device (e.g. `/dev/sda2`).
    pub raw_device: String,
    /// Device to format/mount. Same as `raw_device` unless boot encryption
    /// maps it to `/dev/mapper/Crypt-Boot`.
    pub device_path: String,
    /// Whether boot is LUKS1-encrypted.
    pub encrypted: bool,
    /// Optional LUKS container for encrypted boot.
    pub luks_container: Option<LuksContainer>,
}

/// Unified representation of all volumes the installer operates on.
///
/// Built from a `ComputedLayout` via [`VolumeSet::from_layout`] and
/// progressively updated by `apply_encryption` and `apply_lvm_thin`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct VolumeSet {
    /// Data volumes (root, usr, var, home, or user-defined custom volumes).
    pub entries: Vec<VolumeEntry>,
    /// Boot partition (always present in all layouts).
    pub boot: BootVolume,
    /// EFI partition device path.
    pub efi: String,
    /// Swap device path (None if swap is file-based or ZRAM-only).
    pub swap: Option<String>,
    /// Btrfs subvolumes to create/mount on the root volume (if applicable).
    pub subvolumes: Option<Vec<SubvolumeDef>>,
}

#[allow(dead_code)]
impl VolumeSet {
    /// Create an initial `VolumeSet` from a computed layout and target device.
    ///
    /// All volume device paths point to raw partitions at this stage.
    /// Call `apply_encryption` and/or `apply_lvm_thin` to update them.
    pub fn from_layout(layout: &ComputedLayout, device: &str) -> Self {
        let mut entries = Vec::new();
        let mut boot = None;
        let mut efi = String::new();
        let mut swap = None;

        for part in &layout.partitions {
            let dev = partition_path(device, part.number);

            if part.is_efi {
                efi = dev;
            } else if part.is_boot_fs {
                boot = Some(BootVolume {
                    raw_device: dev.clone(),
                    device_path: dev,
                    encrypted: false,
                    luks_container: None,
                });
            } else if part.is_swap {
                swap = Some(dev);
            } else if let Some(ref mp) = part.mount_point {
                entries.push(VolumeEntry {
                    name: part.name.clone(),
                    mount_point: mp.clone(),
                    device_path: dev,
                    encrypted: part.is_luks,
                    luks_container: None,
                });
            } else if part.name == "LVM" {
                // LVM PV partition — added by apply_lvm_thin_to_layout.
                // Represented as a single entry; apply_lvm_thin will expand
                // it into individual thin volume entries.
                entries.push(VolumeEntry {
                    name: "LVM".to_string(),
                    mount_point: String::new(), // No direct mount
                    device_path: dev,
                    encrypted: part.is_luks,
                    luks_container: None,
                });
            }
        }

        // Fallback boot device (should always be present)
        let boot = boot.unwrap_or(BootVolume {
            raw_device: String::new(),
            device_path: String::new(),
            encrypted: false,
            luks_container: None,
        });

        VolumeSet {
            entries,
            boot,
            efi,
            swap,
            subvolumes: layout.subvolumes.clone(),
        }
    }

    /// Update data volume device paths after LUKS containers have been opened.
    ///
    /// Each container is matched to a volume entry by its `volume_name`
    /// (title-cased, e.g. "Root" matches entry with name "ROOT" or "Root").
    pub fn apply_encryption(&mut self, containers: &[LuksContainer]) {
        for container in containers {
            let canon = container.volume_name.to_lowercase();
            if let Some(entry) = self
                .entries
                .iter_mut()
                .find(|e| e.name.to_lowercase() == canon)
            {
                entry.device_path = container.mapped_path.clone();
                entry.encrypted = true;
                entry.luks_container = Some(container.clone());
            }
        }
    }

    /// Update the boot volume after LUKS1 boot encryption has been set up.
    pub fn apply_boot_encryption(&mut self, container: &LuksContainer) {
        self.boot.device_path = container.mapped_path.clone();
        self.boot.encrypted = true;
        self.boot.luks_container = Some(container.clone());
    }

    /// Replace the single LVM PV entry with individual thin volume entries.
    ///
    /// The LVM PV entry (name == "LVM") is removed and replaced by one
    /// entry per thin volume definition. If encryption was applied to the
    /// LVM entry, the `lvm_container` info is preserved separately but
    /// the thin volume entries point to their LV device paths.
    pub fn apply_lvm_thin(&mut self, vg_name: &str, thin_volumes: &[ThinVolumeDef]) {
        // Remove the LVM PV placeholder entry
        self.entries.retain(|e| e.name != "LVM");

        // Add one entry per thin volume
        for vol in thin_volumes {
            self.entries.push(VolumeEntry {
                name: vol.name.clone(),
                mount_point: vol.mount_point.clone(),
                device_path: crate::disk::lvm::lv_path(vg_name, &vol.name),
                encrypted: false, // Encryption is on the PV, not on individual LVs
                luks_container: None,
            });
        }
    }

    /// Return data volume entries sorted by mount-point depth (shallowest first).
    ///
    /// This ordering is required for mounting (parent dirs before children)
    /// and fstab generation.
    pub fn entries_mount_order(&self) -> Vec<&VolumeEntry> {
        let mut sorted: Vec<&VolumeEntry> = self.entries.iter().collect();
        sorted.sort_by_key(|e| e.mount_point.matches('/').count());
        sorted
    }

    /// Return data volume entries in reverse mount order (deepest first).
    ///
    /// Used for unmounting.
    pub fn entries_unmount_order(&self) -> Vec<&VolumeEntry> {
        let mut sorted: Vec<&VolumeEntry> = self.entries.iter().collect();
        sorted.sort_by(|a, b| {
            b.mount_point
                .matches('/')
                .count()
                .cmp(&a.mount_point.matches('/').count())
        });
        sorted
    }

    /// Check if any data volume uses LUKS encryption.
    pub fn has_encryption(&self) -> bool {
        self.entries.iter().any(|e| e.encrypted) || self.boot.encrypted
    }

    /// Collect all LUKS containers from data volumes and boot.
    pub fn all_luks_containers(&self) -> Vec<&LuksContainer> {
        let mut containers: Vec<&LuksContainer> = self
            .entries
            .iter()
            .filter_map(|e| e.luks_container.as_ref())
            .collect();
        if let Some(ref c) = self.boot.luks_container {
            containers.push(c);
        }
        containers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::layouts::{partition_types, PartitionDef};

    fn sample_standard_layout() -> ComputedLayout {
        ComputedLayout {
            partitions: vec![
                PartitionDef {
                    number: 1,
                    name: "EFI".to_string(),
                    size_mib: 512,
                    type_guid: partition_types::EFI.to_string(),
                    mount_point: Some("/boot/efi".to_string()),
                    is_swap: false,
                    is_efi: true,
                    is_luks: false,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                },
                PartitionDef {
                    number: 2,
                    name: "BOOT".to_string(),
                    size_mib: 2048,
                    type_guid: partition_types::LINUX_FILESYSTEM.to_string(),
                    mount_point: Some("/boot".to_string()),
                    is_swap: false,
                    is_efi: false,
                    is_luks: false,
                    is_bios_boot: false,
                    is_boot_fs: true,
                    attributes: None,
                },
                PartitionDef {
                    number: 3,
                    name: "SWAP".to_string(),
                    size_mib: 8192,
                    type_guid: partition_types::LINUX_SWAP.to_string(),
                    mount_point: None,
                    is_swap: true,
                    is_efi: false,
                    is_luks: false,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                },
                PartitionDef {
                    number: 4,
                    name: "ROOT".to_string(),
                    size_mib: 20480,
                    type_guid: partition_types::LINUX_ROOT_X86_64.to_string(),
                    mount_point: Some("/".to_string()),
                    is_swap: false,
                    is_efi: false,
                    is_luks: true,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                },
                PartitionDef {
                    number: 5,
                    name: "HOME".to_string(),
                    size_mib: 0,
                    type_guid: partition_types::LINUX_HOME.to_string(),
                    mount_point: Some("/home".to_string()),
                    is_swap: false,
                    is_efi: false,
                    is_luks: true,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                },
            ],
            total_mib: 500000,
            subvolumes: None,
            planned_thin_volumes: None,
        }
    }

    #[test]
    fn from_layout_populates_all_fields() {
        let layout = sample_standard_layout();
        let vs = VolumeSet::from_layout(&layout, "/dev/sda");

        assert_eq!(vs.efi, "/dev/sda1");
        assert_eq!(vs.boot.raw_device, "/dev/sda2");
        assert!(vs.swap.is_some());
        assert_eq!(vs.swap.as_ref().unwrap(), "/dev/sda3");
        assert_eq!(vs.entries.len(), 2); // ROOT + HOME
        assert_eq!(vs.entries[0].name, "ROOT");
        assert_eq!(vs.entries[0].mount_point, "/");
        assert!(vs.entries[0].encrypted);
        assert_eq!(vs.entries[1].name, "HOME");
    }

    #[test]
    fn apply_encryption_updates_paths() {
        let layout = sample_standard_layout();
        let mut vs = VolumeSet::from_layout(&layout, "/dev/sda");

        let containers = vec![LuksContainer {
            device: "/dev/sda4".to_string(),
            mapper_name: "Crypt-Root".to_string(),
            mapped_path: "/dev/mapper/Crypt-Root".to_string(),
            volume_name: "Root".to_string(),
        }];

        vs.apply_encryption(&containers);

        assert_eq!(vs.entries[0].device_path, "/dev/mapper/Crypt-Root");
        assert!(vs.entries[0].luks_container.is_some());
        // HOME not in containers, should remain unchanged
        assert_eq!(vs.entries[1].device_path, "/dev/sda5");
    }

    #[test]
    fn apply_boot_encryption_updates_boot_volume() {
        let layout = sample_standard_layout();
        let mut vs = VolumeSet::from_layout(&layout, "/dev/sda");

        let boot_container = LuksContainer {
            device: "/dev/sda2".to_string(),
            mapper_name: "Crypt-Boot".to_string(),
            mapped_path: "/dev/mapper/Crypt-Boot".to_string(),
            volume_name: "Boot".to_string(),
        };

        vs.apply_boot_encryption(&boot_container);

        assert!(vs.boot.encrypted);
        assert_eq!(vs.boot.device_path, "/dev/mapper/Crypt-Boot");
        assert_eq!(vs.boot.raw_device, "/dev/sda2"); // Raw unchanged
    }

    #[test]
    fn apply_lvm_thin_replaces_lvm_entry() {
        // Create a layout with an LVM PV partition
        let layout = ComputedLayout {
            partitions: vec![
                PartitionDef {
                    number: 1,
                    name: "EFI".to_string(),
                    size_mib: 512,
                    type_guid: partition_types::EFI.to_string(),
                    mount_point: Some("/boot/efi".to_string()),
                    is_swap: false,
                    is_efi: true,
                    is_luks: false,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                },
                PartitionDef {
                    number: 2,
                    name: "BOOT".to_string(),
                    size_mib: 2048,
                    type_guid: partition_types::LINUX_FILESYSTEM.to_string(),
                    mount_point: Some("/boot".to_string()),
                    is_swap: false,
                    is_efi: false,
                    is_luks: false,
                    is_bios_boot: false,
                    is_boot_fs: true,
                    attributes: None,
                },
                PartitionDef {
                    number: 3,
                    name: "LVM".to_string(),
                    size_mib: 0,
                    type_guid: partition_types::LINUX_FILESYSTEM.to_string(),
                    mount_point: None,
                    is_swap: false,
                    is_efi: false,
                    is_luks: true,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                },
            ],
            total_mib: 500000,
            subvolumes: None,
            planned_thin_volumes: None,
        };

        let mut vs = VolumeSet::from_layout(&layout, "/dev/sda");
        assert_eq!(vs.entries.len(), 1);
        assert_eq!(vs.entries[0].name, "LVM");

        let thin_vols = vec![
            ThinVolumeDef {
                name: "root".to_string(),
                virtual_size: "50G".to_string(),
                mount_point: "/".to_string(),
            },
            ThinVolumeDef {
                name: "home".to_string(),
                virtual_size: "200G".to_string(),
                mount_point: "/home".to_string(),
            },
        ];

        vs.apply_lvm_thin("vg0", &thin_vols);

        assert_eq!(vs.entries.len(), 2);
        assert_eq!(vs.entries[0].name, "root");
        assert_eq!(vs.entries[0].device_path, "/dev/vg0/root");
        assert_eq!(vs.entries[1].name, "home");
        assert_eq!(vs.entries[1].device_path, "/dev/vg0/home");
    }

    #[test]
    fn entries_mount_order_sorts_by_depth() {
        let layout = sample_standard_layout();
        let vs = VolumeSet::from_layout(&layout, "/dev/sda");

        let ordered = vs.entries_mount_order();
        // "/" should come before "/home"
        assert_eq!(ordered[0].mount_point, "/");
        assert_eq!(ordered[1].mount_point, "/home");
    }

    #[test]
    fn all_luks_containers_collects_from_data_and_boot() {
        let layout = sample_standard_layout();
        let mut vs = VolumeSet::from_layout(&layout, "/dev/sda");

        let data_container = LuksContainer {
            device: "/dev/sda4".to_string(),
            mapper_name: "Crypt-Root".to_string(),
            mapped_path: "/dev/mapper/Crypt-Root".to_string(),
            volume_name: "Root".to_string(),
        };
        let boot_container = LuksContainer {
            device: "/dev/sda2".to_string(),
            mapper_name: "Crypt-Boot".to_string(),
            mapped_path: "/dev/mapper/Crypt-Boot".to_string(),
            volume_name: "Boot".to_string(),
        };

        vs.apply_encryption(&[data_container]);
        vs.apply_boot_encryption(&boot_container);

        let containers = vs.all_luks_containers();
        assert_eq!(containers.len(), 2);
    }
}
