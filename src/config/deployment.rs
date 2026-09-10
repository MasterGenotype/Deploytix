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

/// Options for a recovery install: reinstall onto a disk while adopting
/// volumes that are already there rather than recreating them.
///
/// Everything here is opt-in and defaults off, so an ordinary install is
/// unaffected.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RecoveryConfig {
    /// Reuse the existing `/home` volume instead of recreating it.
    ///
    /// The HOME partition keeps its exact on-disk extent, its LUKS container
    /// is opened rather than reformatted, and its filesystem is not touched.
    #[serde(default)]
    pub reuse_home: bool,

    /// Path — **on the installer host**, not the target — to the keyfile
    /// that unlocks the existing HOME LUKS container.
    ///
    /// This is not the same thing as the per-volume keyfiles deploytix
    /// generates under `/etc/cryptsetup-keys.d` and bakes into the target's
    /// initramfs. This one is read once, to open the container being
    /// adopted; the target still gets its own freshly generated keyfile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_keyfile: Option<String>,

    /// Prompt for the HOME passphrase when no keyfile is given, or when the
    /// keyfile is rejected.
    #[serde(default = "default_true")]
    pub allow_passphrase_fallback: bool,
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

    /// User-defined data partitions (e.g. ROOT, HOME, USR, VAR).
    /// EFI + Boot are always auto-prepended; Swap is prepended when
    /// `swap_type == Partition`.
    #[serde(default = "default_partitions")]
    pub partitions: Vec<CustomPartitionEntry>,

    /// Recovery-install options (reusing an existing /home). Defaults to
    /// off, so an ordinary install behaves exactly as before.
    #[serde(default)]
    pub recovery: RecoveryConfig,
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
    pub wifi_password: Option<String>,
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
    /// Install Zen Browser (AUR: `zen-browser-bin`).
    ///
    /// Optional, and requires `install_yay = true` — it is an AUR package.
    /// It was previously installed unconditionally alongside yay, which meant
    /// every install with an AUR helper also got a browser it had not asked
    /// for.
    #[serde(default)]
    pub install_zen_browser: bool,
    /// Install Warp Terminal, the agentic terminal.
    ///
    /// Warp publishes an Arch/pacman package but is in no repository and has
    /// no AUR entry, so it is fetched from the vendor
    /// ([`WARP_TERMINAL_URL`]) and installed with `pacman -U`.
    #[serde(default)]
    pub install_warp_terminal: bool,
    /// Install the prebuilt linux-tkg kernel *instead of* `linux-zen`.
    ///
    /// Frogging-Family publishes ready-built Arch packages with every release,
    /// so the kernel does not have to be compiled on the target — the package
    /// and its headers are downloaded ([`TKG_RELEASES_API`]) and installed
    /// with `pacman -U`, the same trust decision as fetching them by hand.
    ///
    /// This *replaces* the stock kernel rather than sitting beside it, which
    /// means there is no fallback if the download fails: the install aborts
    /// instead of finishing with an unbootable disk. It also rules out ZFS
    /// (no `zfs-linux-tkg` module exists) and switches NVIDIA to `nvidia-dkms`,
    /// since the prebuilt `nvidia` package targets a stock kernel ABI.
    #[serde(default)]
    pub install_tkg_kernel: bool,
    /// CPU scheduler variant of the linux-tkg kernel. Ignored unless
    /// `install_tkg_kernel = true`.
    #[serde(default)]
    pub tkg_scheduler: TkgScheduler,
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
    /// Transactional immutable root with a read-only, integrity-checked OS and
    /// atomic updates via `deploytix update` (applied on reboot); direct
    /// `pacman -Syu` on the live system is blocked. Two backends, selected by the
    /// disk layout:
    ///  - **btrfs** (`install_grub_btrfs = true`): mount `/` and `/usr` read-only,
    ///    keep `/etc` on a writable `@etc` subvolume, and snapshot `{@, @usr, @etc}`
    ///    as an atomic set that rolls back together.
    ///  - **LVM thin** (`use_lvm_thin = true`): A/B dual-slot; each slot's root LV
    ///    (including `/usr`) is read-only and dm-verity protected. Updates build the
    ///    inactive slot and flip the boot pointer; `deploytix rollback` flips back.
    ///
    /// Requires: install_grub_btrfs = true OR use_lvm_thin = true.
    #[serde(default)]
    pub immutable_root: bool,
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
    /// Download the Steam client during installation instead of on first boot.
    ///
    /// The `steam` package ships only `bootstraplinux_ubuntu12_32.tar.xz`; the
    /// gamepad UI (`steamwebhelper`, `steamui.so`) arrives in the client Steam
    /// fetches on its first real run. Without this, that download happens on
    /// the target's first boot, which is the boot least likely to have a
    /// working network — and until it completes, Game Mode has no UI to draw.
    /// Running it here makes the deployed system boot straight into Game Mode.
    ///
    /// Costs a few hundred MB and several minutes of install time, and needs
    /// `xorg-server-xvfb` (added automatically) because Steam will not
    /// bootstrap without a display. Entirely best-effort: a failure here is
    /// logged and the first-boot path in `steam-gamescope-session` still
    /// handles it.
    ///
    /// Requires: install_gaming = true.
    #[serde(default)]
    pub steam_prefetch_client: bool,
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
    /// Ship udev quirks that stop the controllers on Lenovo Legion Go family
    /// handhelds (Legion Go, Legion Go 2, Legion Go S) disconnecting and
    /// reconnecting: pin USB runtime power management off for the pads, bind
    /// them to `xpad` on kernels that predate their IDs, and open their
    /// hidraw nodes to the session user.
    ///
    /// Unset (the default) means auto — the rules are written only when the
    /// installing host's DMI identifies it as one of those handhelds. Set
    /// `true` to force them on (deploying to removable media from another
    /// machine), `false` to suppress them.
    #[serde(default)]
    pub handheld_controller_quirks: Option<bool>,
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

/// Where Warp's Arch package is downloaded from.
///
/// `/download`, not `/get_warp`. The latter is the marketing landing page: it
/// answers `200 text/html` with an 11 KB SPA, so `curl -f` sees a success and
/// saves the page under the package's name, and only `pacman -U` notices --
/// as a corrupt archive, on a best-effort step that shrugs and carries on.
/// `/download?package=pacman` is the endpoint that redirects to the real
/// `releases.warp.dev/stable/v<ver>/warp-terminal-v<ver>-1-x86_64.pkg.tar.zst`.
///
/// The URL is a constant rather than user input, which is what makes it safe
/// to interpolate into the `curl` command in
/// [`crate::install::packages::warp_terminal_script`]. It is single-quoted
/// there, and `warp_url_is_safe_to_shell_quote` keeps it that way.
pub const WARP_TERMINAL_URL: &str = "https://app.warp.dev/download?package=pacman";

/// Where the prebuilt linux-tkg packages are looked up.
///
/// Frogging-Family publishes a set of prebuilt Arch packages with every
/// release — one `.pkg.tar.zst` per CPU scheduler, plus a matching `-headers-`
/// package. The asset names carry both the kernel series and the build number
/// (`linux72-tkg-bore-llvm-7.2.3-273-x86_64.pkg.tar.zst`), so there is no
/// stable "latest" URL to hardcode: every release changes all three.
///
/// So the URL is resolved from the releases API at install time and the pinned
/// pair below is only the fallback for when that lookup cannot be made — the
/// API is unauthenticated (60 requests/hour), and a rate-limited installer
/// should still produce a bootable system rather than none at all.
pub const TKG_RELEASES_API: &str =
    "https://api.github.com/repos/Frogging-Family/linux-tkg/releases/latest";

/// Release tag of the pinned fallback build.
pub const TKG_FALLBACK_TAG: &str = "v7.2.3";
/// Kernel-series prefix of the pinned fallback build (`linux72` = 7.2.x).
pub const TKG_FALLBACK_KVER: &str = "linux72";
/// `<version>-<pkgrel>` of the pinned fallback build.
pub const TKG_FALLBACK_BUILD: &str = "7.2.3-273";

/// The `(kernel, headers)` download URLs for the pinned fallback build.
///
/// These are built from constants and a closed enum — never from user input —
/// which is what keeps them safe to single-quote into the shell script in
/// [`crate::install::packages::tkg_kernel_script`].
pub fn tkg_fallback_urls(sched: TkgScheduler) -> (String, String) {
    let base = format!(
        "https://github.com/Frogging-Family/linux-tkg/releases/download/{TKG_FALLBACK_TAG}"
    );
    let stem = format!("{TKG_FALLBACK_KVER}-tkg-{}-llvm", sched.as_str());
    (
        format!("{base}/{stem}-{TKG_FALLBACK_BUILD}-x86_64.pkg.tar.zst"),
        format!("{base}/{stem}-headers-{TKG_FALLBACK_BUILD}-x86_64.pkg.tar.zst"),
    )
}

/// CPU scheduler variant of the prebuilt linux-tkg kernel.
///
/// Every variant is built with LLVM and published under the same naming
/// scheme, so the choice only ever substitutes one token into the asset name.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TkgScheduler {
    /// BMQ — BitMap Queue, Alfred Chen's minimal-latency scheduler.
    Bmq,
    /// BORE — Burst-Oriented Response Enhancer over EEVDF. The upstream
    /// default and the most widely used of the set.
    #[default]
    Bore,
    /// EEVDF — the mainline scheduler, TKG-patched.
    Eevdf,
    /// MuQSS — Con Kolivas' Multiple Queue Skiplist Scheduler.
    Muqss,
    /// PDS — Priority and Deadline based Skiplist.
    Pds,
}

impl TkgScheduler {
    /// The token as it appears in the release asset name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bmq => "bmq",
            Self::Bore => "bore",
            Self::Eevdf => "eevdf",
            Self::Muqss => "muqss",
            Self::Pds => "pds",
        }
    }

    /// Every variant, for the GUI dropdown and the CLI wizard prompt.
    pub fn all() -> &'static [Self] {
        &[Self::Bmq, Self::Bore, Self::Eevdf, Self::Muqss, Self::Pds]
    }
}

impl std::fmt::Display for TkgScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-llvm", self.as_str())
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

    /// Whether the immutable root uses the **btrfs** snapshot backend
    /// (read-only `/`+`/usr`, paired `{@,@usr,@etc}` snapshot sets).
    pub fn immutable_btrfs(&self) -> bool {
        self.packages.immutable_root && self.packages.install_grub_btrfs
    }

    /// Whether the immutable root uses the **LVM A/B dm-verity** backend
    /// (two read-only, integrity-checked root slots; atomic slot flips).
    pub fn immutable_lvm_ab(&self) -> bool {
        self.packages.immutable_root && self.disk.use_lvm_thin
    }

    /// Serialise the config to TOML and write it to `path`, creating
    /// any missing parent directories.  Used by the post-install
    /// extras step to persist user-entered extras for later re-runs.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
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

        // Hibernation. Asked here rather than with the other system options
        // because it is only answerable once swap is chosen: zram is RAM-backed,
        // so there is no image to resume from and nothing to ask about.
        let hibernation = if swap_type == SwapType::ZramOnly {
            println!("  Hibernation: unavailable with ZRAM-only swap (nothing survives power-off)");
            false
        } else {
            prompt_confirm("Enable hibernation (suspend to disk)?", false)?
        };

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
        let install_warp_terminal =
            prompt_confirm("Install Warp Terminal: The Agentic Terminal?", false)?;

        // Kernel.  Offered as a replacement for linux-zen, so the prompt says
        // so — this is not an additive extra like the others around it.
        let install_tkg_kernel = prompt_confirm(
            "Install the prebuilt linux-tkg kernel (replaces linux-zen)?",
            false,
        )?;
        let tkg_scheduler = if install_tkg_kernel {
            let scheds = TkgScheduler::all();
            let default = scheds
                .iter()
                .position(|s| *s == TkgScheduler::default())
                .unwrap_or(0);
            scheds[prompt_select("linux-tkg CPU scheduler", scheds, default)?]
        } else {
            TkgScheduler::default()
        };

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

        // Optional Wi-Fi pre-seeding so the installed system has connectivity
        // on first boot. Asked here, after Game Mode is decided, because it
        // matters far more there: Steam's gamepad UI is drawn by
        // steamwebhelper, which cannot start on a never-signed-in client with
        // no network — so the OOBE page that would let the user configure
        // Wi-Fi never appears. Default to yes when Game Mode is enabled.
        if install_session_switching {
            println!(
                "  Note: Game Mode's first boot needs network access before Steam \
                 can show its own network-setup page. Pre-seed Wi-Fi here unless \
                 this machine will be on ethernet at first boot."
            );
        }
        let (wifi_ssid, wifi_password) = if prompt_confirm(
            "Pre-configure a Wi-Fi network on the installed system?",
            install_session_switching,
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

        // yay AUR helper
        let install_yay = prompt_confirm("Install yay AUR helper? (built from source)", false)?;

        // AUR package, so only offered when there is a helper to build it.
        let install_zen_browser = if install_yay {
            prompt_confirm("Install Zen Browser? (AUR: zen-browser-bin)", false)?
        } else {
            false
        };

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

        // Transactional immutable root — btrfs backend builds on grub-btrfs
        // snapshots; LVM thin backend uses A/B dual-slot dm-verity roots.
        let immutable_root = if install_grub_btrfs {
            prompt_confirm(
                "Enable transactional immutable root? (read-only /usr + /, atomic paired snapshots, `deploytix update`)",
                false,
            )?
        } else if use_lvm_thin {
            prompt_confirm(
                "Enable transactional immutable root? (A/B dual-slot, dm-verity read-only /, `deploytix update`)",
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

        // Steam client prefetch — requires gaming packages (Steam)
        let steam_prefetch_client = if install_gaming {
            prompt_confirm(
                "Download the Steam client now? (a few hundred MB; otherwise \
                 the target downloads it on first boot before Game Mode works)",
                install_session_switching,
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

        // Handheld controller quirks — offered with the detected hardware as
        // the default, so a Legion Go family machine gets them by answering
        // through the wizard without knowing the flag exists.
        let detected_handheld = crate::configure::handheld_quirks::detect_host_model();
        let handheld_controller_quirks = Some(prompt_confirm(
            &match detected_handheld {
                Some(model) => format!(
                    "Apply handheld controller quirks? ({} detected — stops the \
                     controllers disconnecting/reconnecting)",
                    model.as_str()
                ),
                None => "Apply handheld controller quirks? (Lenovo Legion Go family — \
                         stops the controllers disconnecting/reconnecting)"
                    .to_string(),
            },
            detected_handheld.is_some(),
        )?);

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
                partitions,
                recovery: RecoveryConfig::default(),
            },
            system: SystemConfig {
                init,
                bootloader,
                timezone,
                locale,
                keymap,
                hostname,
                hibernation,
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
            network: NetworkConfig {
                backend,
                iwd_frontend,
                wifi_ssid,
                wifi_password,
            },
            desktop: DesktopConfig {
                environment,
                display_manager,
            },
            packages: PackagesConfig {
                install_yay,
                install_warp_terminal,
                install_tkg_kernel,
                tkg_scheduler,
                install_zen_browser,
                install_wine,
                install_gaming,
                install_session_switching,
                install_btrfs_tools,
                install_grub_btrfs,
                immutable_root,
                sysctl_gaming_tweaks,
                sysctl_network_performance,
                install_hhd,
                steam_prefetch_client,
                install_decky_loader,
                install_evdevhook2,
                handheld_controller_quirks,
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
                recovery: RecoveryConfig::default(),
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
                iwd_frontend: IwdFrontend::default(),
                wifi_ssid: None,
                wifi_password: None,
            },
            desktop: DesktopConfig {
                environment: DesktopEnvironment::Kde,
                display_manager: DisplayManager::default(),
            },
            packages: PackagesConfig::default(),
        }
    }

    /// Non-fatal configuration advisories.
    ///
    /// Separate from [`validate`](Self::validate), which only ever returns
    /// hard errors. These are conditions that produce a working install but
    /// a poor first boot, so they are surfaced rather than enforced.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        // Game Mode's first boot needs the machine to already be online.
        // Steam's gamepad UI is rendered by steamwebhelper, which cannot
        // initialise on a never-signed-in client with no network — so the
        // OOBE page that would let the user *configure* Wi-Fi never draws.
        // Pre-seeded credentials (or wired ethernet) break the deadlock.
        if self.packages.install_session_switching && self.network.wifi_ssid.is_none() {
            warnings.push(
                "Game Mode is enabled but no Wi-Fi network is pre-seeded \
                 (network.wifi_ssid is unset). Steam's gamepad UI needs network \
                 access on first boot before its own network-setup page can be \
                 shown, so a machine with no wired connection may reach Game Mode \
                 with no way to get online. Set network.wifi_ssid / \
                 network.wifi_password, or ensure ethernet is connected at first boot."
                    .to_string(),
            );
        }

        warnings
    }

    /// Validate the recovery-install options.
    ///
    /// Split out of [`validate`](Self::validate) so it is unit-testable:
    /// `validate` requires a real block device, these rules do not.
    fn validate_recovery(&self, partitions: &[CustomPartitionEntry]) -> Result<()> {
        // ── Recovery install (reusing an existing /home) ──────────────
        if self.disk.recovery.reuse_home {
            // There has to be a /home volume in the target layout for an
            // existing one to be adopted into.
            if !partitions.iter().any(|p| p.mount_point == "/home") {
                return Err(DeploytixError::ValidationError(
                    "recovery.reuse_home requires a /home entry in disk.partitions".to_string(),
                ));
            }

            // The LVM backends place home inside a volume group, so adopting
            // it means adopting an existing VG rather than a partition —
            // a different problem, deliberately out of scope.
            if self.disk.use_lvm_thin {
                return Err(DeploytixError::ValidationError(
                    "recovery.reuse_home is not supported with use_lvm_thin \
                 (home lives inside the volume group, not on its own partition)"
                        .to_string(),
                ));
            }

            // An encrypted home cannot be adopted without a way in.
            if self.disk.encryption
                && self.disk.recovery.home_keyfile.is_none()
                && !self.disk.recovery.allow_passphrase_fallback
            {
                return Err(DeploytixError::ValidationError(
                    "recovery.reuse_home on an encrypted disk needs \
                 recovery.home_keyfile or recovery.allow_passphrase_fallback"
                        .to_string(),
                ));
            }

            // Fail here rather than after the disk has been repartitioned.
            if let Some(keyfile) = &self.disk.recovery.home_keyfile {
                if !Path::new(keyfile).is_file() {
                    return Err(DeploytixError::ValidationError(format!(
                        "recovery.home_keyfile '{}' does not exist or is not a \
                     regular file on this host",
                        keyfile
                    )));
                }
            }
        } else if self.disk.recovery.home_keyfile.is_some() {
            return Err(DeploytixError::ValidationError(
                "recovery.home_keyfile is set but recovery.reuse_home is false".to_string(),
            ));
        }

        Ok(())
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

        if let Some(msg) = self.tkg_kernel_conflict() {
            return Err(DeploytixError::ValidationError(msg));
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

        self.validate_recovery(partitions)?;

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

        // Prefetching the Steam client is meaningless without Steam.
        if self.packages.steam_prefetch_client && !self.packages.install_gaming {
            return Err(DeploytixError::ValidationError(
                "steam_prefetch_client requires install_gaming = true".to_string(),
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

        self.validate_immutable_backend()?;
        self.validate_hibernation()?;

        Ok(())
    }

    /// Hibernation needs somewhere to put the image (device-independent, so
    /// unit-testable on its own). Called from [`Self::validate`].
    ///
    /// Only `zram_only` is rejected, and it has to be: zram is RAM-backed, so
    /// there is no device that survives the power-off and nothing to resume
    /// from. It is not a configuration that can be made to work, and a system
    /// that accepts hibernation and then cold-boots is worse than one that
    /// refuses up front.
    ///
    /// A swap partition and a swap file both work, on a mutable root and on
    /// either immutable backend: swap is never brought into LUKS or LVM, and
    /// on an immutable root the swap file is placed on `/var`
    /// (`configure::swap::swap_file_path`), which is writable and shared across
    /// snapshot sets rather than inside the read-only, snapshotted root.
    /// Why this config cannot have the linux-tkg kernel, if it cannot.
    ///
    /// Split out of [`Self::validate`] as a pure rule so it can be tested
    /// without a block device present — `validate` checks that the target
    /// device exists before it reaches any of the business rules.
    pub(crate) fn tkg_kernel_conflict(&self) -> Option<String> {
        if !self.packages.install_tkg_kernel {
            return None;
        }

        // The ZFS module is packaged per-kernel (`zfs-linux-zen`) and upstream
        // ships no `zfs-linux-tkg`.  Since the tkg kernel replaces linux-zen
        // rather than joining it, allowing this would leave the pool with no
        // module to import it — an unbootable system, discovered at first boot
        // instead of here.
        if self.disk.filesystem == Filesystem::Zfs || self.disk.boot_filesystem == Filesystem::Zfs {
            return Some(
                "the linux-tkg kernel is not supported with ZFS: it replaces linux-zen, and \
                 there is no zfs-linux-tkg module package to import the pool with"
                    .to_string(),
            );
        }

        None
    }

    pub(crate) fn validate_hibernation(&self) -> Result<()> {
        if self.system.hibernation && self.disk.swap_type == SwapType::ZramOnly {
            return Err(DeploytixError::ValidationError(
                "hibernation requires swap that outlives a power-off, and swap_type = \"zram_only\" is RAM-backed: there is no image to resume from. Use swap_type = \"partition\" or \"file_zram\"."
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// Backend rules for the transactional immutable root (device-independent, so
    /// unit-testable on its own). Called from [`Self::validate`].
    ///
    /// Two backends:
    ///  - **btrfs**: grub-btrfs snapshot machinery → requires `install_grub_btrfs`.
    ///  - **LVM thin**: A/B dual-slot dm-verity → requires `use_lvm_thin`.
    ///
    /// Exactly one must apply, and the LVM A/B backend is incompatible with the
    /// sbctl-SecureBoot standalone-GRUB mode (which embeds grub.cfg in a signed
    /// EFI binary the on-disk boot-pointer edit cannot reach).
    pub(crate) fn validate_immutable_backend(&self) -> Result<()> {
        if self.packages.immutable_root
            && !self.packages.install_grub_btrfs
            && !self.disk.use_lvm_thin
        {
            return Err(DeploytixError::ValidationError(
                "immutable_root requires either install_grub_btrfs = true (btrfs snapshot backend) or use_lvm_thin = true (LVM A/B dm-verity backend)".to_string(),
            ));
        }

        if self.immutable_lvm_ab()
            && self.system.secureboot
            && self.system.secureboot_method == SecureBootMethod::Sbctl
            && self.disk.encryption
        {
            return Err(DeploytixError::ValidationError(
                "immutable_root on LVM (A/B dm-verity) is not supported with sbctl SecureBoot on an encrypted disk: that mode embeds grub.cfg in a signed standalone EFI binary, so the A/B boot-pointer edit cannot take effect. Use a different SecureBoot method, disk encryption without sbctl, or the btrfs immutable backend.".to_string(),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── immutable backend selection + validation ─────────────────────────────

    /// A minimal LVM-immutable-A/B config that passes the earlier validation
    /// gates (network/user/encryption), so validate() reaches the A/B rules.
    fn lvm_ab_config() -> DeploymentConfig {
        let mut c = DeploymentConfig::sample();
        c.disk.use_lvm_thin = true;
        c.disk.use_subvolumes = false;
        c.packages.install_grub_btrfs = false;
        c.packages.immutable_root = true;
        c.packages.install_yay = true; // iwd backend needs yay
        c
    }

    #[test]
    fn backend_selectors_are_mutually_exclusive() {
        let ab = lvm_ab_config();
        assert!(ab.immutable_lvm_ab());
        assert!(!ab.immutable_btrfs());

        let mut btrfs = DeploymentConfig::sample();
        btrfs.disk.use_subvolumes = true;
        btrfs.packages.install_grub_btrfs = true;
        btrfs.packages.immutable_root = true;
        assert!(btrfs.immutable_btrfs());
        assert!(!btrfs.immutable_lvm_ab());
    }

    #[test]
    fn immutable_requires_a_backend() {
        let mut c = DeploymentConfig::sample();
        c.packages.immutable_root = true;
        c.packages.install_grub_btrfs = false;
        c.disk.use_lvm_thin = false;
        let err = c.validate_immutable_backend().unwrap_err().to_string();
        assert!(err.contains("immutable_root requires"), "got: {err}");
    }

    #[test]
    fn lvm_ab_backend_rules_pass() {
        assert!(lvm_ab_config().validate_immutable_backend().is_ok());
    }

    // ── hibernation ──────────────────────────────────────────────────────────

    /// zram is RAM-backed: the image would not survive the power-off it exists
    /// to survive. Better to refuse than to hand back a system that accepts the
    /// setting and cold-boots anyway.
    #[test]
    fn hibernation_onto_zram_only_is_refused() {
        let mut c = DeploymentConfig::sample();
        c.system.hibernation = true;
        c.disk.swap_type = SwapType::ZramOnly;
        let err = c.validate_hibernation().unwrap_err().to_string();
        assert!(err.contains("zram_only"), "got: {err}");
    }

    /// Both real backing stores are allowed, on a mutable root and on either
    /// immutable backend — the immutable case is the point of the exercise.
    #[test]
    fn hibernation_is_allowed_with_real_swap_including_immutable_roots() {
        for swap in [SwapType::Partition, SwapType::FileZram] {
            for immutable in [false, true] {
                let mut c = DeploymentConfig::sample();
                c.system.hibernation = true;
                c.disk.swap_type = swap.clone();
                c.packages.immutable_root = immutable;
                assert!(
                    c.validate_hibernation().is_ok(),
                    "{swap:?} + immutable_root={immutable} must be allowed"
                );
            }
        }
        // And the LVM A/B backend specifically.
        let mut ab = lvm_ab_config();
        ab.system.hibernation = true;
        ab.disk.swap_type = SwapType::Partition;
        assert!(ab.validate_hibernation().is_ok());
    }

    /// Hibernation off must not make zram_only invalid.
    #[test]
    fn zram_only_is_fine_when_nobody_hibernates() {
        let mut c = DeploymentConfig::sample();
        c.system.hibernation = false;
        c.disk.swap_type = SwapType::ZramOnly;
        assert!(c.validate_hibernation().is_ok());
    }

    #[test]
    fn lvm_ab_rejected_with_sbctl_secureboot_on_encrypted_disk() {
        let mut c = lvm_ab_config();
        c.disk.encryption = true;
        c.disk.encryption_password = Some("pw".into());
        c.system.secureboot = true;
        c.system.secureboot_method = SecureBootMethod::Sbctl;
        let err = c.validate_immutable_backend().unwrap_err().to_string();
        assert!(err.contains("sbctl SecureBoot"), "got: {err}");
    }

    #[test]
    fn lvm_ab_allowed_with_non_sbctl_secureboot() {
        let mut c = lvm_ab_config();
        c.disk.encryption = true;
        c.disk.encryption_password = Some("pw".into());
        c.system.secureboot = true;
        c.system.secureboot_method = SecureBootMethod::ManualKeys;
        // The A/B+sbctl rule must not trip for ManualKeys.
        assert!(c.validate_immutable_backend().is_ok());
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
    /// A ZFS root needs a module built for the running kernel, and there is
    /// no `zfs-linux-tkg`.  Because the tkg kernel replaces linux-zen instead
    /// of joining it, letting this through would produce a pool nothing can
    /// import — caught at first boot, with the disk already written.
    #[test]
    fn the_tkg_kernel_is_rejected_on_zfs() {
        for fs in [Filesystem::Zfs, Filesystem::Btrfs] {
            for boot_fs in [Filesystem::Zfs, Filesystem::Ext4] {
                let mut cfg = DeploymentConfig::sample();
                cfg.packages.install_tkg_kernel = true;
                cfg.disk.filesystem = fs.clone();
                cfg.disk.boot_filesystem = boot_fs.clone();

                let conflict = cfg.tkg_kernel_conflict();
                if fs == Filesystem::Zfs || boot_fs == Filesystem::Zfs {
                    let msg = conflict.unwrap_or_else(|| {
                        panic!("tkg + zfs ({fs:?}/{boot_fs:?}) must be rejected")
                    });
                    assert!(msg.contains("zfs-linux-tkg"), "{msg}");
                } else {
                    assert!(conflict.is_none(), "{conflict:?}");
                }
            }
        }
    }

    /// The rule only bites when the kernel is actually selected — ZFS on the
    /// stock kernel is a supported layout and must stay that way.
    #[test]
    fn zfs_is_fine_without_the_tkg_kernel() {
        let mut cfg = DeploymentConfig::sample();
        cfg.packages.install_tkg_kernel = false;
        cfg.disk.filesystem = Filesystem::Zfs;
        assert!(cfg.tkg_kernel_conflict().is_none());
    }

    /// Game Mode's first boot needs the machine already online; flag the
    /// config that would reach it with no way to connect.
    #[test]
    fn game_mode_without_preseeded_wifi_warns() {
        let mut cfg = DeploymentConfig::sample();
        cfg.packages.install_session_switching = true;
        cfg.network.wifi_ssid = None;
        assert!(cfg
            .warnings()
            .iter()
            .any(|w| w.contains("no Wi-Fi network is pre-seeded")));
    }

    #[test]
    fn game_mode_with_preseeded_wifi_is_quiet() {
        let mut cfg = DeploymentConfig::sample();
        cfg.packages.install_session_switching = true;
        cfg.network.wifi_ssid = Some("home".to_string());
        assert!(cfg.warnings().is_empty());
    }

    #[test]
    fn no_game_mode_needs_no_wifi_warning() {
        let mut cfg = DeploymentConfig::sample();
        cfg.packages.install_session_switching = false;
        cfg.network.wifi_ssid = None;
        assert!(cfg.warnings().is_empty());
    }

    /// The fstab skip leaves `/`, `/usr` and `/etc` out so the initramfs can
    /// mount a booted snapshot set without `mount -a` shadowing it. That is
    /// specific to btrfs snapshot booting: on any other install those three
    /// must be listed as normal, or nothing mounts them.
    #[test]
    fn the_fstab_skip_applies_only_to_the_btrfs_immutable_backend() {
        let mut cfg = DeploymentConfig::sample();

        cfg.packages.immutable_root = false;
        cfg.packages.install_grub_btrfs = false;
        assert!(!cfg.immutable_btrfs(), "a plain install lists all three");

        cfg.packages.install_grub_btrfs = true;
        assert!(!cfg.immutable_btrfs(), "btrfs alone is not immutable");

        cfg.packages.immutable_root = true;
        cfg.packages.install_grub_btrfs = false;
        cfg.disk.use_lvm_thin = true;
        assert!(!cfg.immutable_btrfs(), "the A/B backend has its own fstab");
        assert!(cfg.immutable_lvm_ab());

        cfg.disk.use_lvm_thin = false;
        cfg.packages.install_grub_btrfs = true;
        assert!(
            cfg.immutable_btrfs(),
            "both options together, and only then"
        );
    }

    // ── Recovery install validation ──────────────────────────────────────

    fn recovery_config() -> DeploymentConfig {
        let mut cfg = DeploymentConfig::sample();
        cfg.disk.encryption = true;
        cfg.disk.recovery.reuse_home = true;
        cfg.disk.recovery.allow_passphrase_fallback = true;
        cfg.disk.recovery.home_keyfile = None;
        cfg
    }

    #[test]
    fn recovery_accepts_a_layout_with_a_home_partition() {
        let cfg = recovery_config();
        assert!(cfg.validate_recovery(&cfg.disk.partitions).is_ok());
    }

    #[test]
    fn recovery_requires_a_home_partition_to_adopt_into() {
        let mut cfg = recovery_config();
        cfg.disk.partitions.retain(|p| p.mount_point != "/home");
        let err = cfg
            .validate_recovery(&cfg.disk.partitions)
            .unwrap_err()
            .to_string();
        assert!(err.contains("/home entry"), "unexpected error: {}", err);
    }

    /// The LVM backends put home inside the volume group, so there is no
    /// partition to preserve — a different problem, deliberately refused.
    #[test]
    fn recovery_is_refused_for_lvm_thin() {
        let mut cfg = recovery_config();
        cfg.disk.use_lvm_thin = true;
        let err = cfg
            .validate_recovery(&cfg.disk.partitions)
            .unwrap_err()
            .to_string();
        assert!(err.contains("use_lvm_thin"), "unexpected error: {}", err);
    }

    /// An encrypted home cannot be adopted with no way to open it.
    #[test]
    fn recovery_on_an_encrypted_disk_needs_a_credential() {
        let mut cfg = recovery_config();
        cfg.disk.recovery.allow_passphrase_fallback = false;
        let err = cfg
            .validate_recovery(&cfg.disk.partitions)
            .unwrap_err()
            .to_string();
        assert!(err.contains("home_keyfile"), "unexpected error: {}", err);
    }

    /// Catch an unreadable keyfile now, not after the disk is repartitioned.
    #[test]
    fn a_missing_keyfile_is_rejected_at_validation_time() {
        let mut cfg = recovery_config();
        cfg.disk.recovery.home_keyfile = Some("/nonexistent/home.key".to_string());
        let err = cfg
            .validate_recovery(&cfg.disk.partitions)
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not exist"), "unexpected error: {}", err);
    }

    /// A keyfile without reuse_home means the disk gets erased despite the
    /// user clearly intending to keep something. Refuse rather than proceed.
    #[test]
    fn a_keyfile_without_reuse_home_is_refused() {
        let mut cfg = recovery_config();
        cfg.disk.recovery.reuse_home = false;
        cfg.disk.recovery.home_keyfile = Some("/tmp/good.key".to_string());
        let err = cfg
            .validate_recovery(&cfg.disk.partitions)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("reuse_home is false"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn an_ordinary_config_passes_recovery_validation_untouched() {
        let cfg = DeploymentConfig::sample();
        assert!(!cfg.disk.recovery.reuse_home);
        assert!(cfg.validate_recovery(&cfg.disk.partitions).is_ok());
    }
}
