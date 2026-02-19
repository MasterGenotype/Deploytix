//! Deployment configuration structure

use crate::disk::detection::list_block_devices;
use crate::utils::error::{DeploytixError, Result};
use crate::utils::prompt::*;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::FileTypeExt;
use std::path::Path;

/// Main deployment configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentConfig {
    pub disk: DiskConfig,
    pub system: SystemConfig,
    pub user: UserConfig,
    pub network: NetworkConfig,
    pub desktop: DesktopConfig,
}

/// One user-defined data partition for PartitionLayout::Custom.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomPartitionEntry {
    /// Root-relative mount point, e.g. "/", "/home", "/var", "/data".
    pub mount_point: String,

    /// Partition label (e.g. "ROOT", "HOME").
    /// If omitted, derived from the last path component, uppercased.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Size in MiB. Set to 0 to consume all remaining disk space.
    /// Exactly one entry in the list may be 0.
    pub size_mib: u64,

    /// Per-partition encryption override. Inherits `disk.encryption` when None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption: Option<bool>,
}

impl CustomPartitionEntry {
    /// Derive label from mount_point if not explicitly set.
    /// "/" -> "ROOT", "/home" -> "HOME", "/var/log" -> "LOG"
    pub fn effective_label(&self) -> String {
        if let Some(ref label) = self.label {
            label.clone()
        } else if self.mount_point == "/" {
            "ROOT".to_string()
        } else {
            self.mount_point
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or("DATA")
                .to_uppercase()
        }
    }

    /// Determine if this partition should be encrypted.
    pub fn is_encrypted(&self, global_encryption: bool) -> bool {
        self.encryption.unwrap_or(global_encryption)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskConfig {
    /// Target device path (e.g., /dev/sda)
    pub device: String,
    /// Partition layout preset
    #[serde(default)]
    pub layout: PartitionLayout,
    /// Filesystem type
    #[serde(default)]
    pub filesystem: Filesystem,
    /// Enable LUKS encryption on data partitions (Root, Usr, Var, Home for Standard layout)
    #[serde(default)]
    pub encryption: bool,
    /// Encryption password (if encryption enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption_password: Option<String>,
    /// Name for the LUKS mapper device (default: "Crypt-Root")
    #[serde(default = "default_luks_mapper_name")]
    pub luks_mapper_name: String,
    /// Enable LUKS1 encryption on the separate /boot partition
    #[serde(default)]
    pub boot_encryption: bool,
    /// Name for the LUKS boot mapper device (default: "Crypt-Boot")
    #[serde(default = "default_luks_boot_mapper_name")]
    pub luks_boot_mapper_name: String,
    /// Path to keyfile (None = password prompt)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyfile_path: Option<String>,
    /// Enable dm-integrity for per-sector integrity protection alongside encryption
    /// Uses HMAC-SHA256 to detect silent data corruption on encrypted volumes.
    /// Only supported with LUKS2; boot partition (LUKS1) is excluded from integrity.
    /// Not compatible with TRIM/discard.
    #[serde(default)]
    pub integrity: bool,
    /// Enable keyfile-based automatic unlocking (default: true when encryption enabled)
    #[serde(default = "default_true")]
    pub keyfile_enabled: bool,
    /// Use btrfs subvolumes within partitions (Standard layout only)
    #[serde(default)]
    pub use_subvolumes: bool,

    // LVM Thin Provisioning options
    /// Use LVM thin provisioning (for LvmThin layout)
    #[serde(default)]
    pub use_lvm_thin: bool,
    /// Volume group name (default: "vg0")
    #[serde(default = "default_vg_name")]
    pub lvm_vg_name: String,
    /// Thin pool name (default: "thinpool")
    #[serde(default = "default_thin_pool_name")]
    pub lvm_thin_pool_name: String,
    /// Thin pool size as percentage of VG (default: 95%)
    #[serde(default = "default_thin_pool_percent")]
    pub lvm_thin_pool_percent: u8,

    // Swap configuration
    /// Swap configuration type
    #[serde(default)]
    pub swap_type: SwapType,
    /// Swap file size in MiB (only for FileZram, 0 = auto-calculate based on RAM)
    #[serde(default)]
    pub swap_file_size_mib: u64,
    /// ZRAM size as percentage of RAM (default: 50%)
    #[serde(default = "default_zram_percent")]
    pub zram_percent: u8,
    /// ZRAM compression algorithm (default: "zstd")
    #[serde(default = "default_zram_algorithm")]
    pub zram_algorithm: String,

    /// Partition list for PartitionLayout::Custom.
    /// EFI, Boot, and Swap are always prepended by the system.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_partitions: Option<Vec<CustomPartitionEntry>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemConfig {
    /// Init system
    #[serde(default)]
    pub init: InitSystem,
    /// Bootloader
    #[serde(default)]
    pub bootloader: Bootloader,
    /// Timezone (e.g., "America/New_York")
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Locale (e.g., "en_US.UTF-8")
    #[serde(default = "default_locale")]
    pub locale: String,
    /// Keyboard layout
    #[serde(default = "default_keymap")]
    pub keymap: String,
    /// Hostname
    #[serde(default = "default_hostname")]
    pub hostname: String,
    /// Enable hibernation support
    #[serde(default)]
    pub hibernation: bool,

    // SecureBoot options
    /// Enable SecureBoot signing
    #[serde(default)]
    pub secureboot: bool,
    /// SecureBoot key management method
    #[serde(default)]
    pub secureboot_method: SecureBootMethod,
    /// Path to existing keys directory (for ManualKeys method)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secureboot_keys_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfig {
    /// Username
    pub name: String,
    /// User password
    pub password: String,
    /// Additional groups
    #[serde(default = "default_groups")]
    pub groups: Vec<String>,
    /// Create as sudoer (wheel group)
    #[serde(default = "default_true")]
    pub sudoer: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Network backend
    #[serde(default)]
    pub backend: NetworkBackend,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopConfig {
    /// Desktop environment
    #[serde(default)]
    pub environment: DesktopEnvironment,
    /// Display manager
    #[serde(default)]
    pub display_manager: Option<String>,
}

// Enums for configuration options

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PartitionLayout {
    /// Standard 7-partition layout (EFI, Boot, Swap, Root, Usr, Var, Home)
    /// Supports optional encryption (LUKS2 on Root/Usr/Var/Home) and/or btrfs subvolumes.
    #[default]
    Standard,
    /// Minimal 4-partition layout (EFI, Boot, Swap, Root with btrfs subvolumes)
    /// Supports both UEFI and Legacy BIOS boot.
    Minimal,
    /// LVM Thin Provisioning layout (EFI, Boot, optional Swap, LUKS+LVM PV)
    /// Thin LVs for root, usr, var, home with space-efficient overprovisioning.
    LvmThin,
    /// Custom layout (advanced)
    Custom,
}

impl std::fmt::Display for PartitionLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Standard => write!(f, "Standard (EFI, Boot, Swap, Root, Usr, Var, Home)"),
            Self::Minimal => write!(f, "Minimal (EFI, Boot, Swap, Root with subvolumes)"),
            Self::LvmThin => write!(f, "LVM Thin (EFI, Boot, LUKS+LVM with thin provisioning)"),
            Self::Custom => write!(f, "Custom"),
        }
    }
}

/// Swap configuration type
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SwapType {
    /// Traditional swap partition
    #[default]
    Partition,
    /// Swap file + ZRAM (no swap partition)
    FileZram,
    /// ZRAM only (no persistent swap)
    ZramOnly,
}

impl std::fmt::Display for SwapType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Partition => write!(f, "Swap Partition"),
            Self::FileZram => write!(f, "Swap File + ZRAM"),
            Self::ZramOnly => write!(f, "ZRAM Only"),
        }
    }
}

/// SecureBoot key management method
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SecureBootMethod {
    /// Use sbctl for key management (easiest)
    #[default]
    Sbctl,
    /// User provides PK, KEK, db keys
    ManualKeys,
    /// Use shim-signed with MOK enrollment
    Shim,
}

impl std::fmt::Display for SecureBootMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sbctl => write!(f, "sbctl (automatic key management)"),
            Self::ManualKeys => write!(f, "Manual Keys (provide your own)"),
            Self::Shim => write!(f, "Shim (MOK enrollment)"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Filesystem {
    #[default]
    Btrfs,
    Ext4,
    Xfs,
    F2fs,
}

impl std::fmt::Display for Filesystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Btrfs => write!(f, "btrfs"),
            Self::Ext4 => write!(f, "ext4"),
            Self::Xfs => write!(f, "xfs"),
            Self::F2fs => write!(f, "f2fs"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum InitSystem {
    #[default]
    Runit,
    OpenRC,
    S6,
    Dinit,
}

impl std::fmt::Display for InitSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Runit => write!(f, "runit"),
            Self::OpenRC => write!(f, "openrc"),
            Self::S6 => write!(f, "s6"),
            Self::Dinit => write!(f, "dinit"),
        }
    }
}

#[allow(dead_code)]
impl InitSystem {
    /// Get the base package name for this init system
    pub fn base_package(&self) -> &str {
        match self {
            Self::Runit => "runit",
            Self::OpenRC => "openrc",
            Self::S6 => "s6-base",
            Self::Dinit => "dinit",
        }
    }

    /// Get the service directory path
    pub fn service_dir(&self) -> &str {
        match self {
            Self::Runit => "/etc/runit/sv",
            Self::OpenRC => "/etc/init.d",
            Self::S6 => "/etc/s6/sv",
            Self::Dinit => "/etc/dinit.d",
        }
    }

    /// Get the enabled service directory path
    pub fn enabled_dir(&self) -> &str {
        match self {
            Self::Runit => "/run/runit/service",
            Self::OpenRC => "/etc/runlevels/default",
            Self::S6 => "/etc/s6/rc/compiled",
            Self::Dinit => "/etc/dinit.d/boot.d",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Bootloader {
    #[default]
    Grub,
    SystemdBoot,
}

impl std::fmt::Display for Bootloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Grub => write!(f, "GRUB"),
            Self::SystemdBoot => write!(f, "systemd-boot"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NetworkBackend {
    #[default]
    Iwd,
    NetworkManager,
}

impl std::fmt::Display for NetworkBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Iwd => write!(f, "iwd (standalone)"),
            Self::NetworkManager => write!(f, "NetworkManager + iwd"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DesktopEnvironment {
    #[default]
    None,
    Kde,
    Gnome,
    Xfce,
}

impl std::fmt::Display for DesktopEnvironment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "None (headless/server)"),
            Self::Kde => write!(f, "KDE Plasma"),
            Self::Gnome => write!(f, "GNOME"),
            Self::Xfce => write!(f, "XFCE"),
        }
    }
}

// Default value functions

fn default_timezone() -> String {
    "UTC".to_string()
}

fn default_locale() -> String {
    "en_US.UTF-8".to_string()
}

fn default_keymap() -> String {
    "us".to_string()
}

fn default_hostname() -> String {
    "artix".to_string()
}

pub fn default_luks_mapper_name() -> String {
    "Crypt-Root".to_string()
}

pub fn default_luks_boot_mapper_name() -> String {
    "Crypt-Boot".to_string()
}

fn default_vg_name() -> String {
    "vg0".to_string()
}

fn default_thin_pool_name() -> String {
    "thinpool".to_string()
}

fn default_thin_pool_percent() -> u8 {
    95
}

fn default_zram_percent() -> u8 {
    50
}

fn default_zram_algorithm() -> String {
    "zstd".to_string()
}

fn default_groups() -> Vec<String> {
    vec![
        "wheel".to_string(),
        "video".to_string(),
        "audio".to_string(),
        "network".to_string(),
        "log".to_string(),
    ]
}

fn default_true() -> bool {
    true
}

impl DeploymentConfig {
    /// Load configuration from a TOML file
    pub fn from_file(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: DeploymentConfig = toml::from_str(&content)?;
        Ok(config)
    }

    /// Create configuration interactively
    pub fn from_wizard(device: Option<String>) -> Result<Self> {
        println!("\n🚀 Deploytix Configuration Wizard\n");

        // Disk selection
        let device = if let Some(d) = device {
            d
        } else {
            let devices = list_block_devices(false)?;
            if devices.is_empty() {
                return Err(DeploytixError::ConfigError(
                    "No suitable disks found".to_string(),
                ));
            }
            let items: Vec<String> = devices
                .iter()
                .map(|d| {
                    format!(
                        "{} - {} {}",
                        d.path,
                        d.size_human(),
                        d.model.as_deref().unwrap_or("")
                    )
                })
                .collect();
            let idx = prompt_select("Select target disk", &items, 0)?;
            devices[idx].path.clone()
        };

        // Partition layout
        let layouts = [
            PartitionLayout::Standard,
            PartitionLayout::Minimal,
            PartitionLayout::LvmThin,
            PartitionLayout::Custom,
        ];
        let layout_idx = prompt_select("Partition layout", &layouts, 0)?;
        let layout = layouts[layout_idx].clone();

        // Custom partitions for Custom layout
        let custom_partitions = if layout == PartitionLayout::Custom {
            println!("\n📦 Custom Partition Configuration");
            println!("  Note: EFI (512 MiB), Boot (2 GiB), and Swap are prepended automatically.");
            println!("  Set size_mib=0 for one partition to use remaining space.\n");

            let mut entries = Vec::new();
            loop {
                let mount_point = prompt_input("Mount point (e.g. /, /home, /var)", None)?;

                // Validate mount point
                if !mount_point.starts_with('/') {
                    println!("  Mount point must start with '/'");
                    continue;
                }
                if mount_point == "/boot" || mount_point == "/boot/efi" {
                    println!("  /boot and /boot/efi are reserved for system partitions");
                    continue;
                }
                if entries
                    .iter()
                    .any(|e: &CustomPartitionEntry| e.mount_point == mount_point)
                {
                    println!("  Duplicate mount point");
                    continue;
                }

                let size_str = prompt_input("Size in MiB (0 = remaining space)", Some("0"))?;
                let size_mib: u64 = size_str.parse().unwrap_or(0);

                // Check for multiple remainder partitions
                if size_mib == 0
                    && entries
                        .iter()
                        .any(|e: &CustomPartitionEntry| e.size_mib == 0)
                {
                    println!("  Only one partition may use remaining space (size_mib=0)");
                    continue;
                }

                let default_label = if mount_point == "/" {
                    "ROOT".to_string()
                } else {
                    mount_point
                        .rsplit('/')
                        .find(|s| !s.is_empty())
                        .unwrap_or("DATA")
                        .to_uppercase()
                };
                let label_str = prompt_input(
                    &format!("Partition label [{}]", default_label),
                    Some(&default_label),
                )?;
                let label = if label_str == default_label {
                    None
                } else {
                    Some(label_str)
                };

                entries.push(CustomPartitionEntry {
                    mount_point,
                    label,
                    size_mib,
                    encryption: None, // Inherit from global setting
                });

                if !prompt_confirm("Add another partition?", true)? {
                    break;
                }
            }

            // Ensure at least one entry with mount_point == "/"
            if !entries.iter().any(|e| e.mount_point == "/") {
                println!(
                    "  Warning: No root (/) partition defined. Adding one with remaining space."
                );
                entries.insert(
                    0,
                    CustomPartitionEntry {
                        mount_point: "/".to_string(),
                        label: None,
                        size_mib: 0,
                        encryption: None,
                    },
                );
            }

            Some(entries)
        } else {
            None
        };

        // Filesystem (LvmThin requires btrfs)
        let filesystem = if layout == PartitionLayout::LvmThin {
            println!("  LvmThin layout uses btrfs filesystem.");
            Filesystem::Btrfs
        } else {
            let filesystems = [
                Filesystem::Btrfs,
                Filesystem::Ext4,
                Filesystem::Xfs,
                Filesystem::F2fs,
            ];
            let fs_idx = prompt_select("Filesystem", &filesystems, 0)?;
            filesystems[fs_idx].clone()
        };

        // Encryption option (required for LvmThin, optional for Standard and Custom)
        let encryption = if layout == PartitionLayout::LvmThin {
            println!("  LvmThin layout uses LUKS encryption.");
            true
        } else if layout == PartitionLayout::Standard || layout == PartitionLayout::Custom {
            prompt_confirm("Enable LUKS encryption on data partitions?", false)?
        } else {
            false // Minimal layout doesn't support encryption currently
        };

        // Subvolumes option (for Standard layout with btrfs)
        let use_subvolumes =
            if layout == PartitionLayout::Standard && filesystem == Filesystem::Btrfs {
                prompt_confirm("Use btrfs subvolumes within partitions?", false)?
            } else {
                layout == PartitionLayout::Minimal // Minimal always uses subvolumes
            };

        // Integrity (dm-integrity alongside LUKS2 encryption)
        let integrity = if encryption {
            prompt_confirm(
                "Enable dm-integrity (per-sector HMAC-SHA256 integrity protection)?",
                false,
            )?
        } else {
            false
        };

        // Boot encryption (LUKS1 on separate /boot partition)
        // When integrity is enabled, boot uses LUKS1 without integrity (LUKS1 doesn't support it)
        let boot_encryption = if encryption
            && (layout == PartitionLayout::Standard || layout == PartitionLayout::LvmThin)
        {
            prompt_confirm("Enable LUKS1 encryption on /boot partition?", true)?
        } else {
            false
        };

        let encryption_password = if encryption {
            Some(prompt_password("Encryption password", true)?)
        } else {
            None
        };

        // Init system
        let init_systems = [
            InitSystem::Runit,
            InitSystem::OpenRC,
            InitSystem::S6,
            InitSystem::Dinit,
        ];
        let init_idx = prompt_select("Init system", &init_systems, 0)?;
        let init = init_systems[init_idx].clone();

        // Bootloader
        let bootloaders = [Bootloader::Grub, Bootloader::SystemdBoot];
        let boot_idx = prompt_select("Bootloader", &bootloaders, 0)?;
        let bootloader = bootloaders[boot_idx].clone();

        // Locale settings
        let timezone = prompt_input("Timezone", Some("UTC"))?;
        let locale = prompt_input("Locale", Some("en_US.UTF-8"))?;
        let keymap = prompt_input("Keyboard layout", Some("us"))?;
        let hostname = prompt_input("Hostname", Some("artix"))?;

        // User
        println!("\n👤 User Configuration\n");
        let username = prompt_input("Username", None)?;
        let password = prompt_password("User password", true)?;

        // Network
        let backends = [NetworkBackend::Iwd, NetworkBackend::NetworkManager];
        let net_idx = prompt_select("Network backend", &backends, 0)?;
        let backend = backends[net_idx].clone();

        // Desktop
        let desktops = [
            DesktopEnvironment::None,
            DesktopEnvironment::Kde,
            DesktopEnvironment::Gnome,
            DesktopEnvironment::Xfce,
        ];
        let de_idx = prompt_select("Desktop environment", &desktops, 0)?;
        let environment = desktops[de_idx].clone();

        // Swap type selection
        let swap_types = [SwapType::Partition, SwapType::FileZram, SwapType::ZramOnly];
        let swap_idx = prompt_select("Swap configuration", &swap_types, 0)?;
        let swap_type = swap_types[swap_idx].clone();

        // LVM thin provisioning (for LvmThin layout)
        let use_lvm_thin = layout == PartitionLayout::LvmThin;

        // SecureBoot option
        let secureboot = prompt_confirm("Enable SecureBoot signing?", false)?;
        let secureboot_method = if secureboot {
            let methods = [
                SecureBootMethod::Sbctl,
                SecureBootMethod::ManualKeys,
                SecureBootMethod::Shim,
            ];
            let method_idx = prompt_select("SecureBoot method", &methods, 0)?;
            methods[method_idx].clone()
        } else {
            SecureBootMethod::default()
        };

        Ok(DeploymentConfig {
            disk: DiskConfig {
                device,
                layout: layout.clone(),
                filesystem,
                encryption,
                encryption_password,
                luks_mapper_name: default_luks_mapper_name(),
                boot_encryption,
                luks_boot_mapper_name: default_luks_boot_mapper_name(),
                keyfile_path: None,
                integrity,
                keyfile_enabled: encryption, // Enable keyfiles when encryption is enabled
                use_subvolumes,
                use_lvm_thin,
                lvm_vg_name: default_vg_name(),
                lvm_thin_pool_name: default_thin_pool_name(),
                lvm_thin_pool_percent: default_thin_pool_percent(),
                swap_type,
                swap_file_size_mib: 0, // Auto-calculate
                zram_percent: default_zram_percent(),
                zram_algorithm: default_zram_algorithm(),
                custom_partitions,
            },
            system: SystemConfig {
                init,
                bootloader,
                timezone,
                locale,
                keymap,
                hostname,
                hibernation: false,
                secureboot,
                secureboot_method,
                secureboot_keys_path: None,
            },
            user: UserConfig {
                name: username,
                password,
                groups: default_groups(),
                sudoer: true,
            },
            network: NetworkConfig { backend },
            desktop: DesktopConfig {
                environment,
                display_manager: None,
            },
        })
    }

    /// Generate a sample configuration
    pub fn sample() -> Self {
        DeploymentConfig {
            disk: DiskConfig {
                device: "/dev/sda".to_string(),
                layout: PartitionLayout::Standard,
                filesystem: Filesystem::Btrfs,
                encryption: false,
                encryption_password: None,
                luks_mapper_name: default_luks_mapper_name(),
                boot_encryption: false,
                luks_boot_mapper_name: default_luks_boot_mapper_name(),
                keyfile_path: None,
                integrity: false,
                keyfile_enabled: false,
                use_subvolumes: false,
                use_lvm_thin: false,
                lvm_vg_name: default_vg_name(),
                lvm_thin_pool_name: default_thin_pool_name(),
                lvm_thin_pool_percent: default_thin_pool_percent(),
                swap_type: SwapType::Partition,
                swap_file_size_mib: 0,
                zram_percent: default_zram_percent(),
                zram_algorithm: default_zram_algorithm(),
                custom_partitions: None,
            },
            system: SystemConfig {
                init: InitSystem::Runit,
                bootloader: Bootloader::Grub,
                timezone: "America/New_York".to_string(),
                locale: "en_US.UTF-8".to_string(),
                keymap: "us".to_string(),
                hostname: "artix".to_string(),
                hibernation: false,
                secureboot: false,
                secureboot_method: SecureBootMethod::Sbctl,
                secureboot_keys_path: None,
            },
            user: UserConfig {
                name: "user".to_string(),
                password: "changeme".to_string(),
                groups: default_groups(),
                sudoer: true,
            },
            network: NetworkConfig {
                backend: NetworkBackend::Iwd,
            },
            desktop: DesktopConfig {
                environment: DesktopEnvironment::Kde,
                display_manager: Some("sddm".to_string()),
            },
        }
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<()> {
        // Check device exists
        if !Path::new(&self.disk.device).exists() {
            return Err(DeploytixError::DeviceNotFound(self.disk.device.clone()));
        }

        // Check device is a block device
        let metadata = std::fs::metadata(&self.disk.device)?;
        if !metadata.file_type().is_block_device() {
            return Err(DeploytixError::NotBlockDevice(self.disk.device.clone()));
        }

        // Validate username
        if self.user.name.is_empty() {
            return Err(DeploytixError::ValidationError(
                "Username cannot be empty".to_string(),
            ));
        }
        if self.user.name.contains(' ') {
            return Err(DeploytixError::ValidationError(
                "Username cannot contain spaces".to_string(),
            ));
        }

        // Validate password
        if self.user.password.is_empty() {
            return Err(DeploytixError::ValidationError(
                "Password cannot be empty".to_string(),
            ));
        }

        // Validate encryption password if encryption enabled
        if self.disk.encryption && self.disk.encryption_password.is_none() {
            return Err(DeploytixError::ValidationError(
                "Encryption password required when encryption is enabled".to_string(),
            ));
        }

        // Subvolumes require btrfs filesystem
        if self.disk.use_subvolumes && self.disk.filesystem != Filesystem::Btrfs {
            return Err(DeploytixError::ValidationError(
                "Subvolumes require btrfs filesystem".to_string(),
            ));
        }

        // Encryption supported on Standard, LvmThin, and Custom layouts
        if self.disk.encryption
            && self.disk.layout != PartitionLayout::Standard
            && self.disk.layout != PartitionLayout::LvmThin
            && self.disk.layout != PartitionLayout::Custom
        {
            return Err(DeploytixError::ValidationError(
                "Encryption is only supported on Standard, LvmThin, and Custom layouts".to_string(),
            ));
        }

        // Integrity requires encryption
        if self.disk.integrity && !self.disk.encryption {
            return Err(DeploytixError::ValidationError(
                "Integrity (dm-integrity) requires encryption to be enabled".to_string(),
            ));
        }

        // Boot encryption requires Standard or LvmThin layout with encryption enabled
        if self.disk.boot_encryption {
            if self.disk.layout != PartitionLayout::Standard
                && self.disk.layout != PartitionLayout::LvmThin
            {
                return Err(DeploytixError::ValidationError(
                    "Boot encryption requires Standard or LvmThin layout".to_string(),
                ));
            }
            if !self.disk.encryption {
                return Err(DeploytixError::ValidationError(
                    "Boot encryption requires disk encryption to be enabled".to_string(),
                ));
            }
        }

        // LvmThin layout requires encryption
        if self.disk.layout == PartitionLayout::LvmThin && !self.disk.encryption {
            return Err(DeploytixError::ValidationError(
                "LvmThin layout requires encryption to be enabled".to_string(),
            ));
        }

        // lvm_thin_pool_percent must be 1–100 (passed as N%VG to lvcreate)
        if self.disk.lvm_thin_pool_percent == 0 || self.disk.lvm_thin_pool_percent > 100 {
            return Err(DeploytixError::ValidationError(format!(
                "lvm_thin_pool_percent must be between 1 and 100, got {}",
                self.disk.lvm_thin_pool_percent
            )));
        }

        // LvmThin layout requires btrfs filesystem
        if self.disk.layout == PartitionLayout::LvmThin && self.disk.filesystem != Filesystem::Btrfs
        {
            return Err(DeploytixError::ValidationError(
                "LvmThin layout requires btrfs filesystem".to_string(),
            ));
        }

        // Swap file requires btrfs or ext4 filesystem
        if self.disk.swap_type == SwapType::FileZram
            && self.disk.filesystem != Filesystem::Btrfs
            && self.disk.filesystem != Filesystem::Ext4
        {
            return Err(DeploytixError::ValidationError(
                "Swap file requires btrfs or ext4 filesystem".to_string(),
            ));
        }

        // SecureBoot with ManualKeys requires keys path
        if self.system.secureboot
            && self.system.secureboot_method == SecureBootMethod::ManualKeys
            && self.system.secureboot_keys_path.is_none()
        {
            return Err(DeploytixError::ValidationError(
                "SecureBoot with ManualKeys method requires secureboot_keys_path".to_string(),
            ));
        }

        // Custom layout validation
        if self.disk.layout == PartitionLayout::Custom {
            let partitions = self.disk.custom_partitions.as_ref().ok_or_else(|| {
                DeploytixError::ValidationError(
                    "Custom layout requires custom_partitions to be defined".to_string(),
                )
            })?;

            if partitions.is_empty() {
                return Err(DeploytixError::ValidationError(
                    "Custom layout requires at least one partition".to_string(),
                ));
            }

            // Must have exactly one root partition
            let root_count = partitions.iter().filter(|p| p.mount_point == "/").count();
            if root_count != 1 {
                return Err(DeploytixError::ValidationError(
                    "Custom layout must have exactly one partition with mount_point = \"/\""
                        .to_string(),
                ));
            }

            // All mount points must start with '/'
            for p in partitions {
                if !p.mount_point.starts_with('/') {
                    return Err(DeploytixError::ValidationError(format!(
                        "Mount point '{}' must start with '/'",
                        p.mount_point
                    )));
                }
            }

            // Reserved mount points
            for p in partitions {
                if p.mount_point == "/boot" || p.mount_point == "/boot/efi" {
                    return Err(DeploytixError::ValidationError(format!(
                        "Mount point '{}' is reserved for system partitions",
                        p.mount_point
                    )));
                }
            }

            // No duplicate mount points
            let mut seen = std::collections::HashSet::new();
            for p in partitions {
                if !seen.insert(&p.mount_point) {
                    return Err(DeploytixError::ValidationError(format!(
                        "Duplicate mount point '{}' in custom_partitions",
                        p.mount_point
                    )));
                }
            }

            // At most one remainder partition (size_mib = 0)
            let remainder_count = partitions.iter().filter(|p| p.size_mib == 0).count();
            if remainder_count > 1 {
                return Err(DeploytixError::ValidationError(
                    "Only one custom partition may have size_mib = 0 (remainder)".to_string(),
                ));
            }

            // Per-partition encryption requires global encryption
            for p in partitions {
                if p.encryption == Some(true) && !self.disk.encryption {
                    return Err(DeploytixError::ValidationError(format!(
                        "Partition '{}' has encryption=true but global encryption is disabled",
                        p.mount_point
                    )));
                }
            }
        }

        Ok(())
    }
}
