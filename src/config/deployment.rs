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
    /// Optional package collections (AUR helper, Wine, Gaming, GPU drivers)
    #[serde(default)]
    pub packages: PackagesConfig,
}

/// One user-defined data partition.
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
    /// Filesystem type for data partitions
    #[serde(default)]
    pub filesystem: Filesystem,
    /// Filesystem type for the /boot partition (ext4, btrfs, xfs, zfs, f2fs).
    /// Defaults to ext4 for maximum GRUB compatibility.
    #[serde(default = "default_boot_filesystem")]
    pub boot_filesystem: Filesystem,
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
    /// Use btrfs subvolumes within partitions.
    /// Automatically set to true whenever `filesystem == Btrfs`; no manual
    /// opt-in is required.  Kept as a serialisable field for backwards
    /// compatibility with existing configuration files.
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
    /// ZRAM compression algorithm (default: "zstd")
    #[serde(default = "default_zram_algorithm")]
    pub zram_algorithm: String,

    /// Preserve existing /home partition during reinstall.
    /// When true, the installer skips repartitioning and formatting of the
    /// /home partition/volume/subvolume, allowing a system reinstall without
    /// overwriting user files.  Requires an existing compatible partition table.
    #[serde(default)]
    pub preserve_home: bool,

    /// User-defined data partitions (e.g. ROOT, HOME, USR, VAR).
    /// EFI + Boot are always auto-prepended; Swap is prepended when
    /// `swap_type == Partition`.
    #[serde(default = "default_partitions")]
    pub partitions: Vec<CustomPartitionEntry>,
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

/// Optional package collections
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackagesConfig {
    /// Install yay AUR helper (built from source; requires go)
    #[serde(default)]
    pub install_yay: bool,
    /// Install Wine compatibility packages
    #[serde(default)]
    pub install_wine: bool,
    /// Install gaming packages (Steam, gamescope)
    #[serde(default)]
    pub install_gaming: bool,
    /// Install session switching scripts (gamescope ↔ desktop mode via greetd).
    /// Requires: install_gaming = true + a desktop environment.
    #[serde(default)]
    pub install_session_switching: bool,
    /// Install btrfs snapshot tools (snapper, btrfs-assistant) via yay.
    /// Requires: install_yay = true + btrfs filesystem.
    #[serde(default)]
    pub install_btrfs_tools: bool,
    /// Install Modular mod manager (CLI + GUI) for game mod management
    #[serde(default)]
    pub install_modular: bool,
    /// Apply gaming/handheld sysctl performance tweaks.
    /// Writes /etc/sysctl.d/99-gaming.conf with vm.max_map_count, swappiness, etc.
    #[serde(default)]
    pub sysctl_gaming_tweaks: bool,
    /// Apply network performance sysctl tweaks.
    /// Writes /etc/sysctl.d/99-network-performance.conf with BBR + fq, larger
    /// socket buffers, MTU probing, ECN, and hygiene defaults. Complements
    /// (does not conflict with) the gaming sysctl config.
    #[serde(default)]
    pub sysctl_network_performance: bool,
    /// Install Handheld Daemon (HHD) — gamepad remapping, TDP control, per-game profiles.
    /// Requires: install_yay = true (AUR package: hhd-git).
    /// Writes an init-specific service file for runit/s6/dinit/openrc.
    #[serde(default)]
    pub install_hhd: bool,
    /// Install Decky Loader (Steam plugin framework).
    /// Requires: install_gaming = true AND install_yay = true
    /// (installed from the decky-loader-bin AUR package).
    /// Writes an init-specific service file for runit/s6/dinit/openrc.
    #[serde(default)]
    pub install_decky_loader: bool,
    /// Install evdevhook2 — Cemuhook UDP motion server for modern Linux drivers
    /// (DualShock 3/4, DualSense, DualSense Edge, Nintendo Joy-Cons / Pro
    /// Controller).  Requires: install_yay = true (AUR package: evdevhook2-git).
    /// Ships a udev rule that grants the `input` group access to motion
    /// sensor evdev nodes, adds the created user to the `input` group, and
    /// writes an init-specific service file for runit/s6/dinit/openrc.
    #[serde(default)]
    pub install_evdevhook2: bool,
    /// GPU driver vendors to install
    #[serde(default)]
    pub gpu_drivers: Vec<GpuDriverVendor>,
}

/// GPU driver vendor selection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum GpuDriverVendor {
    Nvidia,
    Amd,
    Intel,
}

impl std::fmt::Display for GpuDriverVendor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nvidia => write!(f, "NVIDIA"),
            Self::Amd => write!(f, "AMD"),
            Self::Intel => write!(f, "Intel"),
        }
    }
}

// Enums for configuration options

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
    Zfs,
    F2fs,
}

impl std::fmt::Display for Filesystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Btrfs => write!(f, "btrfs"),
            Self::Ext4 => write!(f, "ext4"),
            Self::Xfs => write!(f, "xfs"),
            Self::Zfs => write!(f, "zfs"),
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
}

impl std::fmt::Display for Bootloader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Grub => write!(f, "GRUB"),
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

fn default_zram_algorithm() -> String {
    "zstd".to_string()
}

pub fn default_groups() -> Vec<String> {
    vec![
        "wheel".to_string(),
        "video".to_string(),
        "audio".to_string(),
        "input".to_string(),
        "render".to_string(),
        "network".to_string(),
        "log".to_string(),
        "seat".to_string(),
    ]
}

/// Default partition set: root, usr, var, and home.
///
/// Sizes are sensible starting defaults; the GUI sliders let the user
/// redistribute space.  `/home` uses the remainder of the disk.
pub fn default_partitions() -> Vec<CustomPartitionEntry> {
    vec![
        CustomPartitionEntry {
            mount_point: "/".to_string(),
            label: None,
            size_mib: 20480, // 20 GiB
            encryption: None,
        },
        CustomPartitionEntry {
            mount_point: "/usr".to_string(),
            label: None,
            size_mib: 30720, // 30 GiB
            encryption: None,
        },
        CustomPartitionEntry {
            mount_point: "/var".to_string(),
            label: None,
            size_mib: 10240, // 10 GiB
            encryption: None,
        },
        CustomPartitionEntry {
            mount_point: "/home".to_string(),
            label: None,
            size_mib: 0, // Remainder
            encryption: None,
        },
    ]
}

pub fn default_boot_filesystem() -> Filesystem {
    Filesystem::Ext4
}

/// Derive the boot filesystem from the data filesystem.
/// Btrfs data → Btrfs boot (with @boot subvolume); everything else → Ext4.
pub fn boot_filesystem_for(fs: &Filesystem) -> Filesystem {
    match fs {
        Filesystem::Btrfs => Filesystem::Btrfs,
        _ => Filesystem::Ext4,
    }
}

fn default_true() -> bool {
    true
}

impl DeploymentConfig {
    /// Load configuration from a TOML file.
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

        // Partition definition
        println!("\n📦 Partition Configuration");
        println!("  EFI (512 MiB) and Boot (2 GiB) are added automatically.");
        println!("  Swap partition is added when Swap Type is set to Partition.");
        println!("  Set size_mib=0 for one partition to use remaining space.\n");

        let mut partitions: Vec<CustomPartitionEntry> = Vec::new();
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
            if partitions
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
                && partitions
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

            partitions.push(CustomPartitionEntry {
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
        if !partitions.iter().any(|e| e.mount_point == "/") {
            // If another partition already claims the remainder (size_mib == 0),
            // give the auto-inserted root a default size instead.
            let has_remainder = partitions.iter().any(|e| e.size_mib == 0);
            let root_size = if has_remainder { 20480 } else { 0 };
            println!(
                "  Warning: No root (/) partition defined. Adding one with {} space.",
                if root_size == 0 {
                    "remaining"
                } else {
                    "20 GiB"
                }
            );
            partitions.insert(
                0,
                CustomPartitionEntry {
                    mount_point: "/".to_string(),
                    label: None,
                    size_mib: root_size,
                    encryption: None,
                },
            );
        }

        // Data filesystem
        let filesystems = [
            Filesystem::Btrfs,
            Filesystem::Ext4,
            Filesystem::Xfs,
            Filesystem::Zfs,
            Filesystem::F2fs,
        ];
        let fs_idx = prompt_select("Data filesystem", &filesystems, 0)?;
        let filesystem = filesystems[fs_idx].clone();

        // Boot filesystem is derived from the data filesystem:
        // btrfs → btrfs boot (with @boot subvolume), everything else → ext4
        let boot_filesystem = boot_filesystem_for(&filesystem);

        // Encryption option (available on all layouts)
        let encryption = prompt_confirm("Enable LUKS encryption on data partitions?", false)?;

        // Subvolumes are enabled unconditionally for btrfs. No prompt needed.
        let use_subvolumes = filesystem == Filesystem::Btrfs;

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
        let boot_encryption = if encryption {
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

        // Bootloader (GRUB is the only supported bootloader on Artix)
        let bootloader = Bootloader::Grub;

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

        // Preserve home partition (for reinstall scenarios)
        let preserve_home = prompt_confirm(
            "Preserve existing /home partition? (for reinstall without overwriting user files)",
            false,
        )?;

        // LVM thin provisioning (available on all layouts)
        let use_lvm_thin = prompt_confirm("Enable LVM thin provisioning?", false)?;

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

        // Optional package collections
        println!("\n📦 Optional Package Collections\n");

        // GPU drivers (multi-select)
        let gpu_vendors = [
            GpuDriverVendor::Nvidia,
            GpuDriverVendor::Amd,
            GpuDriverVendor::Intel,
        ];
        let gpu_defaults = vec![false; gpu_vendors.len()];
        let gpu_selected = prompt_multi_select(
            "Video/Graphics Drivers (space to toggle, enter to confirm)",
            &gpu_vendors,
            &gpu_defaults,
        )?;
        let gpu_drivers: Vec<GpuDriverVendor> = gpu_selected
            .iter()
            .map(|&i| gpu_vendors[i].clone())
            .collect();

        // Wine
        let install_wine = prompt_confirm("Install Wine compatibility packages?", false)?;

        // Gaming
        let install_gaming = prompt_confirm("Install Gaming packages (Steam, gamescope)?", false)?;

        // Session switching (only if gaming + desktop are both selected)
        let install_session_switching = if install_gaming && environment != DesktopEnvironment::None
        {
            prompt_confirm(
                "Enable session switching (Game Mode ↔ Desktop via greetd)?",
                true,
            )?
        } else {
            false
        };

        // yay AUR helper
        let install_yay = prompt_confirm("Install yay AUR helper? (built from source)", false)?;

        // Btrfs tools (snapper + btrfs-assistant) via yay — only when btrfs + yay
        let install_btrfs_tools = if install_yay && filesystem == Filesystem::Btrfs {
            prompt_confirm(
                "Install btrfs snapshot tools (snapper, btrfs-assistant) via yay?",
                false,
            )?
        } else {
            false
        };

        let install_modular = prompt_confirm(
            "Install Modular mod manager? (CLI + GUI for NexusMods, GameBanana)",
            false,
        )?;

        // sysctl gaming tweaks (standalone — no prerequisites)
        let sysctl_gaming_tweaks = prompt_confirm(
            "Apply gaming sysctl performance tweaks? (vm.max_map_count, swappiness, etc.)",
            false,
        )?;

        // sysctl network performance tweaks (standalone — no prerequisites)
        let sysctl_network_performance = prompt_confirm(
            "Apply network performance sysctl tweaks? (BBR + fq, larger socket buffers, ECN, etc.)",
            false,
        )?;

        // HHD — requires yay (AUR)
        let install_hhd = if install_yay {
            prompt_confirm(
                "Install Handheld Daemon (HHD)? (gamepad remapping, TDP, profiles — for handhelds)",
                false,
            )?
        } else {
            false
        };

        // Decky Loader — requires gaming packages (Steam) and yay (AUR)
        let install_decky_loader = if install_gaming && install_yay {
            prompt_confirm(
                "Install Decky Loader? (Steam plugin framework — decky-loader-bin via AUR)",
                false,
            )?
        } else {
            false
        };

        // evdevhook2 — requires yay (AUR)
        let install_evdevhook2 = if install_yay {
            prompt_confirm(
                "Install evdevhook2? (Cemuhook UDP motion server for DualShock/DualSense/Joy-Cons)",
                false,
            )?
        } else {
            false
        };

        Ok(DeploymentConfig {
            disk: DiskConfig {
                device,
                filesystem,
                boot_filesystem,
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
                zram_algorithm: default_zram_algorithm(),
                preserve_home,
                partitions,
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
            packages: PackagesConfig {
                install_yay,
                install_wine,
                install_gaming,
                install_session_switching,
                install_btrfs_tools,
                install_modular,
                sysctl_gaming_tweaks,
                sysctl_network_performance,
                install_hhd,
                install_decky_loader,
                install_evdevhook2,
                gpu_drivers,
            },
        })
    }

    /// Generate a sample configuration
    pub fn sample() -> Self {
        DeploymentConfig {
            disk: DiskConfig {
                device: "/dev/sda".to_string(),
                filesystem: Filesystem::Btrfs,
                boot_filesystem: Filesystem::Btrfs,
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
                zram_algorithm: default_zram_algorithm(),
                preserve_home: false,
                partitions: default_partitions(),
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
            packages: PackagesConfig::default(),
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

        // Subvolumes require btrfs filesystem (ZFS uses datasets, not subvolumes)
        if self.disk.use_subvolumes && self.disk.filesystem != Filesystem::Btrfs {
            return Err(DeploytixError::ValidationError(
                "Subvolumes require btrfs filesystem (ZFS uses datasets instead)".to_string(),
            ));
        }

        // ZFS manages its own volumes; LVM thin provisioning is redundant and
        // unsupported when the data filesystem is ZFS.
        if self.disk.use_lvm_thin && self.disk.filesystem == Filesystem::Zfs {
            return Err(DeploytixError::ValidationError(
                "LVM thin provisioning is not supported with ZFS (ZFS manages its own volumes)"
                    .to_string(),
            ));
        }

        // boot_encryption is LUKS1 only - boot_filesystem must not be ZFS
        // (ZFS on /boot with LUKS1 is unsupported)
        if self.disk.boot_encryption && self.disk.boot_filesystem == Filesystem::Zfs {
            return Err(DeploytixError::ValidationError(
                "ZFS is not supported as the boot filesystem when boot encryption (LUKS1) is enabled".to_string(),
            ));
        }

        // preserve_home is incompatible with ZFS (pools cannot be partially preserved)
        if self.disk.preserve_home && self.disk.filesystem == Filesystem::Zfs {
            return Err(DeploytixError::ValidationError(
                "preserve_home is not supported with ZFS filesystem (pools cannot be partially preserved)".to_string(),
            ));
        }

        // preserve_home is incompatible with LVM thin provisioning — the entire
        // LVM PV (including the home thin volume) must be recreated from scratch.
        if self.disk.preserve_home && self.disk.use_lvm_thin {
            return Err(DeploytixError::ValidationError(
                "preserve_home is not supported with LVM thin provisioning (the LVM PV cannot be partially preserved)"
                    .to_string(),
            ));
        }

        // preserve_home without a dedicated /home partition requires subvolumes
        // (there must be an @home subvolume to preserve).
        let has_home_partition = self
            .disk
            .partitions
            .iter()
            .any(|p| p.mount_point == "/home");
        if self.disk.preserve_home && !has_home_partition && !self.disk.use_subvolumes {
            return Err(DeploytixError::ValidationError(
                "preserve_home requires either a /home partition or use_subvolumes = true"
                    .to_string(),
            ));
        }

        // Integrity requires encryption
        if self.disk.integrity && !self.disk.encryption {
            return Err(DeploytixError::ValidationError(
                "Integrity (dm-integrity) requires encryption to be enabled".to_string(),
            ));
        }

        // Boot encryption requires encryption to be enabled
        if self.disk.boot_encryption && !self.disk.encryption {
            return Err(DeploytixError::ValidationError(
                "Boot encryption requires disk encryption to be enabled".to_string(),
            ));
        }

        // lvm_thin_pool_percent must be 1–100 (passed as N%VG to lvcreate)
        if self.disk.lvm_thin_pool_percent == 0 || self.disk.lvm_thin_pool_percent > 100 {
            return Err(DeploytixError::ValidationError(format!(
                "lvm_thin_pool_percent must be between 1 and 100, got {}",
                self.disk.lvm_thin_pool_percent
            )));
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

        // Partition list validation
        let partitions = &self.disk.partitions;

        if partitions.is_empty() {
            return Err(DeploytixError::ValidationError(
                "At least one partition must be defined".to_string(),
            ));
        }

        // Must have exactly one root partition
        let root_count = partitions.iter().filter(|p| p.mount_point == "/").count();
        if root_count != 1 {
            return Err(DeploytixError::ValidationError(
                "Exactly one partition with mount_point = \"/\" is required".to_string(),
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
                    "Duplicate mount point '{}' in partitions",
                    p.mount_point
                )));
            }
        }

        // At most one remainder partition (size_mib = 0)
        let remainder_count = partitions.iter().filter(|p| p.size_mib == 0).count();
        if remainder_count > 1 {
            return Err(DeploytixError::ValidationError(
                "Only one partition may have size_mib = 0 (remainder)".to_string(),
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

        // Session switching requires gaming + a desktop environment
        if self.packages.install_session_switching {
            if !self.packages.install_gaming {
                return Err(DeploytixError::ValidationError(
                    "Session switching requires install_gaming = true".to_string(),
                ));
            }
            if self.desktop.environment == DesktopEnvironment::None {
                return Err(DeploytixError::ValidationError(
                    "Session switching requires a desktop environment".to_string(),
                ));
            }
        }

        // HHD requires yay (AUR)
        if self.packages.install_hhd && !self.packages.install_yay {
            return Err(DeploytixError::ValidationError(
                "Handheld Daemon (HHD) requires install_yay = true".to_string(),
            ));
        }

        // evdevhook2 requires yay (AUR)
        if self.packages.install_evdevhook2 && !self.packages.install_yay {
            return Err(DeploytixError::ValidationError(
                "evdevhook2 requires install_yay = true (AUR package: evdevhook2-git)".to_string(),
            ));
        }

        // Decky Loader requires gaming (Steam) + yay (decky-loader-bin is AUR)
        if self.packages.install_decky_loader {
            if !self.packages.install_gaming {
                return Err(DeploytixError::ValidationError(
                    "Decky Loader requires install_gaming = true".to_string(),
                ));
            }
            if !self.packages.install_yay {
                return Err(DeploytixError::ValidationError(
                    "Decky Loader requires install_yay = true (installed from decky-loader-bin AUR package)".to_string(),
                ));
            }
        }

        // Btrfs tools require yay + btrfs filesystem
        if self.packages.install_btrfs_tools {
            if !self.packages.install_yay {
                return Err(DeploytixError::ValidationError(
                    "Btrfs tools (snapper, btrfs-assistant) require install_yay = true".to_string(),
                ));
            }
            if self.disk.filesystem != Filesystem::Btrfs {
                return Err(DeploytixError::ValidationError(
                    "Btrfs tools require btrfs filesystem".to_string(),
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── CustomPartitionEntry::effective_label ────────────────────────────────

    #[test]
    fn effective_label_uses_explicit_label_when_set() {
        let p = CustomPartitionEntry {
            mount_point: "/data".into(),
            size_mib: 0,
            label: Some("MYDATA".into()),
            encryption: None,
        };
        assert_eq!(p.effective_label(), "MYDATA");
    }

    #[test]
    fn effective_label_root_returns_root_constant() {
        let p = CustomPartitionEntry {
            mount_point: "/".into(),
            size_mib: 0,
            label: None,
            encryption: None,
        };
        assert_eq!(p.effective_label(), "ROOT");
    }

    #[test]
    fn effective_label_derives_uppercase_from_last_path_component() {
        let cases = [
            ("/home", "HOME"),
            ("/var", "VAR"),
            ("/var/log", "LOG"),
            ("/opt/data", "DATA"),
        ];
        for (mount, expected) in cases {
            let p = CustomPartitionEntry {
                mount_point: mount.into(),
                size_mib: 0,
                label: None,
                encryption: None,
            };
            assert_eq!(
                p.effective_label(),
                expected,
                "wrong label for mount_point='{}'",
                mount
            );
        }
    }

    // ── CustomPartitionEntry::is_encrypted ───────────────────────────────────

    #[test]
    fn is_encrypted_inherits_global_when_none() {
        let p = CustomPartitionEntry {
            mount_point: "/data".into(),
            size_mib: 0,
            label: None,
            encryption: None,
        };
        assert!(p.is_encrypted(true), "should inherit global=true");
        assert!(!p.is_encrypted(false), "should inherit global=false");
    }

    #[test]
    fn is_encrypted_overrides_global_when_explicitly_set() {
        let force_on = CustomPartitionEntry {
            mount_point: "/data".into(),
            size_mib: 0,
            label: None,
            encryption: Some(true),
        };
        assert!(
            force_on.is_encrypted(false),
            "explicit true overrides global false"
        );

        let force_off = CustomPartitionEntry {
            mount_point: "/data".into(),
            size_mib: 0,
            label: None,
            encryption: Some(false),
        };
        assert!(
            !force_off.is_encrypted(true),
            "explicit false overrides global true"
        );
    }

    // ── InitSystem methods ───────────────────────────────────────────────────

    #[test]
    fn init_system_base_package_returns_correct_package() {
        assert_eq!(InitSystem::Runit.base_package(), "runit");
        assert_eq!(InitSystem::OpenRC.base_package(), "openrc");
        assert_eq!(InitSystem::S6.base_package(), "s6-base");
        assert_eq!(InitSystem::Dinit.base_package(), "dinit");
    }

    #[test]
    fn init_system_service_dir_returns_correct_path() {
        assert_eq!(InitSystem::Runit.service_dir(), "/etc/runit/sv");
        assert_eq!(InitSystem::OpenRC.service_dir(), "/etc/init.d");
        assert_eq!(InitSystem::S6.service_dir(), "/etc/s6/sv");
        assert_eq!(InitSystem::Dinit.service_dir(), "/etc/dinit.d");
    }

    #[test]
    fn init_system_enabled_dir_returns_correct_path() {
        assert_eq!(InitSystem::Runit.enabled_dir(), "/run/runit/service");
        assert_eq!(InitSystem::OpenRC.enabled_dir(), "/etc/runlevels/default");
        assert_eq!(InitSystem::S6.enabled_dir(), "/etc/s6/rc/compiled");
        assert_eq!(InitSystem::Dinit.enabled_dir(), "/etc/dinit.d/boot.d");
    }

    // NOTE: DeploymentConfig::validate() cannot currently be unit-tested in
    // isolation because it checks block device existence as its very first
    // step, before any of the pure business-logic rules (username, password,
    // encryption constraints, custom layout rules, etc.).  The business rules
    // are all correct and well-defined, but they are only reachable when a
    // real block device is present.
    //
    // Recommended future improvement: extract the pure rule checks into a
    // separate `validate_config_rules()` helper so they can be unit-tested
    // without hardware.  See the test-coverage proposal document for details.
}
