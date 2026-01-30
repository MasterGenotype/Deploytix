# Linux Distribution Classification and Installation Abstractions

This document details the commonalities across Linux distribution installation processes, package management systems, and provides a classification schema for detecting and adapting to host OS distributions.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Distribution Family Taxonomy](#2-distribution-family-taxonomy)
3. [Installation Channel Abstractions](#3-installation-channel-abstractions)
4. [Package Manager Unification](#4-package-manager-unification)
5. [Init System Classification](#5-init-system-classification)
6. [Filesystem and Boot Conventions](#6-filesystem-and-boot-conventions)
7. [Detection Mechanisms](#7-detection-mechanisms)
8. [Classification Parameter Schema](#8-classification-parameter-schema)
9. [Adaptation Patterns](#9-adaptation-patterns)
10. [Implementation Reference](#10-implementation-reference)

---

## 1. Overview

### 1.1 Purpose

Linux distributions, despite their diversity, share fundamental installation patterns. This document establishes:

- **Common abstractions** across installation methods
- **Classification parameters** for runtime detection
- **Interchangeable value sets** for cross-distribution compatibility
- **Adaptation strategies** for host OS detection

### 1.2 Scope

| In Scope | Out of Scope |
|----------|--------------|
| Major distribution families | Embedded Linux (OpenWrt, Yocto) |
| Standard package managers | Source-based (Gentoo, LFS) |
| Common init systems | Container-only distributions |
| UEFI and BIOS boot | Architecture-specific quirks |

---

## 2. Distribution Family Taxonomy

### 2.1 Primary Distribution Families

```
Linux Distributions
├── Debian Family
│   ├── Debian (upstream)
│   ├── Ubuntu
│   │   ├── Linux Mint
│   │   ├── Pop!_OS
│   │   └── Elementary OS
│   ├── Devuan (systemd-free)
│   └── Kali Linux
│
├── Red Hat Family
│   ├── RHEL (upstream)
│   ├── Fedora
│   ├── CentOS / Rocky / Alma
│   └── Oracle Linux
│
├── Arch Family
│   ├── Arch Linux (upstream)
│   ├── Artix Linux (systemd-free)
│   ├── Manjaro
│   ├── EndeavourOS
│   └── Garuda Linux
│
├── SUSE Family
│   ├── openSUSE Tumbleweed
│   ├── openSUSE Leap
│   └── SLES
│
├── Void Family
│   └── Void Linux (xbps, runit/musl)
│
├── Alpine Family
│   └── Alpine Linux (apk, musl, OpenRC)
│
└── Independent
    ├── Slackware
    ├── NixOS
    └── Guix
```

### 2.2 Family Characteristics Matrix

| Family | Package Format | Package Manager | Default Init | C Library | Release Model |
|--------|---------------|-----------------|--------------|-----------|---------------|
| Debian | .deb | apt/dpkg | systemd | glibc | Point release |
| Red Hat | .rpm | dnf/yum | systemd | glibc | Point release |
| Arch | .pkg.tar.zst | pacman | systemd | glibc | Rolling |
| SUSE | .rpm | zypper | systemd | glibc | Both |
| Void | .xbps | xbps | runit | glibc/musl | Rolling |
| Alpine | .apk | apk | OpenRC | musl | Point release |

### 2.3 Derivative Relationship Depth

Understanding derivative depth helps predict compatibility:

```
Depth 0: Root distributions (Debian, Arch, Void, Alpine)
Depth 1: Direct derivatives (Ubuntu, Artix, Manjaro)
Depth 2: Secondary derivatives (Mint, Pop!_OS, EndeavourOS)
Depth 3+: Tertiary derivatives (LMDE, Peppermint)
```

**Rule:** Higher depth = more potential divergence from upstream tooling.

---

## 3. Installation Channel Abstractions

### 3.1 Installation Methods Overview

| Method | Description | Use Case | Examples |
|--------|-------------|----------|----------|
| **Bootstrap** | Minimal rootfs extraction | Automated deployment | debootstrap, pacstrap, basestrap |
| **ISO Install** | Graphical/TUI installer | End-user installation | Calamares, Anaconda |
| **Rootfs Tarball** | Pre-built filesystem archive | Containers, WSL, chroot | stage3 (Gentoo), rootfs.tar.xz |
| **Network Install** | Minimal boot + network packages | Server deployment | netinst, PXE boot |
| **Image Clone** | Block-level disk image | Rapid deployment | dd, Clonezilla |

### 3.2 Bootstrap Tool Equivalents

All bootstrap tools perform the same fundamental operations:

```
┌─────────────────────────────────────────────────────────────┐
│                    Bootstrap Sequence                        │
├─────────────────────────────────────────────────────────────┤
│ 1. Create target directory structure                         │
│ 2. Download/extract base packages                            │
│ 3. Configure package manager for target                      │
│ 4. Install essential packages                                │
│ 5. Generate initial configuration (fstab, locale, etc.)      │
│ 6. Prepare for chroot entry                                  │
└─────────────────────────────────────────────────────────────┘
```

**Tool Mapping:**

| Distribution | Bootstrap Tool | Base Package Set | Mirror Config |
|--------------|---------------|------------------|---------------|
| Debian/Ubuntu | `debootstrap` | `base-files`, `apt` | `/etc/apt/sources.list` |
| Arch | `pacstrap` | `base`, `linux` | `/etc/pacman.d/mirrorlist` |
| Artix | `basestrap` | `base`, `linux`, `$init` | `/etc/pacman.d/mirrorlist` |
| Fedora | `dnf --installroot` | `@core` | `/etc/yum.repos.d/` |
| Void | `xbps-install -R` | `base-system` | `/etc/xbps.d/` |
| Alpine | `apk --root` | `alpine-base` | `/etc/apk/repositories` |
| openSUSE | `zypper --root` | `patterns-base-minimal_base` | `/etc/zypp/repos.d/` |

### 3.3 Unified Bootstrap Abstraction

```rust
/// Abstract bootstrap operation
pub trait BootstrapProvider {
    /// Install base system to target root
    fn install_base(&self, target: &Path, packages: &[String]) -> Result<()>;

    /// Configure package manager mirrors
    fn configure_mirrors(&self, target: &Path, mirrors: &[String]) -> Result<()>;

    /// Get default base package set
    fn default_packages(&self) -> Vec<String>;

    /// Execute command in chroot context
    fn chroot_exec(&self, target: &Path, cmd: &str) -> Result<()>;
}
```

### 3.4 Rootfs Tarball Sources

| Distribution | Rootfs URL Pattern | Compression | Verification |
|--------------|-------------------|-------------|--------------|
| Arch | `archlinux.org/iso/latest/archlinux-bootstrap-*.tar.zst` | zstd | PGP sig |
| Alpine | `alpinelinux.org/releases/*/releases/*/alpine-minirootfs-*.tar.gz` | gzip | SHA256 |
| Void | `repo-default.voidlinux.org/live/current/void-*-ROOTFS-*.tar.xz` | xz | SHA256 |
| Ubuntu | `cdimage.ubuntu.com/ubuntu-base/releases/*/release/ubuntu-base-*-base-*.tar.gz` | gzip | SHA256 |
| Fedora | `kojipkgs.fedoraproject.org/packages/Fedora-Container-Base/` | xz | Checksum |

### 3.5 ISO Installation Phases

Standard ISO installers follow this phase model:

```
Phase 1: Environment Setup
├── Boot live environment
├── Detect hardware
├── Load drivers
└── Start installer service

Phase 2: Configuration
├── Language/locale selection
├── Keyboard layout
├── Network configuration
├── Timezone selection
└── User account setup

Phase 3: Storage
├── Disk detection
├── Partition scheme selection
├── Filesystem creation
├── Encryption setup (optional)
└── Mount point assignment

Phase 4: Installation
├── Package selection
├── Base system installation
├── Bootloader installation
├── Initial configuration
└── Post-install hooks

Phase 5: Finalization
├── Generate fstab
├── Set root password
├── Create user accounts
├── Enable services
└── Unmount and reboot
```

---

## 4. Package Manager Unification

### 4.1 Operation Equivalence Matrix

| Operation | apt | pacman | dnf | xbps | apk | zypper |
|-----------|-----|--------|-----|------|-----|--------|
| Update index | `apt update` | `pacman -Sy` | `dnf check-update` | `xbps-install -S` | `apk update` | `zypper ref` |
| Upgrade all | `apt upgrade` | `pacman -Su` | `dnf upgrade` | `xbps-install -u` | `apk upgrade` | `zypper up` |
| Install pkg | `apt install` | `pacman -S` | `dnf install` | `xbps-install` | `apk add` | `zypper in` |
| Remove pkg | `apt remove` | `pacman -R` | `dnf remove` | `xbps-remove` | `apk del` | `zypper rm` |
| Search | `apt search` | `pacman -Ss` | `dnf search` | `xbps-query -Rs` | `apk search` | `zypper se` |
| Info | `apt show` | `pacman -Si` | `dnf info` | `xbps-query -R` | `apk info` | `zypper if` |
| List files | `dpkg -L` | `pacman -Ql` | `rpm -ql` | `xbps-query -f` | `apk info -L` | `rpm -ql` |
| Owner of file | `dpkg -S` | `pacman -Qo` | `rpm -qf` | `xbps-query -o` | `apk info -W` | `rpm -qf` |
| Clean cache | `apt clean` | `pacman -Sc` | `dnf clean all` | `xbps-remove -O` | `apk cache clean` | `zypper clean` |
| List installed | `dpkg -l` | `pacman -Q` | `dnf list installed` | `xbps-query -l` | `apk info` | `zypper se -i` |

### 4.2 Package Manager Abstraction Layer

```rust
/// Unified package manager interface
pub trait PackageManager {
    /// Get the package manager identifier
    fn id(&self) -> &'static str;

    /// Synchronize package database
    fn sync(&self, cmd: &CommandRunner) -> Result<()>;

    /// Install packages
    fn install(&self, cmd: &CommandRunner, packages: &[&str]) -> Result<()>;

    /// Remove packages
    fn remove(&self, cmd: &CommandRunner, packages: &[&str]) -> Result<()>;

    /// Check if package is installed
    fn is_installed(&self, cmd: &CommandRunner, package: &str) -> Result<bool>;

    /// Get package version
    fn package_version(&self, cmd: &CommandRunner, package: &str) -> Result<String>;

    /// Install to alternate root (for bootstrap)
    fn install_to_root(&self, cmd: &CommandRunner, root: &Path, packages: &[&str]) -> Result<()>;
}
```

### 4.3 Package Name Mapping

Packages have different names across distributions:

| Function | Debian | Arch | Fedora | Void | Alpine |
|----------|--------|------|--------|------|--------|
| Kernel | `linux-image-*` | `linux` | `kernel` | `linux` | `linux-lts` |
| Firmware | `linux-firmware` | `linux-firmware` | `linux-firmware` | `linux-firmware` | `linux-firmware` |
| GRUB (EFI) | `grub-efi-amd64` | `grub` | `grub2-efi-x64` | `grub-x86_64-efi` | `grub-efi` |
| NetworkManager | `network-manager` | `networkmanager` | `NetworkManager` | `NetworkManager` | `networkmanager` |
| Wireless | `iwd` | `iwd` | `iwd` | `iwd` | `iwd` |
| Cryptsetup | `cryptsetup` | `cryptsetup` | `cryptsetup` | `cryptsetup` | `cryptsetup` |
| Btrfs tools | `btrfs-progs` | `btrfs-progs` | `btrfs-progs` | `btrfs-progs` | `btrfs-progs` |
| Sudo | `sudo` | `sudo` | `sudo` | `sudo` | `sudo` |
| Text editor | `nano` | `nano` | `nano` | `nano` | `nano` |
| Compression | `zstd` | `zstd` | `zstd` | `zstd` | `zstd` |

### 4.4 Package Group/Pattern Equivalents

| Concept | Debian | Arch | Fedora | Void | Alpine |
|---------|--------|------|--------|------|--------|
| Minimal base | `--variant=minbase` | `base` | `@core` | `base-system` | `alpine-base` |
| Desktop (GNOME) | `gnome` | `gnome` | `@gnome-desktop` | `gnome` | `gnome` |
| Desktop (KDE) | `kde-plasma-desktop` | `plasma` | `@kde-desktop` | `kde5` | `plasma` |
| Development | `build-essential` | `base-devel` | `@development-tools` | `base-devel` | `build-base` |
| X.org | `xorg` | `xorg` | `@base-x` | `xorg` | `xorg-server` |
| Fonts | `fonts-*` | `ttf-*`, `noto-fonts` | `*-fonts` | `*-fonts-ttf` | `font-*` |

### 4.5 Repository Configuration

| Distribution | Config Location | Format | Mirror Variable |
|--------------|-----------------|--------|-----------------|
| Debian | `/etc/apt/sources.list.d/` | `deb URL dist components` | Direct URL |
| Arch | `/etc/pacman.d/mirrorlist` | `Server = URL` | `$repo`, `$arch` |
| Fedora | `/etc/yum.repos.d/*.repo` | INI with `baseurl=` | `$releasever`, `$basearch` |
| Void | `/etc/xbps.d/*.conf` | `repository=URL` | `$arch` |
| Alpine | `/etc/apk/repositories` | Plain URL list | `$version`, `$arch` |
| openSUSE | `/etc/zypp/repos.d/*.repo` | INI with `baseurl=` | `$releasever` |

---

## 5. Init System Classification

### 5.1 Init System Overview

| Init System | Type | Service Format | Boot Target | Distribution Usage |
|-------------|------|----------------|-------------|-------------------|
| systemd | Monolithic | Unit files | `*.target` | Debian, Fedora, Arch, SUSE |
| OpenRC | Modular | Shell scripts | Runlevels | Alpine, Gentoo, Artix |
| runit | Minimal | Directories | N/A | Void, Artix |
| s6 | Supervision | Execline scripts | N/A | Artix, custom |
| dinit | Hybrid | INI-like | Boot service | Artix, Chimera |
| SysVinit | Traditional | Shell scripts | Runlevels | Devuan, older distros |

### 5.2 Service Management Equivalents

| Operation | systemd | OpenRC | runit | s6 | dinit |
|-----------|---------|--------|-------|-----|-------|
| Start | `systemctl start X` | `rc-service X start` | `sv up X` | `s6-svc -u /run/s6/...` | `dinitctl start X` |
| Stop | `systemctl stop X` | `rc-service X stop` | `sv down X` | `s6-svc -d /run/s6/...` | `dinitctl stop X` |
| Enable | `systemctl enable X` | `rc-update add X` | `ln -s /etc/sv/X /var/service/` | Compile DB | `dinitctl enable X` |
| Disable | `systemctl disable X` | `rc-update del X` | `rm /var/service/X` | Compile DB | `dinitctl disable X` |
| Status | `systemctl status X` | `rc-service X status` | `sv status X` | `s6-svstat` | `dinitctl status X` |
| List | `systemctl list-units` | `rc-status` | `ls /var/service/` | `s6-rc -a list` | `dinitctl list` |

### 5.3 Init System Abstraction

```rust
/// Unified init system interface
pub trait InitSystem {
    fn id(&self) -> &'static str;

    /// Path to available services
    fn service_dir(&self) -> &Path;

    /// Path to enabled services
    fn enabled_dir(&self) -> &Path;

    /// Enable a service
    fn enable_service(&self, cmd: &CommandRunner, root: &Path, service: &str) -> Result<()>;

    /// Disable a service
    fn disable_service(&self, cmd: &CommandRunner, root: &Path, service: &str) -> Result<()>;

    /// Get required base packages for this init system
    fn base_packages(&self) -> Vec<&'static str>;
}
```

### 5.4 Service Name Variations

| Service | systemd | OpenRC | runit | Notes |
|---------|---------|--------|-------|-------|
| Networking | `systemd-networkd` | `networking` | `dhcpcd` | Varies greatly |
| Cron | `cronie.service` | `cronie` | `cronie` | or `fcron`, `dcron` |
| SSH | `sshd.service` | `sshd` | `sshd` | Consistent |
| Time sync | `systemd-timesyncd` | `ntpd` / `chronyd` | `ntpd` | Different daemons |
| Logging | `systemd-journald` | `syslog-ng` | `socklog` | Architecture differs |
| Device mgmt | `systemd-udevd` | `udev` | `udevd` | Same udev usually |

---

## 6. Filesystem and Boot Conventions

### 6.1 Directory Hierarchy Variations

While FHS provides the standard, distributions have variations:

| Path | Standard | Variation | Distribution |
|------|----------|-----------|--------------|
| `/bin`, `/sbin` | Separate | Symlink to `/usr/bin` | Arch, Fedora |
| `/lib`, `/lib64` | Separate | Symlink to `/usr/lib` | Arch, Fedora |
| `/etc/os-release` | Present | Always present | All modern |
| `/etc/lsb-release` | Optional | Debian-family | Ubuntu, Mint |
| `/usr/lib/os-release` | Fallback | systemd distros | Fedora, Arch |

### 6.2 Boot Configuration Locations

| Bootloader | Config Location | Entry Location |
|------------|-----------------|----------------|
| GRUB | `/etc/default/grub` | `/boot/grub/grub.cfg` |
| systemd-boot | `/boot/loader/loader.conf` | `/boot/loader/entries/*.conf` |
| rEFInd | `/boot/EFI/refind/refind.conf` | Auto-detect |
| EFISTUB | N/A (kernel params) | NVRAM |
| LILO | `/etc/lilo.conf` | N/A |
| Syslinux | `/boot/syslinux/syslinux.cfg` | N/A |

### 6.3 Kernel and Initramfs Paths

| Distribution | Kernel Path | Initramfs Path | Naming Convention |
|--------------|-------------|----------------|-------------------|
| Debian | `/boot/vmlinuz-*` | `/boot/initrd.img-*` | Version suffix |
| Arch | `/boot/vmlinuz-linux` | `/boot/initramfs-linux.img` | Variant suffix |
| Fedora | `/boot/vmlinuz-*` | `/boot/initramfs-*.img` | Version suffix |
| Void | `/boot/vmlinuz-*` | `/boot/initramfs-*.img` | Version suffix |
| Alpine | `/boot/vmlinuz-*` | `/boot/initramfs-*` | Variant suffix |

### 6.4 Initramfs Generation Tools

| Tool | Distribution | Config File | Regenerate Command |
|------|--------------|-------------|-------------------|
| mkinitcpio | Arch, Artix | `/etc/mkinitcpio.conf` | `mkinitcpio -P` |
| dracut | Fedora, SUSE, Void | `/etc/dracut.conf.d/` | `dracut --force` |
| initramfs-tools | Debian, Ubuntu | `/etc/initramfs-tools/` | `update-initramfs -u` |
| mkinitfs | Alpine | `/etc/mkinitfs/mkinitfs.conf` | `mkinitfs` |

### 6.5 Fstab UUID vs Label vs Path

| Method | Format | Reliability | Distribution Preference |
|--------|--------|-------------|------------------------|
| UUID | `UUID=abc-123` | Highest | Debian, Fedora, Ubuntu |
| PARTUUID | `PARTUUID=abc-123` | High | GPT systems |
| Label | `LABEL=root` | Medium | openSUSE |
| Path | `/dev/sda1` | Low | Legacy only |

---

## 7. Detection Mechanisms

### 7.1 Primary Detection: os-release

The `/etc/os-release` file (or `/usr/lib/os-release`) is the standard detection method:

```bash
# /etc/os-release fields
ID=artix                    # Lowercase identifier
ID_LIKE=arch                # Parent distribution(s)
NAME="Artix Linux"          # Human-readable name
VERSION_ID=2024.01.01       # Version (optional for rolling)
PRETTY_NAME="Artix Linux"   # Display name
HOME_URL="https://..."      # Project URL
```

**Key Fields:**

| Field | Purpose | Example Values |
|-------|---------|----------------|
| `ID` | Primary identifier | `debian`, `ubuntu`, `arch`, `fedora`, `void`, `alpine` |
| `ID_LIKE` | Parent/compatible distros | `arch`, `debian`, `rhel fedora` |
| `VERSION_ID` | Numeric version | `22.04`, `39`, `3.18` |
| `VERSION_CODENAME` | Release codename | `jammy`, `bookworm` |

### 7.2 Secondary Detection Methods

When `os-release` is insufficient:

```rust
/// Detection priority order
pub enum DetectionMethod {
    OsRelease,           // /etc/os-release (preferred)
    LsbRelease,          // /etc/lsb-release (Debian family)
    DistroSpecific,      // /etc/debian_version, /etc/arch-release, etc.
    PackageManager,      // Detect by available package manager
    InitSystem,          // Detect by running init
    FilePresence,        // /etc/alpine-release, /etc/void-release
}
```

**Distribution-Specific Files:**

| File | Distribution |
|------|--------------|
| `/etc/debian_version` | Debian and derivatives |
| `/etc/arch-release` | Arch Linux |
| `/etc/artix-release` | Artix Linux |
| `/etc/fedora-release` | Fedora |
| `/etc/redhat-release` | RHEL and derivatives |
| `/etc/alpine-release` | Alpine |
| `/etc/void-release` | Void Linux |
| `/etc/SuSE-release` | openSUSE (legacy) |

### 7.3 Package Manager Detection

```rust
/// Detect package manager by binary presence
pub fn detect_package_manager() -> Option<PackageManagerType> {
    let checks = [
        ("/usr/bin/apt", PackageManagerType::Apt),
        ("/usr/bin/pacman", PackageManagerType::Pacman),
        ("/usr/bin/dnf", PackageManagerType::Dnf),
        ("/usr/bin/xbps-install", PackageManagerType::Xbps),
        ("/sbin/apk", PackageManagerType::Apk),
        ("/usr/bin/zypper", PackageManagerType::Zypper),
        ("/usr/bin/yum", PackageManagerType::Yum),
    ];

    for (path, pm_type) in checks {
        if Path::new(path).exists() {
            return Some(pm_type);
        }
    }
    None
}
```

### 7.4 Init System Detection

```rust
/// Detect running init system
pub fn detect_init_system() -> Option<InitSystemType> {
    // Check /proc/1/comm for init process name
    if let Ok(init_name) = std::fs::read_to_string("/proc/1/comm") {
        return match init_name.trim() {
            "systemd" => Some(InitSystemType::Systemd),
            "init" => detect_init_by_filesystem(),  // Could be SysV, OpenRC, etc.
            "runit" => Some(InitSystemType::Runit),
            "s6-svscan" => Some(InitSystemType::S6),
            "dinit" => Some(InitSystemType::Dinit),
            _ => None,
        };
    }
    None
}

fn detect_init_by_filesystem() -> Option<InitSystemType> {
    if Path::new("/run/openrc").exists() {
        Some(InitSystemType::OpenRC)
    } else if Path::new("/run/runit").exists() {
        Some(InitSystemType::Runit)
    } else if Path::new("/etc/inittab").exists() {
        Some(InitSystemType::SysVinit)
    } else {
        None
    }
}
```

### 7.5 Architecture Detection

```rust
pub fn detect_architecture() -> Architecture {
    #[cfg(target_arch = "x86_64")]
    return Architecture::X86_64;

    #[cfg(target_arch = "aarch64")]
    return Architecture::Aarch64;

    #[cfg(target_arch = "x86")]
    return Architecture::I686;

    #[cfg(target_arch = "arm")]
    return Architecture::Arm;
}
```

---

## 8. Classification Parameter Schema

### 8.1 Distribution Profile Structure

```rust
/// Complete distribution profile
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributionProfile {
    /// Primary identification
    pub identity: DistroIdentity,

    /// Package management configuration
    pub package_manager: PackageManagerProfile,

    /// Init system configuration
    pub init_system: InitSystemProfile,

    /// Filesystem conventions
    pub filesystem: FilesystemProfile,

    /// Boot configuration
    pub boot: BootProfile,

    /// Feature capabilities
    pub capabilities: CapabilitySet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistroIdentity {
    /// Primary ID (from os-release)
    pub id: String,

    /// Parent distributions
    pub id_like: Vec<String>,

    /// Distribution family
    pub family: DistroFamily,

    /// Human readable name
    pub name: String,

    /// Version (if applicable)
    pub version: Option<String>,

    /// Release model
    pub release_model: ReleaseModel,

    /// C library
    pub libc: LibC,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DistroFamily {
    Debian,
    RedHat,
    Arch,
    Suse,
    Void,
    Alpine,
    Slackware,
    Independent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReleaseModel {
    Rolling,
    PointRelease,
    LongTermSupport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LibC {
    Glibc,
    Musl,
}
```

### 8.2 Package Manager Profile

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageManagerProfile {
    /// Package manager type
    pub manager_type: PackageManagerType,

    /// Package format
    pub package_format: PackageFormat,

    /// Binary paths
    pub binaries: PackageManagerBinaries,

    /// Configuration paths
    pub config_paths: PackageManagerPaths,

    /// Command templates
    pub commands: PackageManagerCommands,

    /// Package name mappings
    pub package_map: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PackageManagerType {
    Apt,
    Pacman,
    Dnf,
    Yum,
    Xbps,
    Apk,
    Zypper,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PackageFormat {
    Deb,
    Rpm,
    PkgTarZst,
    Xbps,
    Apk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageManagerCommands {
    pub sync: String,           // "pacman -Sy"
    pub install: String,        // "pacman -S --noconfirm {packages}"
    pub remove: String,         // "pacman -R {packages}"
    pub upgrade: String,        // "pacman -Su"
    pub search: String,         // "pacman -Ss {query}"
    pub bootstrap: String,      // "pacstrap {root} {packages}"
}
```

### 8.3 Init System Profile

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitSystemProfile {
    /// Init system type
    pub init_type: InitSystemType,

    /// Service directory
    pub service_dir: PathBuf,

    /// Enabled services location
    pub enabled_dir: PathBuf,

    /// Service management commands
    pub commands: InitSystemCommands,

    /// Required packages
    pub packages: Vec<String>,

    /// Service name mappings
    pub service_map: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InitSystemType {
    Systemd,
    OpenRC,
    Runit,
    S6,
    Dinit,
    SysVinit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitSystemCommands {
    pub enable: String,         // "systemctl enable {service}"
    pub disable: String,        // "systemctl disable {service}"
    pub start: String,          // "systemctl start {service}"
    pub stop: String,           // "systemctl stop {service}"
    pub status: String,         // "systemctl status {service}"
}
```

### 8.4 Boot Profile

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootProfile {
    /// Supported bootloaders
    pub bootloaders: Vec<BootloaderType>,

    /// Default bootloader
    pub default_bootloader: BootloaderType,

    /// Initramfs tool
    pub initramfs_tool: InitramfsTool,

    /// Kernel path pattern
    pub kernel_path: String,

    /// Initramfs path pattern
    pub initramfs_path: String,

    /// Bootloader configuration paths
    pub bootloader_config: HashMap<BootloaderType, PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InitramfsTool {
    Mkinitcpio,
    Dracut,
    InitramfsTools,
    Mkinitfs,
}
```

### 8.5 Capability Set

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// Supports UEFI boot
    pub uefi: bool,

    /// Supports BIOS boot
    pub bios: bool,

    /// Supports Secure Boot
    pub secure_boot: bool,

    /// Supports full disk encryption
    pub fde: bool,

    /// Supports Btrfs
    pub btrfs: bool,

    /// Supports ZFS
    pub zfs: bool,

    /// Supports LVM
    pub lvm: bool,

    /// Supports RAID
    pub raid: bool,

    /// Supports containers (built-in)
    pub containers: bool,

    /// Supports Flatpak
    pub flatpak: bool,

    /// Supports Snap
    pub snap: bool,
}
```

---

## 9. Adaptation Patterns

### 9.1 Profile Resolution Chain

```rust
impl DistributionProfile {
    /// Resolve profile from detected distribution
    pub fn resolve(detected: &DetectedDistro) -> Result<Self> {
        // 1. Try exact match
        if let Some(profile) = BUILTIN_PROFILES.get(&detected.id) {
            return Ok(profile.clone());
        }

        // 2. Try ID_LIKE inheritance
        for parent_id in &detected.id_like {
            if let Some(parent_profile) = BUILTIN_PROFILES.get(parent_id) {
                return Ok(Self::derive_from(parent_profile, detected));
            }
        }

        // 3. Infer from detected capabilities
        Ok(Self::infer_from_detection(detected))
    }

    /// Derive profile from parent with overrides
    fn derive_from(parent: &Self, detected: &DetectedDistro) -> Self {
        let mut profile = parent.clone();
        profile.identity.id = detected.id.clone();
        profile.identity.name = detected.name.clone();
        // Apply any known overrides for this derivative
        profile
    }
}
```

### 9.2 Package Name Resolution

```rust
impl PackageManagerProfile {
    /// Resolve canonical package name to distribution-specific name
    pub fn resolve_package(&self, canonical: &str) -> String {
        self.package_map
            .get(canonical)
            .cloned()
            .unwrap_or_else(|| canonical.to_string())
    }

    /// Resolve multiple packages
    pub fn resolve_packages(&self, packages: &[&str]) -> Vec<String> {
        packages.iter()
            .map(|p| self.resolve_package(p))
            .collect()
    }
}

// Example package map
lazy_static! {
    static ref DEBIAN_PACKAGE_MAP: HashMap<String, String> = {
        let mut m = HashMap::new();
        m.insert("kernel".into(), "linux-image-amd64".into());
        m.insert("grub-efi".into(), "grub-efi-amd64".into());
        m.insert("networkmanager".into(), "network-manager".into());
        m
    };
}
```

### 9.3 Service Name Resolution

```rust
impl InitSystemProfile {
    /// Resolve canonical service name to init-specific name
    pub fn resolve_service(&self, canonical: &str) -> String {
        self.service_map
            .get(canonical)
            .cloned()
            .unwrap_or_else(|| canonical.to_string())
    }
}

// Example: Different service names for same function
// Canonical: "network"
// systemd: "systemd-networkd" or "NetworkManager"
// OpenRC: "net.eth0" or "NetworkManager"
// runit: "dhcpcd" or "NetworkManager"
```

### 9.4 Command Template Expansion

```rust
impl PackageManagerCommands {
    /// Expand command template with variables
    pub fn expand(&self, template: &str, vars: &HashMap<&str, &str>) -> String {
        let mut result = template.to_string();
        for (key, value) in vars {
            result = result.replace(&format!("{{{}}}", key), value);
        }
        result
    }
}

// Usage:
// Template: "pacstrap {root} {packages}"
// Vars: {"root": "/mnt", "packages": "base linux"}
// Result: "pacstrap /mnt base linux"
```

### 9.5 Fallback Strategies

```rust
/// Strategy for handling unsupported features
pub enum FallbackStrategy {
    /// Fail with error
    Error,

    /// Skip and continue
    Skip,

    /// Use alternative implementation
    Alternative(Box<dyn Fn() -> Result<()>>),

    /// Prompt user for decision
    Prompt,
}

impl CapabilitySet {
    /// Check capability with fallback
    pub fn require(&self, cap: Capability, fallback: FallbackStrategy) -> Result<()> {
        if self.has(cap) {
            return Ok(());
        }

        match fallback {
            FallbackStrategy::Error => {
                Err(DeploytixError::UnsupportedCapability(cap))
            }
            FallbackStrategy::Skip => {
                warn!("Skipping unsupported capability: {:?}", cap);
                Ok(())
            }
            FallbackStrategy::Alternative(f) => f(),
            FallbackStrategy::Prompt => {
                // Interactive decision
                todo!()
            }
        }
    }
}
```

---

## 10. Implementation Reference

### 10.1 Built-in Distribution Profiles

```rust
// src/distro/profiles.rs

lazy_static! {
    pub static ref BUILTIN_PROFILES: HashMap<String, DistributionProfile> = {
        let mut m = HashMap::new();

        m.insert("arch".into(), DistributionProfile {
            identity: DistroIdentity {
                id: "arch".into(),
                id_like: vec![],
                family: DistroFamily::Arch,
                name: "Arch Linux".into(),
                version: None,
                release_model: ReleaseModel::Rolling,
                libc: LibC::Glibc,
            },
            package_manager: PackageManagerProfile {
                manager_type: PackageManagerType::Pacman,
                package_format: PackageFormat::PkgTarZst,
                commands: PackageManagerCommands {
                    sync: "pacman -Sy".into(),
                    install: "pacman -S --noconfirm {packages}".into(),
                    remove: "pacman -R {packages}".into(),
                    upgrade: "pacman -Su --noconfirm".into(),
                    search: "pacman -Ss {query}".into(),
                    bootstrap: "pacstrap {root} {packages}".into(),
                },
                // ...
            },
            init_system: InitSystemProfile {
                init_type: InitSystemType::Systemd,
                service_dir: "/usr/lib/systemd/system".into(),
                enabled_dir: "/etc/systemd/system".into(),
                commands: InitSystemCommands {
                    enable: "systemctl enable {service}".into(),
                    disable: "systemctl disable {service}".into(),
                    start: "systemctl start {service}".into(),
                    stop: "systemctl stop {service}".into(),
                    status: "systemctl status {service}".into(),
                },
                // ...
            },
            boot: BootProfile {
                bootloaders: vec![BootloaderType::Grub, BootloaderType::SystemdBoot],
                default_bootloader: BootloaderType::Grub,
                initramfs_tool: InitramfsTool::Mkinitcpio,
                kernel_path: "/boot/vmlinuz-{variant}".into(),
                initramfs_path: "/boot/initramfs-{variant}.img".into(),
                // ...
            },
            capabilities: CapabilitySet {
                uefi: true,
                bios: true,
                secure_boot: true,
                fde: true,
                btrfs: true,
                zfs: true,  // Via AUR/archzfs
                lvm: true,
                raid: true,
                containers: true,
                flatpak: true,
                snap: true,
            },
        });

        m.insert("artix".into(), DistributionProfile {
            identity: DistroIdentity {
                id: "artix".into(),
                id_like: vec!["arch".into()],
                family: DistroFamily::Arch,
                name: "Artix Linux".into(),
                version: None,
                release_model: ReleaseModel::Rolling,
                libc: LibC::Glibc,
            },
            package_manager: PackageManagerProfile {
                manager_type: PackageManagerType::Pacman,
                package_format: PackageFormat::PkgTarZst,
                commands: PackageManagerCommands {
                    sync: "pacman -Sy".into(),
                    install: "pacman -S --noconfirm {packages}".into(),
                    remove: "pacman -R {packages}".into(),
                    upgrade: "pacman -Su --noconfirm".into(),
                    search: "pacman -Ss {query}".into(),
                    bootstrap: "basestrap {root} {packages}".into(),  // Artix uses basestrap
                },
                // ...
            },
            init_system: InitSystemProfile {
                init_type: InitSystemType::Runit,  // Default, but supports OpenRC, s6, dinit
                service_dir: "/etc/runit/sv".into(),
                enabled_dir: "/run/runit/service".into(),
                commands: InitSystemCommands {
                    enable: "ln -sf /etc/runit/sv/{service} /run/runit/service/".into(),
                    disable: "rm /run/runit/service/{service}".into(),
                    start: "sv up {service}".into(),
                    stop: "sv down {service}".into(),
                    status: "sv status {service}".into(),
                },
                // ...
            },
            // ... rest similar to arch but with init variations
        });

        // Add more profiles: debian, ubuntu, fedora, void, alpine, etc.

        m
    };
}
```

### 10.2 Detection Implementation

```rust
// src/distro/detection.rs

pub struct DistroDetector;

impl DistroDetector {
    pub fn detect() -> Result<DetectedDistro> {
        // Primary: os-release
        if let Ok(os_release) = Self::parse_os_release() {
            return Ok(os_release);
        }

        // Secondary: lsb-release
        if let Ok(lsb) = Self::parse_lsb_release() {
            return Ok(lsb);
        }

        // Tertiary: distribution-specific files
        Self::detect_by_specific_files()
    }

    fn parse_os_release() -> Result<DetectedDistro> {
        let content = fs::read_to_string("/etc/os-release")
            .or_else(|_| fs::read_to_string("/usr/lib/os-release"))?;

        let mut id = String::new();
        let mut id_like = Vec::new();
        let mut name = String::new();
        let mut version = None;

        for line in content.lines() {
            if let Some((key, value)) = line.split_once('=') {
                let value = value.trim_matches('"');
                match key {
                    "ID" => id = value.to_string(),
                    "ID_LIKE" => id_like = value.split_whitespace().map(String::from).collect(),
                    "NAME" => name = value.to_string(),
                    "VERSION_ID" => version = Some(value.to_string()),
                    _ => {}
                }
            }
        }

        Ok(DetectedDistro { id, id_like, name, version })
    }
}
```

### 10.3 Integration with Deploytix

```rust
// src/lib.rs additions

pub mod distro {
    pub mod detection;
    pub mod profiles;
    pub mod adaptation;
}

// src/config/deployment.rs additions

impl DeploymentConfig {
    /// Load with host OS detection and adaptation
    pub fn from_wizard_adaptive() -> Result<Self> {
        // Detect host distribution
        let host_distro = DistroDetector::detect()?;
        let host_profile = DistributionProfile::resolve(&host_distro)?;

        // Detect target distribution (may differ in cross-install scenarios)
        let target_profile = Self::select_target_distro()?;

        // Adapt configuration based on profiles
        let config = Self::from_wizard_with_profiles(&host_profile, &target_profile)?;

        Ok(config)
    }
}
```

### 10.4 Profile Configuration File Format

```toml
# /etc/deploytix/profiles.d/custom.toml

[identity]
id = "my-distro"
id_like = ["arch"]
family = "arch"
name = "My Custom Distro"
release_model = "rolling"
libc = "glibc"

[package_manager]
type = "pacman"
format = "pkg.tar.zst"

[package_manager.commands]
sync = "pacman -Sy"
install = "pacman -S --noconfirm {packages}"
remove = "pacman -R {packages}"
upgrade = "pacman -Su --noconfirm"
bootstrap = "pacstrap {root} {packages}"

[package_manager.package_map]
kernel = "linux-custom"
networkmanager = "networkmanager"

[init_system]
type = "runit"
service_dir = "/etc/runit/sv"
enabled_dir = "/run/runit/service"

[init_system.commands]
enable = "ln -sf /etc/runit/sv/{service} /run/runit/service/"
disable = "rm /run/runit/service/{service}"
start = "sv up {service}"
stop = "sv down {service}"

[boot]
initramfs_tool = "mkinitcpio"
kernel_path = "/boot/vmlinuz-{variant}"
initramfs_path = "/boot/initramfs-{variant}.img"
default_bootloader = "grub"

[capabilities]
uefi = true
bios = true
fde = true
btrfs = true
zfs = false
lvm = true
```

---

## Appendix A: Quick Reference Tables

### A.1 Distribution Detection Cheat Sheet

| Check | Command/Path | Result Interpretation |
|-------|--------------|----------------------|
| Primary ID | `grep ^ID= /etc/os-release` | `ID=ubuntu` → Ubuntu |
| Family | `grep ^ID_LIKE= /etc/os-release` | `ID_LIKE=debian` → Debian family |
| Package Manager | `which apt pacman dnf xbps-install apk` | First found = active |
| Init System | `cat /proc/1/comm` | `systemd`, `init`, `runit`, etc. |
| Architecture | `uname -m` | `x86_64`, `aarch64`, etc. |
| C Library | `ldd --version 2>&1 \| head -1` | `musl` or `GNU` |

### A.2 Bootstrap Command Quick Reference

```bash
# Debian/Ubuntu
debootstrap --arch=amd64 jammy /mnt http://archive.ubuntu.com/ubuntu

# Arch
pacstrap /mnt base linux linux-firmware

# Artix
basestrap /mnt base linux linux-firmware runit elogind-runit

# Fedora
dnf --installroot=/mnt --releasever=39 install @core

# Void
XBPS_ARCH=x86_64 xbps-install -S -R https://repo.voidlinux.org/current -r /mnt base-system

# Alpine
apk -X http://dl-cdn.alpinelinux.org/alpine/latest-stable/main \
    -U --allow-untrusted --root /mnt --initdb add alpine-base

# openSUSE
zypper --root /mnt install patterns-base-minimal_base
```

### A.3 Common Service Names Across Init Systems

| Function | Canonical | systemd | OpenRC | runit |
|----------|-----------|---------|--------|-------|
| Network (DHCP) | `dhcp` | `dhcpcd.service` | `dhcpcd` | `dhcpcd` |
| Network (NM) | `networkmanager` | `NetworkManager.service` | `NetworkManager` | `NetworkManager` |
| SSH Server | `sshd` | `sshd.service` | `sshd` | `sshd` |
| Cron | `cron` | `cronie.service` | `cronie` | `cronie` |
| Display Manager | `dm` | `sddm.service` | `sddm` | `sddm` |
| Audio | `audio` | `pipewire.service` | `pipewire` | `pipewire` |
| Bluetooth | `bluetooth` | `bluetooth.service` | `bluetoothd` | `bluetoothd` |
| Time Sync | `ntp` | `systemd-timesyncd` | `ntpd` | `ntpd` |

---

*Documentation generated: 2026-01-30*
*Classification schema version: 1.0*
