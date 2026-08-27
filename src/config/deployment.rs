//! Deployment configuration structure

use crate::disk::detection::list_block_devices;
use crate::utils::error::{DeploytixError, Result};
use crate::utils::prompt::*;
use crate::utils::secret::Secret;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
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
    pub encryption_password: Option<Secret>,
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
    /// Whether members of `wheel` must type a password for sudo.
    ///
    /// Defaults to `password`.  Deploytix <= 1.4.0 unconditionally granted
    /// `NOPASSWD: ALL` to the whole wheel group, which makes the account
    /// password worthless for privilege escalation; set `nopasswd` to keep
    /// that behaviour deliberately.
    #[serde(default)]
    pub sudo_policy: SudoPolicy,

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
    pub password: Secret,
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
    /// AUR GUI frontend used when `backend = "iwd"`. Ignored otherwise.
    #[serde(default)]
    pub iwd_frontend: IwdFrontend,
    /// Optional Wi-Fi network to pre-seed on the installed system so it has
    /// connectivity from the very first boot (required for Steam's first-run
    /// client bootstrap in the gamescope session, which happens before the
    /// OOBE network page exists). Written as a NetworkManager system
    /// connection or an iwd network file depending on `backend`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi_ssid: Option<String>,
    /// WPA-PSK passphrase for `wifi_ssid`. Omit for an open network.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi_password: Option<Secret>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesktopConfig {
    /// Desktop environment
    #[serde(default)]
    pub environment: DesktopEnvironment,
    /// Display manager (defaults to greetd auto-login; ignored when
    /// `environment = "none"`)
    #[serde(default)]
    pub display_manager: DisplayManager,
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
    /// Install grub-btrfs (bootable snapshot menu entries + grub-btrfsd
    /// daemon) and configure snapper for the root subvolume, including a
    /// top-level @snapshots subvolume mounted at /.snapshots.
    /// Requires: btrfs filesystem + subvolumes; incompatible with use_lvm_thin.
    #[serde(default)]
    pub install_grub_btrfs: bool,
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
    /// User-supplied extras collected by the post-install extras step
    /// (phase 5.95).  When set in a config-driven run, these install
    /// non-interactively at the end of phase 5.
    #[serde(default)]
    pub extra_packages: ExtraPackagesConfig,
}

/// User-supplied extras to install in phase 5.95 after the configured
/// selection has finished.  AUR entries require `install_yay = true`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExtraPackagesConfig {
    #[serde(default)]
    pub pacman: Vec<String>,
    #[serde(default)]
    pub aur: Vec<String>,
}

impl ExtraPackagesConfig {
    pub fn is_empty(&self) -> bool {
        self.pacman.is_empty() && self.aur.is_empty()
    }
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

/// Whether `name` is acceptable to `useradd` on a POSIX system.
///
/// Deliberately the portable subset (`^[a-z_][a-z0-9_-]*$`) rather than
/// everything `useradd --badname` would tolerate: the username is used to
/// build home paths, service files and ownership strings throughout the
/// installer.
fn is_valid_username(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Whether `host` is a valid RFC 1123 hostname label.
fn is_valid_hostname(host: &str) -> bool {
    if host.is_empty() || host.len() > 63 {
        return false;
    }
    if host.starts_with('-') || host.ends_with('-') {
        return false;
    }
    host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Whether `%wheel` needs a password for sudo on the installed system.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SudoPolicy {
    /// `%wheel ALL=(ALL:ALL) ALL` — sudo prompts for the user's password.
    #[default]
    Password,
    /// `%wheel ALL=(ALL:ALL) NOPASSWD: ALL` — passwordless root for every
    /// member of `wheel`.  Convenient for kiosk/handheld images; it means
    /// anyone who reaches a shell as that user is already root.
    NoPasswd,
}

impl SudoPolicy {
    /// The sudoers rule this policy expands to.
    pub fn sudoers_rule(&self) -> &'static str {
        match self {
            Self::Password => "%wheel ALL=(ALL:ALL) ALL",
            Self::NoPasswd => "%wheel ALL=(ALL:ALL) NOPASSWD: ALL",
        }
    }
}

impl std::fmt::Display for SudoPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Password => write!(f, "password required"),
            Self::NoPasswd => write!(f, "passwordless (NOPASSWD)"),
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
    ///
    /// For s6 this is the default bundle's contents directory.  Do not
    /// write to it directly: since Artix moved to the upstream s6-frontend,
    /// services are enabled with `s6 set enable <name>` and persisted with
    /// `s6 set commit` + `s6 live install --init` (see
    /// `configure::services::{enable_s6_service, commit_service_database}`).
    pub fn enabled_dir(&self) -> &str {
        match self {
            Self::Runit => "/run/runit/service",
            Self::OpenRC => "/etc/runlevels/default",
            Self::S6 => "/etc/s6/adminsv/default/contents.d",
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
    /// iwd paired with an AUR GUI frontend (iwgtk / iwdgui / iwqt).
    /// The specific frontend is selected via `NetworkConfig::iwd_frontend`.
    #[default]
    Iwd,
    /// NetworkManager with iwd as the wifi backend.
    NetworkManager,
    /// NetworkManager with wpa_supplicant as the wifi backend.
    #[serde(rename = "networkmanager-wpa")]
    NetworkManagerWpa,
}

impl std::fmt::Display for NetworkBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Iwd => write!(f, "iwd + GUI frontend (AUR)"),
            Self::NetworkManager => write!(f, "NetworkManager + iwd"),
            Self::NetworkManagerWpa => write!(f, "NetworkManager + wpa_supplicant"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IwdFrontend {
    #[default]
    Iwgtk,
    Iwdgui,
    Iwqt,
}

impl IwdFrontend {
    /// AUR package name for this frontend.
    pub fn aur_package(&self) -> &'static str {
        match self {
            Self::Iwgtk => "iwgtk",
            Self::Iwdgui => "iwdgui",
            Self::Iwqt => "iwqt",
        }
    }

    /// Desktop entry name (used to autostart the tray applet).
    pub fn desktop_entry(&self) -> &'static str {
        match self {
            Self::Iwgtk => "iwgtk",
            Self::Iwdgui => "iwdgui",
            Self::Iwqt => "iwqt",
        }
    }
}

impl std::fmt::Display for IwdFrontend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Iwgtk => write!(f, "iwgtk (GTK)"),
            Self::Iwdgui => write!(f, "iwdgui (GTK)"),
            Self::Iwqt => write!(f, "iwqt (Qt)"),
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

/// Display manager selection for desktop installs.
///
/// `Greetd` is the deploytix default and keeps the original behavior:
/// greetd auto-logins the created user straight into the desktop session
/// (no greeter). The other variants install a conventional display manager
/// with its normal login screen. `None` boots to a TTY login; the desktop
/// can be started manually via `startx` (~/.xinitrc is written per DE).
///
/// Ignored when `environment = "none"`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DisplayManager {
    #[default]
    Greetd,
    Sddm,
    Gdm,
    Lightdm,
    None,
}

impl DisplayManager {
    /// Service name as registered with the init system. This is also the
    /// base package name (the Artix service package is `{name}-{init}`).
    /// `None` for the TTY-login variant, which has no service.
    pub fn service_name(&self) -> Option<&'static str> {
        match self {
            Self::Greetd => Some("greetd"),
            Self::Sddm => Some("sddm"),
            Self::Gdm => Some("gdm"),
            Self::Lightdm => Some("lightdm"),
            Self::None => None,
        }
    }
}

impl std::fmt::Display for DisplayManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Greetd => write!(f, "greetd (auto-login, deploytix default)"),
            Self::Sddm => write!(f, "SDDM (login screen)"),
            Self::Gdm => write!(f, "GDM (login screen)"),
            Self::Lightdm => write!(f, "LightDM (login screen)"),
            Self::None => write!(f, "None (TTY login, startx)"),
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

    /// Serialise the config to TOML and write it to `path`, creating
    /// any missing parent directories.  Used by the post-install
    /// extras step to persist user-entered extras for later re-runs.
    /// A saved config can carry the LUKS passphrase, the account password
    /// and the Wi-Fi PSK in cleartext, so it is created 0600 rather than at
    /// the process umask.  `OpenOptions::mode` applies at creation time, so
    /// there is no window during which the file exists world-readable.  An
    /// existing file is re-permissioned explicitly, since `mode` is ignored
    /// when the file is merely truncated.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        use std::io::Write;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
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
        let backends = [
            NetworkBackend::Iwd,
            NetworkBackend::NetworkManager,
            NetworkBackend::NetworkManagerWpa,
        ];
        let net_idx = prompt_select("Network backend", &backends, 0)?;
        let mut backend = backends[net_idx].clone();
        // Sub-choice: AUR GUI frontend when iwd is the standalone backend.
        let iwd_frontend = if backend == NetworkBackend::Iwd {
            let frontends = [IwdFrontend::Iwgtk, IwdFrontend::Iwdgui, IwdFrontend::Iwqt];
            let f_idx = prompt_select("iwd GUI frontend (AUR)", &frontends, 0)?;
            frontends[f_idx]
        } else {
            IwdFrontend::default()
        };

        // Optional Wi-Fi pre-seeding so the installed system has connectivity
        // on first boot (needed for Steam's first-run bootstrap in Game Mode).
        let (wifi_ssid, wifi_password) = if prompt_confirm(
            "Pre-configure a Wi-Fi network on the installed system?",
            false,
        )? {
            let ssid = prompt_input("Wi-Fi SSID", None)?;
            let password = if prompt_confirm("Is the network password-protected (WPA-PSK)?", true)?
            {
                Some(prompt_password("Wi-Fi passphrase", true)?)
            } else {
                None
            };
            (Some(ssid), password)
        } else {
            (None, None)
        };

        // Desktop
        let desktops = [
            DesktopEnvironment::None,
            DesktopEnvironment::Kde,
            DesktopEnvironment::Gnome,
            DesktopEnvironment::Xfce,
        ];
        let de_idx = prompt_select("Desktop environment", &desktops, 0)?;
        let environment = desktops[de_idx].clone();

        // Display manager (only meaningful with a desktop environment)
        let mut display_manager = if environment != DesktopEnvironment::None {
            let dms = [
                DisplayManager::Greetd,
                DisplayManager::Sddm,
                DisplayManager::Gdm,
                DisplayManager::Lightdm,
                DisplayManager::None,
            ];
            let dm_idx = prompt_select("Display manager", &dms, 0)?;
            dms[dm_idx]
        } else {
            DisplayManager::None
        };

        // Swap type selection
        let swap_types = [SwapType::Partition, SwapType::FileZram, SwapType::ZramOnly];
        let swap_idx = prompt_select("Swap configuration", &swap_types, 0)?;
        let swap_type = swap_types[swap_idx].clone();

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

        // Steam's gamepad UI configures Wi-Fi through NetworkManager; the
        // standalone iwd backend would leave first-boot network setup broken
        // in Game Mode (and fail validation), so coerce it here.
        if install_session_switching && backend == NetworkBackend::Iwd {
            println!(
                "  Note: Game Mode session switching requires NetworkManager. \
                 Switching network backend to NetworkManager + iwd."
            );
            backend = NetworkBackend::NetworkManager;
        }

        // The gamescope ↔ desktop loop (session manager, IPC helper, PAM
        // files) is built on greetd, so coerce the display manager as well.
        if install_session_switching && display_manager != DisplayManager::Greetd {
            println!(
                "  Note: Game Mode session switching is driven through greetd. \
                 Switching display manager to greetd."
            );
            display_manager = DisplayManager::Greetd;
        }

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

        // grub-btrfs — bootable snapshot menu entries; needs btrfs subvolumes
        // and is out of scope for LVM thin (no subvolume-per-data layout there).
        let install_grub_btrfs =
            if filesystem == Filesystem::Btrfs && use_subvolumes && !use_lvm_thin {
                prompt_confirm(
                    "Install grub-btrfs? (bootable snapshot menu entries + snapper root config)",
                    false,
                )?
            } else {
                false
            };

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
                encryption_password: encryption_password.map(Secret::new),
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
                sudo_policy: SudoPolicy::default(),
                secureboot,
                secureboot_method,
                secureboot_keys_path: None,
            },
            user: UserConfig {
                name: username,
                password: Secret::new(password),
                groups: default_groups(),
                sudoer: true,
            },
            network: NetworkConfig {
                backend,
                iwd_frontend,
                wifi_ssid,
                wifi_password: wifi_password.map(Secret::new),
            },
            desktop: DesktopConfig {
                environment,
                display_manager,
            },
            packages: PackagesConfig {
                install_yay,
                install_wine,
                install_gaming,
                install_session_switching,
                install_btrfs_tools,
                install_grub_btrfs,
                sysctl_gaming_tweaks,
                sysctl_network_performance,
                install_hhd,
                install_decky_loader,
                install_evdevhook2,
                gpu_drivers,
                extra_packages: ExtraPackagesConfig::default(),
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
                sudo_policy: SudoPolicy::default(),
                secureboot: false,
                secureboot_method: SecureBootMethod::Sbctl,
                secureboot_keys_path: None,
            },
            user: UserConfig {
                name: "user".to_string(),
                password: Secret::new("changeme"),
                groups: default_groups(),
                sudoer: true,
            },
            network: NetworkConfig {
                backend: NetworkBackend::Iwd,
                iwd_frontend: IwdFrontend::default(),
                wifi_ssid: None,
                wifi_password: None,
            },
            desktop: DesktopConfig {
                environment: DesktopEnvironment::Kde,
                display_manager: DisplayManager::default(),
            },
            packages: PackagesConfig {
                // The iwd backend above has no in-repo GUI frontend, so
                // validate_rules() requires yay for it.  Without this the
                // sample emitted by `deploytix generate-config` fails
                // `deploytix validate` for a reason the user did not cause.
                install_yay: true,
                ..PackagesConfig::default()
            },
        }
    }

    /// Validate the configuration.
    ///
    /// Device checks first (they are the cheapest way to catch the most
    /// common mistake), then the pure cross-field rules.
    pub fn validate(&self) -> Result<()> {
        self.validate_device()?;
        self.validate_rules()
    }

    /// Check that `disk.device` exists and is a block device.
    ///
    /// Split out from [`Self::validate`] because it is the only part that
    /// touches the filesystem: keeping it separate is what lets
    /// [`Self::validate_rules`] be unit-tested without hardware.
    pub fn validate_device(&self) -> Result<()> {
        // Check device exists
        if !Path::new(&self.disk.device).exists() {
            return Err(DeploytixError::DeviceNotFound(self.disk.device.clone()));
        }

        // Check device is a block device
        let metadata = std::fs::metadata(&self.disk.device)?;
        if !metadata.file_type().is_block_device() {
            return Err(DeploytixError::NotBlockDevice(self.disk.device.clone()));
        }

        Ok(())
    }

    /// Every cross-field rule that does not touch the filesystem.
    ///
    /// Pure and hardware-independent, so each rule below can have a test.
    /// Rules are fail-fast: the first violation wins, and callers see one
    /// error at a time.
    pub fn validate_rules(&self) -> Result<()> {
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
        // Restrict to the POSIX portable username set. Beyond matching what
        // useradd will actually accept, this bounds what can appear in the
        // paths and service files built from the username elsewhere.
        if !is_valid_username(&self.user.name) {
            return Err(DeploytixError::ValidationError(format!(
                "Invalid username '{}': must start with a lowercase letter or \
                 underscore and contain only lowercase letters, digits, \
                 underscores or hyphens",
                self.user.name
            )));
        }

        // Validate password
        if self.user.password.is_empty() {
            return Err(DeploytixError::ValidationError(
                "Password cannot be empty".to_string(),
            ));
        }

        // sudoer grants sudo via a %wheel rule, so the account has to be in
        // wheel or the setting silently does nothing.
        if self.user.sudoer && !self.user.groups.iter().any(|g| g == "wheel") {
            return Err(DeploytixError::ValidationError(
                "user.sudoer = true grants sudo through the 'wheel' group, but \
                 'wheel' is not in user.groups — add it, or set sudoer = false"
                    .to_string(),
            ));
        }

        // Hostname must be a valid RFC 1123 label; an invalid one breaks
        // name resolution on the installed system.
        if !is_valid_hostname(&self.system.hostname) {
            return Err(DeploytixError::ValidationError(format!(
                "Invalid hostname '{}': must be 1-63 characters of letters, \
                 digits or hyphens, and must not start or end with a hyphen",
                self.system.hostname
            )));
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
            // The gamescope ↔ desktop loop (deploytix-session-manager, the
            // greetd-ipc helper, the PAM files, and the switch scripts'
            // `sv restart greetd`) is built on greetd.
            if self.desktop.display_manager != DisplayManager::Greetd {
                return Err(DeploytixError::ValidationError(
                    "Session switching requires display_manager = \"greetd\" \
                     (the Game Mode ↔ Desktop loop is driven through greetd IPC)"
                        .to_string(),
                ));
            }
            // Steam's gamepad UI (Deck OOBE network page, Settings > Internet)
            // configures Wi-Fi via NetworkManager over D-Bus; the standalone
            // iwd backend leaves it non-functional in the gamescope session.
            if self.network.backend == NetworkBackend::Iwd {
                return Err(DeploytixError::ValidationError(
                    "Session switching (gamescope Game Mode) requires a NetworkManager backend \
                     (backend = \"networkmanager\" or \"networkmanager-wpa\"); Steam's gamepad UI \
                     configures Wi-Fi through NetworkManager"
                        .to_string(),
                ));
            }
        }

        // Wi-Fi pre-seeding sanity checks
        if let Some(ssid) = &self.network.wifi_ssid {
            if ssid.is_empty() || ssid.len() > 32 {
                return Err(DeploytixError::ValidationError(
                    "wifi_ssid must be 1-32 characters".to_string(),
                ));
            }
            // The SSID is used as a filename on the target system.
            if ssid.contains('/') || ssid.chars().any(|c| c.is_control()) {
                return Err(DeploytixError::ValidationError(
                    "wifi_ssid must not contain '/' or control characters".to_string(),
                ));
            }
            if let Some(pw) = &self.network.wifi_password {
                if pw.len() < 8 || pw.len() > 63 {
                    return Err(DeploytixError::ValidationError(
                        "wifi_password must be a WPA-PSK passphrase of 8-63 characters".to_string(),
                    ));
                }
            }
        } else if self.network.wifi_password.is_some() {
            return Err(DeploytixError::ValidationError(
                "wifi_password is set but wifi_ssid is missing".to_string(),
            ));
        }

        // The standalone-iwd backend ships an AUR GUI frontend (iwgtk / iwdgui /
        // iwqt) that's only reachable via yay.  Without yay there's no way to
        // install the frontend, so refuse the combination at validation time.
        if self.network.backend == NetworkBackend::Iwd && !self.packages.install_yay {
            return Err(DeploytixError::ValidationError(format!(
                "Network backend 'iwd' requires install_yay = true (AUR package: {})",
                self.network.iwd_frontend.aur_package()
            )));
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

        // AUR extras require yay
        if !self.packages.extra_packages.aur.is_empty() && !self.packages.install_yay {
            return Err(DeploytixError::ValidationError(
                "extra_packages.aur is non-empty but install_yay = false".to_string(),
            ));
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

        // grub-btrfs requires btrfs + subvolumes; the LVM thin path does not
        // use btrfs subvolumes for data and is out of scope for snapshot boot.
        if self.packages.install_grub_btrfs {
            if self.disk.filesystem != Filesystem::Btrfs {
                return Err(DeploytixError::ValidationError(
                    "grub-btrfs requires btrfs filesystem".to_string(),
                ));
            }
            if !self.disk.use_subvolumes {
                return Err(DeploytixError::ValidationError(
                    "grub-btrfs requires use_subvolumes = true (snapshot boot entries need the @ subvolume layout)".to_string(),
                ));
            }
            if self.disk.use_lvm_thin {
                return Err(DeploytixError::ValidationError(
                    "grub-btrfs is not supported with use_lvm_thin = true".to_string(),
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Secret redaction ─────────────────────────────────────────────────────
    //
    // `DeploymentConfig` derives `Debug`, so a single `{:?}` anywhere in the
    // installer would otherwise dump every credential at once.  These guard
    // that the `Secret` wrapper actually covers the whole config.

    #[test]
    fn debug_of_config_never_leaks_secrets() {
        let mut cfg = DeploymentConfig::sample();
        cfg.disk.encryption = true;
        cfg.disk.encryption_password = Some(Secret::new("luks-passphrase-sentinel"));
        cfg.user.password = Secret::new("account-password-sentinel");
        cfg.network.wifi_ssid = Some("somenet".to_string());
        cfg.network.wifi_password = Some(Secret::new("wifi-psk-sentinel"));

        let rendered = format!("{:?}", cfg);

        assert!(!rendered.contains("luks-passphrase-sentinel"));
        assert!(!rendered.contains("account-password-sentinel"));
        assert!(!rendered.contains("wifi-psk-sentinel"));
        // Non-secret fields must still be visible, or Debug is useless.
        assert!(rendered.contains("somenet"));
    }

    #[test]
    fn secrets_still_round_trip_through_toml() {
        let mut cfg = DeploymentConfig::sample();
        cfg.disk.encryption = true;
        cfg.disk.encryption_password = Some(Secret::new("luks-passphrase"));
        cfg.user.password = Secret::new("account-password");

        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: DeploymentConfig = toml::from_str(&text).unwrap();

        assert_eq!(
            back.disk.encryption_password.as_deref(),
            Some("luks-passphrase")
        );
        assert_eq!(back.user.password.as_str(), "account-password");
    }

    #[test]
    fn save_to_creates_the_file_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("deploytix-savetest-{}", std::process::id()));
        let path = dir.join("cfg.toml");
        let cfg = DeploymentConfig::sample();

        cfg.save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "generated config must not be world-readable");

        // Overwriting an existing (deliberately loose) file must tighten it:
        // OpenOptions::mode is ignored when the file already exists.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        cfg.save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "re-saving must re-tighten an existing file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ═══════════════════════════════════════════════════════════════════
    // validate_rules() — one test per rule.
    //
    // These are reachable only because validate() was split into
    // validate_device() + validate_rules(): the device checks used to run
    // first, so every rule below was unreachable without a real disk.
    // ═══════════════════════════════════════════════════════════════════

    /// A config that satisfies every rule.  Each test perturbs exactly one
    /// thing, so a failure names the rule that fired.
    fn valid() -> DeploymentConfig {
        let mut c = DeploymentConfig::sample();
        c.network.backend = NetworkBackend::NetworkManager;
        c.packages.install_yay = false;
        c
    }

    /// Assert that `cfg` is rejected and the message mentions `needle`.
    #[track_caller]
    fn rejected_because(cfg: &DeploymentConfig, needle: &str) {
        match cfg.validate_rules() {
            Ok(()) => panic!(
                "expected rejection mentioning {:?}, but config passed",
                needle
            ),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.to_lowercase().contains(&needle.to_lowercase()),
                    "expected message mentioning {:?}, got: {}",
                    needle,
                    msg
                );
            }
        }
    }

    #[test]
    fn the_baseline_config_is_valid() {
        assert!(valid().validate_rules().is_ok());
    }

    #[test]
    fn sample_config_passes_every_rule() {
        // `deploytix generate-config` emits sample(); if it cannot pass its
        // own rules then `deploytix validate` on a freshly generated file
        // fails for reasons the user did not cause.
        if let Err(e) = DeploymentConfig::sample().validate_rules() {
            panic!(
                "DeploymentConfig::sample() violates a validation rule: {}",
                e
            );
        }
    }

    #[test]
    fn validate_rules_does_not_touch_the_filesystem() {
        // A device path that cannot exist must not affect the pure rules.
        let mut c = valid();
        c.disk.device = "/dev/deploytix-definitely-not-a-real-device".to_string();
        assert!(c.validate_rules().is_ok());
        // ...while validate() as a whole still rejects it.
        assert!(c.validate().is_err());
    }

    // ── user ─────────────────────────────────────────────────────────────

    #[test]
    fn rejects_empty_username() {
        let mut c = valid();
        c.user.name = String::new();
        rejected_because(&c, "Username cannot be empty");
    }

    #[test]
    fn rejects_username_with_spaces() {
        let mut c = valid();
        c.user.name = "two words".to_string();
        rejected_because(&c, "cannot contain spaces");
    }

    #[test]
    fn rejects_usernames_outside_the_posix_portable_set() {
        for bad in [
            "Tester",          // uppercase
            "1tester",         // leading digit
            "-tester",         // leading hyphen
            "tester;rm -rf /", // shell metacharacters
            "tes.ter",         // dot
            "tester$",         // dollar
        ] {
            let mut c = valid();
            c.user.name = bad.to_string();
            assert!(
                c.validate_rules().is_err(),
                "username {:?} should be rejected",
                bad
            );
        }
    }

    #[test]
    fn accepts_ordinary_usernames() {
        for good in ["tester", "_svc", "user1", "my-user", "a"] {
            let mut c = valid();
            c.user.name = good.to_string();
            assert!(
                c.validate_rules().is_ok(),
                "username {:?} should be accepted",
                good
            );
        }
    }

    #[test]
    fn rejects_empty_password() {
        let mut c = valid();
        c.user.password = Secret::new("");
        rejected_because(&c, "Password cannot be empty");
    }

    #[test]
    fn rejects_sudoer_without_the_wheel_group() {
        let mut c = valid();
        c.user.sudoer = true;
        c.user.groups = vec!["video".to_string(), "audio".to_string()];
        rejected_because(&c, "wheel");
    }

    #[test]
    fn allows_non_sudoer_without_wheel() {
        let mut c = valid();
        c.user.sudoer = false;
        c.user.groups = vec!["video".to_string()];
        assert!(c.validate_rules().is_ok());
    }

    // ── hostname ─────────────────────────────────────────────────────────

    #[test]
    fn rejects_invalid_hostnames() {
        for bad in [
            "",
            "-lead",
            "trail-",
            "has space",
            "under_score",
            &"x".repeat(64),
        ] {
            let mut c = valid();
            c.system.hostname = bad.to_string();
            assert!(
                c.validate_rules().is_err(),
                "hostname {:?} should be rejected",
                bad
            );
        }
    }

    #[test]
    fn accepts_valid_hostnames() {
        for good in ["artix", "my-box", "box1", "a", &"x".repeat(63)] {
            let mut c = valid();
            c.system.hostname = good.to_string();
            assert!(
                c.validate_rules().is_ok(),
                "hostname {:?} should be accepted",
                good
            );
        }
    }

    // ── encryption ───────────────────────────────────────────────────────

    #[test]
    fn rejects_encryption_without_a_passphrase() {
        let mut c = valid();
        c.disk.encryption = true;
        c.disk.encryption_password = None;
        rejected_because(&c, "Encryption password required");
    }

    #[test]
    fn rejects_integrity_without_encryption() {
        let mut c = valid();
        c.disk.encryption = false;
        c.disk.integrity = true;
        rejected_because(&c, "integrity");
    }

    #[test]
    fn rejects_boot_encryption_without_encryption() {
        let mut c = valid();
        c.disk.encryption = false;
        c.disk.boot_encryption = true;
        rejected_because(&c, "encryption");
    }

    #[test]
    fn rejects_boot_encryption_with_zfs_boot() {
        let mut c = valid();
        c.disk.encryption = true;
        c.disk.encryption_password = Some(Secret::new("pw"));
        c.disk.boot_encryption = true;
        c.disk.boot_filesystem = Filesystem::Zfs;
        rejected_because(&c, "zfs");
    }

    #[test]
    fn rejects_per_partition_encryption_without_global_encryption() {
        let mut c = valid();
        c.disk.encryption = false;
        c.disk.partitions[0].encryption = Some(true);
        assert!(c.validate_rules().is_err());
    }

    // ── filesystem / layout ──────────────────────────────────────────────

    #[test]
    fn rejects_subvolumes_on_non_btrfs() {
        let mut c = valid();
        c.disk.filesystem = Filesystem::Ext4;
        c.disk.use_subvolumes = true;
        rejected_because(&c, "btrfs");
    }

    #[test]
    fn rejects_lvm_thin_with_zfs() {
        let mut c = valid();
        c.disk.filesystem = Filesystem::Zfs;
        c.disk.use_subvolumes = false;
        c.disk.use_lvm_thin = true;
        assert!(c.validate_rules().is_err());
    }

    #[test]
    fn rejects_out_of_range_thin_pool_percent() {
        for bad in [0u8, 101, 255] {
            let mut c = valid();
            c.disk.lvm_thin_pool_percent = bad;
            assert!(
                c.validate_rules().is_err(),
                "thin pool percent {} should be rejected",
                bad
            );
        }
    }

    #[test]
    fn accepts_thin_pool_percent_at_the_boundaries() {
        for good in [1u8, 50, 100] {
            let mut c = valid();
            c.disk.lvm_thin_pool_percent = good;
            assert!(c.validate_rules().is_ok(), "percent {} should pass", good);
        }
    }

    #[test]
    fn rejects_swap_file_on_filesystems_that_cannot_host_one() {
        for fs in [Filesystem::Xfs, Filesystem::Zfs, Filesystem::F2fs] {
            let mut c = valid();
            c.disk.use_subvolumes = false;
            c.disk.filesystem = fs.clone();
            c.disk.swap_type = SwapType::FileZram;
            assert!(
                c.validate_rules().is_err(),
                "swap file on {:?} should be rejected",
                fs
            );
        }
    }

    #[test]
    fn accepts_swap_file_on_btrfs_and_ext4() {
        for fs in [Filesystem::Btrfs, Filesystem::Ext4] {
            let mut c = valid();
            c.disk.filesystem = fs.clone();
            c.disk.use_subvolumes = fs == Filesystem::Btrfs;
            c.disk.swap_type = SwapType::FileZram;
            assert!(
                c.validate_rules().is_ok(),
                "swap file on {:?} should be accepted",
                fs
            );
        }
    }

    // ── partitions ───────────────────────────────────────────────────────

    fn part(mount: &str, size: u64) -> CustomPartitionEntry {
        CustomPartitionEntry {
            mount_point: mount.to_string(),
            size_mib: size,
            label: None,
            encryption: None,
        }
    }

    #[test]
    fn rejects_an_empty_partition_list() {
        let mut c = valid();
        c.disk.partitions = vec![];
        rejected_because(&c, "at least one partition");
    }

    #[test]
    fn requires_exactly_one_root_partition() {
        let mut c = valid();
        c.disk.partitions = vec![part("/home", 0)];
        assert!(c.validate_rules().is_err(), "no root partition");

        c.disk.partitions = vec![part("/", 1024), part("/", 0)];
        assert!(c.validate_rules().is_err(), "two root partitions");
    }

    #[test]
    fn rejects_relative_mount_points() {
        let mut c = valid();
        c.disk.partitions = vec![part("/", 0), part("home", 1024)];
        assert!(c.validate_rules().is_err());
    }

    #[test]
    fn rejects_reserved_mount_points() {
        for reserved in ["/boot", "/boot/efi"] {
            let mut c = valid();
            c.disk.partitions = vec![part("/", 0), part(reserved, 1024)];
            assert!(
                c.validate_rules().is_err(),
                "{} is managed by the installer and must be rejected",
                reserved
            );
        }
    }

    #[test]
    fn rejects_duplicate_mount_points() {
        let mut c = valid();
        c.disk.partitions = vec![part("/", 0), part("/var", 1024), part("/var", 2048)];
        assert!(c.validate_rules().is_err());
    }

    #[test]
    fn rejects_more_than_one_remainder_partition() {
        let mut c = valid();
        c.disk.partitions = vec![part("/", 0), part("/home", 0)];
        assert!(c.validate_rules().is_err());
    }

    // ── package interdependencies ────────────────────────────────────────

    #[test]
    fn iwd_backend_requires_yay_for_its_gui_frontend() {
        let mut c = valid();
        c.network.backend = NetworkBackend::Iwd;
        c.packages.install_yay = false;
        rejected_because(&c, "yay");

        c.packages.install_yay = true;
        assert!(c.validate_rules().is_ok());
    }

    #[test]
    fn aur_only_packages_require_yay() {
        let cases: Vec<(&str, fn(&mut DeploymentConfig))> = vec![
            ("hhd", |c| c.packages.install_hhd = true),
            ("evdevhook2", |c| c.packages.install_evdevhook2 = true),
            ("aur extras", |c| {
                c.packages.extra_packages.aur = vec!["some-aur-pkg".to_string()]
            }),
        ];
        for (name, apply) in cases {
            let mut c = valid();
            apply(&mut c);
            c.packages.install_yay = false;
            assert!(
                c.validate_rules().is_err(),
                "{} without yay should fail",
                name
            );

            let mut c = valid();
            apply(&mut c);
            c.packages.install_yay = true;
            assert!(c.validate_rules().is_ok(), "{} with yay should pass", name);
        }
    }

    #[test]
    fn decky_loader_requires_both_gaming_and_yay() {
        let mut c = valid();
        c.packages.install_decky_loader = true;
        c.packages.install_gaming = false;
        c.packages.install_yay = true;
        assert!(c.validate_rules().is_err(), "decky without gaming");

        let mut c = valid();
        c.packages.install_decky_loader = true;
        c.packages.install_gaming = true;
        c.packages.install_yay = false;
        assert!(c.validate_rules().is_err(), "decky without yay");

        let mut c = valid();
        c.packages.install_decky_loader = true;
        c.packages.install_gaming = true;
        c.packages.install_yay = true;
        assert!(c.validate_rules().is_ok());
    }

    #[test]
    fn btrfs_tools_require_yay_and_a_btrfs_filesystem() {
        let mut c = valid();
        c.packages.install_btrfs_tools = true;
        c.packages.install_yay = false;
        assert!(c.validate_rules().is_err(), "btrfs tools without yay");

        let mut c = valid();
        c.packages.install_btrfs_tools = true;
        c.packages.install_yay = true;
        c.disk.filesystem = Filesystem::Ext4;
        c.disk.use_subvolumes = false;
        assert!(c.validate_rules().is_err(), "btrfs tools on ext4");
    }

    #[test]
    fn grub_btrfs_requires_btrfs_subvolumes_and_no_lvm_thin() {
        let mut c = valid();
        c.packages.install_grub_btrfs = true;
        c.disk.filesystem = Filesystem::Ext4;
        c.disk.use_subvolumes = false;
        assert!(c.validate_rules().is_err(), "grub-btrfs on ext4");

        let mut c = valid();
        c.packages.install_grub_btrfs = true;
        c.disk.use_subvolumes = false;
        assert!(c.validate_rules().is_err(), "grub-btrfs without subvolumes");

        let mut c = valid();
        c.packages.install_grub_btrfs = true;
        c.disk.use_lvm_thin = true;
        assert!(c.validate_rules().is_err(), "grub-btrfs with lvm thin");
    }

    #[test]
    fn session_switching_requires_gaming_and_a_desktop() {
        let mut c = valid();
        c.packages.install_session_switching = true;
        c.packages.install_gaming = false;
        assert!(
            c.validate_rules().is_err(),
            "session switching without gaming"
        );

        let mut c = valid();
        c.packages.install_session_switching = true;
        c.packages.install_gaming = true;
        c.desktop.environment = DesktopEnvironment::None;
        assert!(
            c.validate_rules().is_err(),
            "session switching without a DE"
        );
    }

    #[test]
    fn session_switching_requires_greetd_and_networkmanager() {
        let mut c = valid();
        c.packages.install_session_switching = true;
        c.packages.install_gaming = true;
        c.desktop.display_manager = DisplayManager::Sddm;
        assert!(
            c.validate_rules().is_err(),
            "session switching needs greetd"
        );

        let mut c = valid();
        c.packages.install_session_switching = true;
        c.packages.install_gaming = true;
        c.packages.install_yay = true;
        c.network.backend = NetworkBackend::Iwd;
        assert!(
            c.validate_rules().is_err(),
            "session switching needs NetworkManager for Steam's Wi-Fi UI"
        );
    }

    // ── secureboot ───────────────────────────────────────────────────────

    #[test]
    fn manual_secureboot_keys_require_a_path() {
        let mut c = valid();
        c.system.secureboot = true;
        c.system.secureboot_method = SecureBootMethod::ManualKeys;
        c.system.secureboot_keys_path = None;
        rejected_because(&c, "keys");

        c.system.secureboot_keys_path = Some("/etc/secureboot/keys".to_string());
        assert!(c.validate_rules().is_ok());
    }

    // ── wifi pre-seeding ─────────────────────────────────────────────────

    #[test]
    fn rejects_a_wifi_password_with_no_ssid() {
        let mut c = valid();
        c.network.wifi_ssid = None;
        c.network.wifi_password = Some(Secret::new("12345678"));
        assert!(c.validate_rules().is_err());
    }

    #[test]
    fn rejects_out_of_range_ssid_lengths() {
        for bad in ["", &"x".repeat(33)] {
            let mut c = valid();
            c.network.wifi_ssid = Some(bad.to_string());
            assert!(
                c.validate_rules().is_err(),
                "ssid {:?} should be rejected",
                bad
            );
        }
    }

    #[test]
    fn rejects_ssids_that_would_be_unsafe_as_filenames() {
        let mut c = valid();
        c.network.wifi_ssid = Some("net/work".to_string());
        assert!(c.validate_rules().is_err());
    }

    #[test]
    fn rejects_psk_lengths_outside_the_wpa_range() {
        for bad in ["short", &"x".repeat(64)] {
            let mut c = valid();
            c.network.wifi_ssid = Some("somenet".to_string());
            c.network.wifi_password = Some(Secret::new(bad));
            assert!(
                c.validate_rules().is_err(),
                "psk {:?} should be rejected",
                bad
            );
        }
    }

    #[test]
    fn accepts_a_well_formed_wifi_preseed() {
        let mut c = valid();
        c.network.wifi_ssid = Some("somenet".to_string());
        c.network.wifi_password = Some(Secret::new("correcthorse"));
        assert!(c.validate_rules().is_ok());
    }

    #[test]
    fn accepts_an_open_network_with_no_psk() {
        let mut c = valid();
        c.network.wifi_ssid = Some("somenet".to_string());
        c.network.wifi_password = None;
        assert!(c.validate_rules().is_ok());
    }

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
        assert_eq!(
            InitSystem::S6.enabled_dir(),
            "/etc/s6/adminsv/default/contents.d"
        );
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
