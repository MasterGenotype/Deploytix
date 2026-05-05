//! Application state types for the Deploytix GUI

use crate::config::{
    Bootloader, CustomPartitionEntry, DesktopEnvironment, Filesystem, InitSystem, NetworkBackend,
    SecureBootMethod, SwapType,
};
use crate::disk::detection::BlockDevice;
use std::sync::mpsc::Receiver;

// ── Wizard navigation ──────────────────────────────────────────────────

/// Wizard steps in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    Configure,
    Summary,
    Installing,
}

impl WizardStep {
    pub const ALL: [Self; 3] = [Self::Configure, Self::Summary, Self::Installing];

    pub fn next(self) -> Option<Self> {
        match self {
            Self::Configure => Some(Self::Summary),
            Self::Summary => Some(Self::Installing),
            Self::Installing => None,
        }
    }

    pub fn prev(self) -> Option<Self> {
        match self {
            Self::Configure => None,
            Self::Summary => Some(Self::Configure),
            Self::Installing => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Configure => "Configure",
            Self::Summary => "Review",
            Self::Installing => "Install",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }
}

// ── Inter-thread messages ──────────────────────────────────────────────

/// Messages sent from the installation thread to the GUI.
pub enum InstallMessage {
    Status(String),
    Progress(f32),
    Log(String),
    Finished,
    Error(String),
    /// Preflight results from the dry-run verification thread.
    PreflightResults {
        lines: Vec<crate::preflight::report::PreflightLine>,
        has_failures: bool,
    },
    /// Rehearsal results from the full rehearsal installation thread.
    RehearsalResults {
        lines: Vec<crate::rehearsal::RehearsalLogLine>,
        has_failures: bool,
    },
}

// ── State sub-structs ──────────────────────────────────────────────────

/// Disk selection and configuration state.
pub struct DiskState {
    // Selection
    pub devices: Vec<BlockDevice>,
    pub selected_device_index: Option<usize>,
    pub refreshing: bool,

    // Filesystem
    pub filesystem: Filesystem,

    // Encryption
    pub encryption: bool,
    pub encryption_password: String,
    pub boot_encryption: bool,
    pub integrity: bool,

    // Swap
    pub swap_type: SwapType,

    // Btrfs
    pub use_subvolumes: bool,
    pub preserve_home: bool,

    // LVM thin provisioning
    pub use_lvm_thin: bool,
    pub lvm_vg_name: String,
    pub lvm_thin_pool_name: String,
    pub lvm_thin_pool_percent: u8,

    // Partition table
    pub partitions: Vec<CustomPartitionEntry>,

    // New-partition form fields
    pub new_partition_mount: String,
    pub new_partition_size: String,
    pub new_partition_label: String,
}

impl Default for DiskState {
    fn default() -> Self {
        Self {
            devices: Vec::new(),
            selected_device_index: None,
            refreshing: true,
            filesystem: Filesystem::Btrfs,
            encryption: false,
            encryption_password: String::new(),
            boot_encryption: false,
            integrity: false,
            swap_type: SwapType::Partition,
            use_subvolumes: false,
            preserve_home: false,
            use_lvm_thin: false,
            lvm_vg_name: "vg0".to_string(),
            lvm_thin_pool_name: "thinpool".to_string(),
            lvm_thin_pool_percent: 95,
            partitions: crate::config::default_partitions(),
            new_partition_mount: String::new(),
            new_partition_size: String::new(),
            new_partition_label: String::new(),
        }
    }
}

impl DiskState {
    /// Size in MiB of the selected device, or 0 if none selected.
    pub fn selected_disk_size_mib(&self) -> u64 {
        self.selected_device_index
            .and_then(|i| self.devices.get(i))
            .map(|d| d.size_mib())
            .unwrap_or(0)
    }

    /// Device path of the selected device, or `"(none)"`.
    pub fn selected_device_path(&self) -> &str {
        self.selected_device_index
            .and_then(|i| self.devices.get(i))
            .map(|d| d.path.as_str())
            .unwrap_or("(none)")
    }
}

/// System configuration state.
pub struct SystemState {
    pub init_system: InitSystem,
    pub bootloader: Bootloader,
    pub timezone: String,
    pub locale: String,
    pub keymap: String,
    pub hostname: String,
    pub secureboot: bool,
    pub secureboot_method: SecureBootMethod,
}

impl Default for SystemState {
    fn default() -> Self {
        Self {
            init_system: InitSystem::Runit,
            bootloader: Bootloader::Grub,
            timezone: "UTC".to_string(),
            locale: "en_US.UTF-8".to_string(),
            keymap: "us".to_string(),
            hostname: "artix".to_string(),
            secureboot: false,
            secureboot_method: SecureBootMethod::Sbctl,
        }
    }
}

/// User account configuration state.
pub struct UserState {
    pub username: String,
    pub password: String,
    pub password_confirm: String,
    pub sudoer: bool,
}

impl Default for UserState {
    fn default() -> Self {
        Self {
            username: String::new(),
            password: String::new(),
            password_confirm: String::new(),
            sudoer: true,
        }
    }
}

/// Package and desktop configuration state.
pub struct PackagesState {
    pub network_backend: NetworkBackend,
    pub desktop_env: DesktopEnvironment,
    pub install_yay: bool,
    pub install_wine: bool,
    pub install_gaming: bool,
    pub install_session_switching: bool,
    pub install_btrfs_tools: bool,
    pub install_modular: bool,
    pub sysctl_gaming_tweaks: bool,
    pub sysctl_network_performance: bool,
    pub install_hhd: bool,
    pub install_decky_loader: bool,
    pub install_evdevhook2: bool,
    pub gpu_nvidia: bool,
    pub gpu_amd: bool,
    pub gpu_intel: bool,
}

impl Default for PackagesState {
    fn default() -> Self {
        Self {
            network_backend: NetworkBackend::Iwd,
            desktop_env: DesktopEnvironment::None,
            install_yay: false,
            install_wine: false,
            install_gaming: false,
            install_session_switching: false,
            install_btrfs_tools: false,
            install_modular: false,
            sysctl_gaming_tweaks: false,
            sysctl_network_performance: false,
            install_hhd: false,
            install_decky_loader: false,
            install_evdevhook2: false,
            gpu_nvidia: false,
            gpu_amd: false,
            gpu_intel: false,
        }
    }
}

/// Installation progress and summary state.
pub struct InstallState {
    // Summary / pre-install
    pub dry_run: bool,
    pub confirmed: bool,
    pub save_config_path: String,
    pub save_config_status: Option<(String, bool)>,
    pub save_requested: bool,

    // Preflight dry-run
    pub preflight_running: bool,
    pub preflight_results: Option<Vec<crate::preflight::report::PreflightLine>>,
    pub preflight_has_failures: bool,
    pub preflight_requested: bool,

    // Rehearsal
    pub rehearsal_running: bool,
    pub rehearsal_results: Option<Vec<crate::rehearsal::RehearsalLogLine>>,
    pub rehearsal_has_failures: bool,
    pub rehearsal_requested: bool,

    // Progress
    pub status: String,
    pub progress: f32,
    pub logs: Vec<String>,
    pub finished: bool,
    pub error: Option<String>,
    pub receiver: Option<Receiver<InstallMessage>>,
}

impl Default for InstallState {
    fn default() -> Self {
        Self {
            dry_run: false,
            confirmed: false,
            save_config_path: "deploytix.toml".to_string(),
            save_config_status: None,
            save_requested: false,
            preflight_running: false,
            preflight_results: None,
            preflight_has_failures: false,
            preflight_requested: false,
            rehearsal_running: false,
            rehearsal_results: None,
            rehearsal_has_failures: false,
            rehearsal_requested: false,
            status: String::new(),
            progress: 0.0,
            logs: Vec::new(),
            finished: false,
            error: None,
            receiver: None,
        }
    }
}
