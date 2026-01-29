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
    /// Enable LUKS encryption
    #[serde(default)]
    pub encryption: bool,
    /// Encryption password (if encryption enabled)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption_password: Option<String>,
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
    /// DNS provider
    #[serde(default)]
    pub dns: DnsProvider,
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
    #[default]
    Standard,
    /// Minimal layout (EFI, Swap, Root)
    Minimal,
    /// Custom layout (advanced)
    Custom,
}

impl std::fmt::Display for PartitionLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Standard => write!(f, "Standard (EFI, Boot, Swap, Root, Usr, Var, Home)"),
            Self::Minimal => write!(f, "Minimal (EFI, Swap, Root)"),
            Self::Custom => write!(f, "Custom"),
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
    Connman,
}

impl std::fmt::Display for NetworkBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Iwd => write!(f, "iwd (standalone)"),
            Self::NetworkManager => write!(f, "NetworkManager + iwd"),
            Self::Connman => write!(f, "ConnMan"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum DnsProvider {
    #[default]
    DnscryptProxy,
    Systemd,
    None,
}

impl std::fmt::Display for DnsProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DnscryptProxy => write!(f, "dnscrypt-proxy"),
            Self::Systemd => write!(f, "systemd-resolved"),
            Self::None => write!(f, "None (use DHCP)"),
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
        ];
        let layout_idx = prompt_select("Partition layout", &layouts, 0)?;
        let layout = layouts[layout_idx].clone();

        // Filesystem
        let filesystems = [Filesystem::Btrfs, Filesystem::Ext4, Filesystem::Xfs, Filesystem::F2fs];
        let fs_idx = prompt_select("Filesystem", &filesystems, 0)?;
        let filesystem = filesystems[fs_idx].clone();

        // Encryption
        let encryption = prompt_confirm("Enable LUKS encryption?", false)?;
        let encryption_password = if encryption {
            Some(prompt_password("Encryption password", true)?)
        } else {
            None
        };

        // Init system
        let init_systems = [InitSystem::Runit, InitSystem::OpenRC, InitSystem::S6, InitSystem::Dinit];
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
        let backends = [NetworkBackend::Iwd, NetworkBackend::NetworkManager, NetworkBackend::Connman];
        let net_idx = prompt_select("Network backend", &backends, 0)?;
        let backend = backends[net_idx].clone();

        let dns_providers = [DnsProvider::DnscryptProxy, DnsProvider::Systemd, DnsProvider::None];
        let dns_idx = prompt_select("DNS provider", &dns_providers, 0)?;
        let dns = dns_providers[dns_idx].clone();

        // Desktop
        let desktops = [
            DesktopEnvironment::None,
            DesktopEnvironment::Kde,
            DesktopEnvironment::Gnome,
            DesktopEnvironment::Xfce,
        ];
        let de_idx = prompt_select("Desktop environment", &desktops, 0)?;
        let environment = desktops[de_idx].clone();

        Ok(DeploymentConfig {
            disk: DiskConfig {
                device,
                layout,
                filesystem,
                encryption,
                encryption_password,
            },
            system: SystemConfig {
                init,
                bootloader,
                timezone,
                locale,
                keymap,
                hostname,
                hibernation: false,
            },
            user: UserConfig {
                name: username,
                password,
                groups: default_groups(),
                sudoer: true,
            },
            network: NetworkConfig { backend, dns },
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
            },
            system: SystemConfig {
                init: InitSystem::Runit,
                bootloader: Bootloader::Grub,
                timezone: "America/New_York".to_string(),
                locale: "en_US.UTF-8".to_string(),
                keymap: "us".to_string(),
                hostname: "artix".to_string(),
                hibernation: false,
            },
            user: UserConfig {
                name: "user".to_string(),
                password: "changeme".to_string(),
                groups: default_groups(),
                sudoer: true,
            },
            network: NetworkConfig {
                backend: NetworkBackend::Iwd,
                dns: DnsProvider::DnscryptProxy,
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

        Ok(())
    }
}
