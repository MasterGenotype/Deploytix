# Linux Distribution Classification and Installation Abstractions

This document details the commonalities across Linux distribution installation processes, package management systems, and provides a classification schema for detecting and adapting to host OS distributions. This documentation focuses exclusively on systemd-free distributions and init systems.

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
| Systemd-free distributions | Systemd-based distributions |
| Alternative init systems (runit, OpenRC, s6, dinit) | Systemd service management |
| Independent package managers | Distributions without alternatives |
| UEFI and BIOS boot | Architecture-specific quirks |

### 1.3 Philosophy

This project prioritizes Unix philosophy and modular init systems:

- **Simplicity**: Shell scripts over binary service managers
- **Transparency**: Readable configuration over opaque databases
- **Choice**: Multiple init systems, not monoculture
- **Independence**: No hard dependencies on specific implementations

---

## 2. Distribution Family Taxonomy

### 2.1 Systemd-Free Distribution Families

```
Systemd-Free Linux Distributions
├── Arch Family (systemd-free)
│   ├── Artix Linux (runit, OpenRC, s6, dinit)
│   ├── Obarun (s6)
│   ├── Hyperbola GNU/Linux-libre (OpenRC)
│   └── Parabola GNU/Linux-libre (OpenRC option)
│
├── Debian Family (systemd-free)
│   ├── Devuan (sysvinit, OpenRC, runit)
│   ├── Refracta (Devuan-based)
│   └── MX Linux (sysvinit default)
│
├── Void Family
│   └── Void Linux (runit, musl/glibc)
│
├── Alpine Family
│   └── Alpine Linux (OpenRC, musl)
│
├── Gentoo Family
│   ├── Gentoo (OpenRC default)
│   ├── Funtoo (OpenRC)
│   └── Calculate Linux (OpenRC)
│
├── Independent
│   ├── Chimera Linux (dinit, musl)
│   ├── Adélie Linux (s6)
│   ├── KISS Linux (busybox init)
│   ├── Carbs Linux (sinit)
│   └── Slackware (sysvinit/BSD-style)
│
└── Source-Based
    ├── CRUX (sysvinit/BSD-style)
    └── GoboLinux (custom)
```

### 2.2 Family Characteristics Matrix

| Family | Package Format | Package Manager | Default Init | C Library | Release Model |
|--------|---------------|-----------------|--------------|-----------|---------------|
| Artix | .pkg.tar.zst | pacman | runit/OpenRC/s6/dinit | glibc | Rolling |
| Devuan | .deb | apt/dpkg | sysvinit | glibc | Point release |
| Void | .xbps | xbps | runit | glibc/musl | Rolling |
| Alpine | .apk | apk | OpenRC | musl | Point release |
| Gentoo | ebuild | portage/emerge | OpenRC | glibc/musl | Rolling |
| Chimera | .apk | apk | dinit | musl | Rolling |
| Slackware | .txz/.tgz | pkgtools/slackpkg | sysvinit | glibc | Point release |

### 2.3 Init System Availability by Distribution

| Distribution | runit | OpenRC | s6 | dinit | sysvinit |
|--------------|-------|--------|-----|-------|----------|
| Artix | Default | Yes | Yes | Yes | No |
| Void | Default | No | No | No | No |
| Alpine | No | Default | No | No | No |
| Devuan | Yes | Yes | No | No | Default |
| Gentoo | Yes | Default | Yes | Yes | No |
| Chimera | No | No | No | Default | No |
| Obarun | No | No | Default | No | No |

### 2.4 Derivative Relationship Depth

Understanding derivative depth helps predict compatibility:

```
Depth 0: Root distributions (Arch, Debian, Void, Alpine, Gentoo)
Depth 1: Direct derivatives (Artix←Arch, Devuan←Debian, Chimera←Alpine-ish)
Depth 2: Secondary derivatives (Refracta←Devuan)
```

**Rule:** Higher depth = more potential divergence from upstream tooling.

---

## 3. Installation Channel Abstractions

### 3.1 Installation Methods Overview

| Method | Description | Use Case | Examples |
|--------|-------------|----------|----------|
| **Bootstrap** | Minimal rootfs extraction | Automated deployment | basestrap, debootstrap, xbps-install |
| **ISO Install** | Graphical/TUI installer | End-user installation | Calamares, custom TUI |
| **Rootfs Tarball** | Pre-built filesystem archive | Containers, WSL, chroot | rootfs.tar.xz |
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
| Artix | `basestrap` | `base`, `linux`, `$init` | `/etc/pacman.d/mirrorlist` |
| Devuan | `debootstrap` | `base-files`, `apt` | `/etc/apt/sources.list` |
| Void | `xbps-install -R` | `base-system` | `/etc/xbps.d/` |
| Alpine | `apk --root` | `alpine-base` | `/etc/apk/repositories` |
| Gentoo | `emerge --root` | `@system` | `/etc/portage/repos.conf/` |
| Chimera | `apk --root` | `base-full` | `/etc/apk/repositories` |

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

    /// Get init-specific packages
    fn init_packages(&self, init: InitSystemType) -> Vec<String>;

    /// Execute command in chroot context
    fn chroot_exec(&self, target: &Path, cmd: &str) -> Result<()>;
}
```

### 3.4 Rootfs Tarball Sources

| Distribution | Rootfs URL Pattern | Compression | Verification |
|--------------|-------------------|-------------|--------------|
| Artix | `iso.artixlinux.org/iso/artix-base-*.tar.zst` | zstd | SHA256 |
| Void | `repo-default.voidlinux.org/live/current/void-*-ROOTFS-*.tar.xz` | xz | SHA256 |
| Alpine | `alpinelinux.org/releases/*/releases/*/alpine-minirootfs-*.tar.gz` | gzip | SHA256 |
| Devuan | `files.devuan.org/devuan_*/minimal-live/*.tar.xz` | xz | SHA256 |
| Chimera | `repo.chimera-linux.org/live/*/chimera-linux-*-ROOTFS-*.tar.gz` | gzip | SHA256 |

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
├── Init system selection (Artix)
├── Base system installation
├── Bootloader installation
├── Initial configuration
└── Post-install hooks

Phase 5: Finalization
├── Generate fstab
├── Set root password
├── Create user accounts
├── Enable services (init-specific)
└── Unmount and reboot
```

---

## 4. Package Manager Unification

### 4.1 Operation Equivalence Matrix

| Operation | apt (Devuan) | pacman (Artix) | xbps (Void) | apk (Alpine/Chimera) | emerge (Gentoo) |
|-----------|--------------|----------------|-------------|---------------------|-----------------|
| Update index | `apt update` | `pacman -Sy` | `xbps-install -S` | `apk update` | `emerge --sync` |
| Upgrade all | `apt upgrade` | `pacman -Su` | `xbps-install -u` | `apk upgrade` | `emerge -uDN @world` |
| Install pkg | `apt install` | `pacman -S` | `xbps-install` | `apk add` | `emerge` |
| Remove pkg | `apt remove` | `pacman -R` | `xbps-remove` | `apk del` | `emerge -C` |
| Search | `apt search` | `pacman -Ss` | `xbps-query -Rs` | `apk search` | `emerge -s` |
| Info | `apt show` | `pacman -Si` | `xbps-query -R` | `apk info` | `emerge -pv` |
| List files | `dpkg -L` | `pacman -Ql` | `xbps-query -f` | `apk info -L` | `equery files` |
| Owner of file | `dpkg -S` | `pacman -Qo` | `xbps-query -o` | `apk info -W` | `equery belongs` |
| Clean cache | `apt clean` | `pacman -Sc` | `xbps-remove -O` | `apk cache clean` | `eclean distfiles` |
| List installed | `dpkg -l` | `pacman -Q` | `xbps-query -l` | `apk info` | `qlist -I` |

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

| Function | Artix | Devuan | Void | Alpine | Gentoo |
|----------|-------|--------|------|--------|--------|
| Kernel | `linux` | `linux-image-*` | `linux` | `linux-lts` | `gentoo-sources` |
| Firmware | `linux-firmware` | `firmware-linux` | `linux-firmware` | `linux-firmware` | `linux-firmware` |
| GRUB (EFI) | `grub` | `grub-efi-amd64` | `grub-x86_64-efi` | `grub-efi` | `grub` |
| NetworkManager | `networkmanager` | `network-manager` | `NetworkManager` | `networkmanager` | `networkmanager` |
| Wireless (iwd) | `iwd` | `iwd` | `iwd` | `iwd` | `iwd` |
| Cryptsetup | `cryptsetup` | `cryptsetup` | `cryptsetup` | `cryptsetup` | `cryptsetup` |
| Btrfs tools | `btrfs-progs` | `btrfs-progs` | `btrfs-progs` | `btrfs-progs` | `btrfs-progs` |
| Sudo | `sudo` | `sudo` | `sudo` | `sudo` | `sudo` |
| Doas | `opendoas` | `doas` | `opendoas` | `doas` | `doas` |

### 4.4 Init-Specific Package Names

| Init System | Artix | Void | Alpine | Devuan | Gentoo |
|-------------|-------|------|--------|--------|--------|
| runit base | `runit` | `runit` | N/A | `runit-init` | `runit` |
| OpenRC base | `openrc` | N/A | `openrc` | `openrc` | `openrc` |
| s6 base | `s6-base` | N/A | N/A | N/A | `s6` |
| dinit base | `dinit` | N/A | N/A | N/A | `dinit` |
| elogind (runit) | `elogind-runit` | `elogind` | N/A | `elogind` | `elogind` |
| elogind (OpenRC) | `elogind-openrc` | N/A | `elogind` | `elogind` | `elogind` |
| seatd (runit) | `seatd-runit` | `seatd` | N/A | N/A | `seatd` |
| seatd (OpenRC) | `seatd-openrc` | N/A | `seatd` | N/A | `seatd` |

### 4.5 Package Group/Pattern Equivalents

| Concept | Artix | Devuan | Void | Alpine | Gentoo |
|---------|-------|--------|------|--------|--------|
| Minimal base | `base` | `--variant=minbase` | `base-system` | `alpine-base` | `@system` |
| Development | `base-devel` | `build-essential` | `base-devel` | `build-base` | `@system` |
| X.org | `xorg` | `xorg` | `xorg` | `xorg-server` | `x11-base/xorg-x11` |
| Fonts | `noto-fonts` | `fonts-noto` | `noto-fonts-ttf` | `font-noto` | `media-fonts/noto` |

### 4.6 Repository Configuration

| Distribution | Config Location | Format | Mirror Variable |
|--------------|-----------------|--------|-----------------|
| Artix | `/etc/pacman.d/mirrorlist` | `Server = URL` | `$repo`, `$arch` |
| Devuan | `/etc/apt/sources.list.d/` | `deb URL dist components` | Direct URL |
| Void | `/etc/xbps.d/*.conf` | `repository=URL` | `$arch` |
| Alpine | `/etc/apk/repositories` | Plain URL list | `$version`, `$arch` |
| Gentoo | `/etc/portage/repos.conf/` | INI format | Sync URI |
| Chimera | `/etc/apk/repositories` | Plain URL list | Direct URL |

---

## 5. Init System Classification

### 5.1 Init System Overview

| Init System | Type | Service Format | Supervision | Complexity |
|-------------|------|----------------|-------------|------------|
| runit | Supervision | Directory + run script | Built-in | Minimal |
| OpenRC | Dependency | Shell scripts | Optional | Moderate |
| s6 | Supervision | Execline scripts | Built-in | Moderate |
| dinit | Dependency + Supervision | INI-like files | Built-in | Moderate |
| sysvinit | Sequential | Shell scripts | None | Traditional |

### 5.2 Service Management Equivalents

| Operation | runit | OpenRC | s6 | dinit | sysvinit |
|-----------|-------|--------|-----|-------|----------|
| Start | `sv up X` | `rc-service X start` | `s6-svc -u /run/service/X` | `dinitctl start X` | `/etc/init.d/X start` |
| Stop | `sv down X` | `rc-service X stop` | `s6-svc -d /run/service/X` | `dinitctl stop X` | `/etc/init.d/X stop` |
| Restart | `sv restart X` | `rc-service X restart` | `s6-svc -r /run/service/X` | `dinitctl restart X` | `/etc/init.d/X restart` |
| Enable | `ln -s /etc/runit/sv/X /run/runit/service/` | `rc-update add X` | Compile DB | `dinitctl enable X` | `update-rc.d X defaults` |
| Disable | `rm /run/runit/service/X` | `rc-update del X` | Compile DB | `dinitctl disable X` | `update-rc.d X remove` |
| Status | `sv status X` | `rc-service X status` | `s6-svstat /run/service/X` | `dinitctl status X` | `/etc/init.d/X status` |
| List | `ls /run/runit/service/` | `rc-status` | `s6-rc -a list` | `dinitctl list` | `ls /etc/init.d/` |

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

    /// Get service suffix (e.g., "-runit", "-openrc")
    fn service_suffix(&self) -> &'static str;
}
```

### 5.4 Init System Directory Structures

**runit:**
```
/etc/runit/
├── 1                    # Stage 1: System initialization
├── 2                    # Stage 2: Service supervision
├── 3                    # Stage 3: Shutdown
└── sv/                  # Available services
    ├── agetty-tty1/
    │   └── run          # Executable script
    ├── dhcpcd/
    │   ├── run
    │   └── log/
    │       └── run      # Optional logging service
    └── sshd/
        └── run

/run/runit/service/      # Enabled services (symlinks)
```

**OpenRC:**
```
/etc/init.d/             # Service scripts
├── agetty
├── dhcpcd
├── sshd
└── ...

/etc/runlevels/
├── boot/                # Boot runlevel
│   ├── hwclock -> /etc/init.d/hwclock
│   └── ...
├── default/             # Default runlevel
│   ├── dhcpcd -> /etc/init.d/dhcpcd
│   └── sshd -> /etc/init.d/sshd
└── shutdown/            # Shutdown runlevel

/etc/conf.d/             # Service configuration
├── dhcpcd
└── sshd
```

**s6:**
```
/etc/s6/sv/              # Service definitions
├── sshd/
│   ├── type             # "longrun" or "oneshot"
│   ├── run              # Execline script
│   ├── finish           # Optional cleanup
│   └── dependencies.d/  # Service dependencies
└── dhcpcd/
    └── run

/run/service/            # Active services (scandir)
```

**dinit:**
```
/etc/dinit.d/            # Service definitions
├── boot                 # Boot target
├── sshd                 # INI-like service file
├── dhcpcd
└── boot.d/              # Enabled at boot (symlinks)
    ├── sshd -> ../sshd
    └── dhcpcd -> ../dhcpcd
```

### 5.5 Service File Examples

**runit service (`/etc/runit/sv/sshd/run`):**
```sh
#!/bin/sh
exec /usr/sbin/sshd -D
```

**OpenRC service (`/etc/init.d/sshd`):**
```sh
#!/sbin/openrc-run

name="sshd"
command="/usr/sbin/sshd"
command_args="-D"
pidfile="/run/sshd.pid"

depend() {
    need net
    use logger dns
}
```

**s6 service (`/etc/s6/sv/sshd/run`):**
```
#!/bin/execlineb -P
/usr/sbin/sshd -D
```

**dinit service (`/etc/dinit.d/sshd`):**
```ini
type = process
command = /usr/sbin/sshd -D
depends-on = network
```

### 5.6 Service Name Variations

| Service | runit (Artix) | OpenRC (Alpine) | s6 (Artix) | dinit (Artix) |
|---------|---------------|-----------------|------------|---------------|
| Getty | `agetty-tty1` | `agetty` | `agetty-tty1` | `agetty-tty1` |
| DHCP Client | `dhcpcd` | `dhcpcd` | `dhcpcd` | `dhcpcd` |
| SSH Server | `sshd` | `sshd` | `sshd` | `sshd` |
| Cron | `cronie` | `crond` | `cronie` | `cronie` |
| NTP | `ntpd` | `ntpd` | `ntpd` | `ntpd` |
| Logging | `socklog-unix` | `syslog-ng` | `s6-log` | `syslog-ng` |
| Device Manager | `udevd` | `udev` | `udevd` | `udevd` |
| Seat Manager | `seatd` | `seatd` | `seatd` | `seatd` |
| Session Manager | `elogind` | `elogind` | `elogind` | `elogind` |

---

## 6. Filesystem and Boot Conventions

### 6.1 Directory Hierarchy Variations

While FHS provides the standard, distributions have variations:

| Path | Standard | Artix | Void | Alpine |
|------|----------|-------|------|--------|
| `/bin`, `/sbin` | Separate | Symlink to `/usr/bin` | Symlink to `/usr/bin` | Separate |
| `/lib`, `/lib64` | Separate | Symlink to `/usr/lib` | Symlink to `/usr/lib` | Separate |
| `/etc/os-release` | Present | Yes | Yes | Yes |
| Service logs | Varies | `/var/log/socklog/` | `/var/log/socklog/` | `/var/log/` |

### 6.2 Boot Configuration Locations

| Bootloader | Config Location | Entry Location |
|------------|-----------------|----------------|
| GRUB | `/etc/default/grub` | `/boot/grub/grub.cfg` |
| rEFInd | `/boot/EFI/refind/refind.conf` | Auto-detect |
| EFISTUB | N/A (kernel params) | NVRAM |
| Syslinux | `/boot/syslinux/syslinux.cfg` | N/A |
| LILO | `/etc/lilo.conf` | N/A |

### 6.3 Kernel and Initramfs Paths

| Distribution | Kernel Path | Initramfs Path | Naming Convention |
|--------------|-------------|----------------|-------------------|
| Artix | `/boot/vmlinuz-linux` | `/boot/initramfs-linux.img` | Variant suffix |
| Void | `/boot/vmlinuz-*` | `/boot/initramfs-*.img` | Version suffix |
| Alpine | `/boot/vmlinuz-*` | `/boot/initramfs-*` | Variant suffix |
| Devuan | `/boot/vmlinuz-*` | `/boot/initrd.img-*` | Version suffix |
| Gentoo | `/boot/vmlinuz-*` | `/boot/initramfs-*.img` | Custom |

### 6.4 Initramfs Generation Tools

| Tool | Distribution | Config File | Regenerate Command |
|------|--------------|-------------|-------------------|
| mkinitcpio | Artix | `/etc/mkinitcpio.conf` | `mkinitcpio -P` |
| dracut | Void, Gentoo | `/etc/dracut.conf.d/` | `dracut --force` |
| mkinitfs | Alpine | `/etc/mkinitfs/mkinitfs.conf` | `mkinitfs` |
| initramfs-tools | Devuan | `/etc/initramfs-tools/` | `update-initramfs -u` |

### 6.5 Fstab UUID vs Label vs Path

| Method | Format | Reliability | Distribution Preference |
|--------|--------|-------------|------------------------|
| UUID | `UUID=abc-123` | Highest | Devuan, Void |
| PARTUUID | `PARTUUID=abc-123` | High | GPT systems |
| Label | `LABEL=root` | Medium | Alpine |
| Path | `/dev/sda1` | Low | Legacy only |

---

## 7. Detection Mechanisms

### 7.1 Primary Detection: os-release

The `/etc/os-release` file (or `/usr/lib/os-release`) is the standard detection method:

```bash
# /etc/os-release fields (Artix example)
ID=artix
ID_LIKE=arch
NAME="Artix Linux"
PRETTY_NAME="Artix Linux"
HOME_URL="https://artixlinux.org/"
```

**Key Fields:**

| Field | Purpose | Example Values |
|-------|---------|----------------|
| `ID` | Primary identifier | `artix`, `devuan`, `void`, `alpine` |
| `ID_LIKE` | Parent/compatible distros | `arch`, `debian` |
| `VERSION_ID` | Numeric version | `3.18`, `5.0` |
| `VERSION_CODENAME` | Release codename | `daedalus`, `chimaera` |

### 7.2 Init System Detection

```rust
/// Detect running init system (no systemd)
pub fn detect_init_system() -> Option<InitSystemType> {
    // Check /proc/1/comm for init process name
    if let Ok(init_name) = std::fs::read_to_string("/proc/1/comm") {
        match init_name.trim() {
            "runit" => return Some(InitSystemType::Runit),
            "init" => return detect_init_by_filesystem(),
            "s6-svscan" => return Some(InitSystemType::S6),
            "dinit" => return Some(InitSystemType::Dinit),
            _ => {}
        }
    }

    // Fallback: Check for init-specific paths
    detect_init_by_filesystem()
}

fn detect_init_by_filesystem() -> Option<InitSystemType> {
    if Path::new("/run/runit").exists() || Path::new("/etc/runit/runsvdir").exists() {
        Some(InitSystemType::Runit)
    } else if Path::new("/run/openrc").exists() {
        Some(InitSystemType::OpenRC)
    } else if Path::new("/run/s6").exists() || Path::new("/run/service/.s6-svscan").exists() {
        Some(InitSystemType::S6)
    } else if Path::new("/run/dinit").exists() || Path::new("/etc/dinit.d").exists() {
        Some(InitSystemType::Dinit)
    } else if Path::new("/etc/inittab").exists() {
        Some(InitSystemType::SysVinit)
    } else {
        None
    }
}
```

### 7.3 Package Manager Detection

```rust
/// Detect package manager by binary presence
pub fn detect_package_manager() -> Option<PackageManagerType> {
    let checks = [
        ("/usr/bin/pacman", PackageManagerType::Pacman),
        ("/usr/bin/xbps-install", PackageManagerType::Xbps),
        ("/sbin/apk", PackageManagerType::Apk),
        ("/usr/bin/apt", PackageManagerType::Apt),
        ("/usr/bin/emerge", PackageManagerType::Portage),
        ("/usr/sbin/slackpkg", PackageManagerType::Slackpkg),
    ];

    for (path, pm_type) in checks {
        if Path::new(path).exists() {
            return Some(pm_type);
        }
    }
    None
}
```

### 7.4 Distribution-Specific Detection Files

| File | Distribution |
|------|--------------|
| `/etc/artix-release` | Artix Linux |
| `/etc/devuan_version` | Devuan |
| `/etc/void-release` | Void Linux |
| `/etc/alpine-release` | Alpine |
| `/etc/gentoo-release` | Gentoo |
| `/etc/slackware-version` | Slackware |
| `/etc/chimera-release` | Chimera Linux |

### 7.5 C Library Detection

```rust
pub fn detect_libc() -> LibC {
    // Check if musl is in use
    if let Ok(output) = std::process::Command::new("ldd")
        .arg("--version")
        .output()
    {
        let output_str = String::from_utf8_lossy(&output.stderr);
        if output_str.contains("musl") {
            return LibC::Musl;
        }
    }

    // Check for musl libc directly
    if Path::new("/lib/ld-musl-x86_64.so.1").exists()
        || Path::new("/lib/ld-musl-aarch64.so.1").exists()
    {
        return LibC::Musl;
    }

    LibC::Glibc
}
```

---

## 8. Classification Parameter Schema

### 8.1 Distribution Profile Structure

```rust
/// Complete distribution profile (systemd-free only)
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
    Arch,       // Artix, Obarun, Hyperbola
    Debian,     // Devuan
    Void,       // Void Linux
    Alpine,     // Alpine, Chimera
    Gentoo,     // Gentoo, Funtoo
    Slackware,  // Slackware
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

### 8.2 Init System Profile

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitSystemProfile {
    /// Init system type
    pub init_type: InitSystemType,

    /// Service directory (available services)
    pub service_dir: PathBuf,

    /// Enabled services location
    pub enabled_dir: PathBuf,

    /// Service management commands
    pub commands: InitSystemCommands,

    /// Required packages
    pub packages: Vec<String>,

    /// Service name mappings
    pub service_map: HashMap<String, String>,

    /// Package suffix for init-specific packages
    pub package_suffix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InitSystemType {
    Runit,
    OpenRC,
    S6,
    Dinit,
    SysVinit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitSystemCommands {
    pub enable: String,         // "ln -sf /etc/runit/sv/{service} /run/runit/service/"
    pub disable: String,        // "rm /run/runit/service/{service}"
    pub start: String,          // "sv up {service}"
    pub stop: String,           // "sv down {service}"
    pub status: String,         // "sv status {service}"
    pub list_enabled: String,   // "ls /run/runit/service/"
}
```

### 8.3 Package Manager Profile

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

    /// Package name mappings (canonical -> distro-specific)
    pub package_map: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PackageManagerType {
    Pacman,     // Artix
    Apt,        // Devuan
    Xbps,       // Void
    Apk,        // Alpine, Chimera
    Portage,    // Gentoo
    Slackpkg,   // Slackware
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PackageFormat {
    PkgTarZst,  // Arch family
    Deb,        // Debian family
    Xbps,       // Void
    Apk,        // Alpine family
    Ebuild,     // Gentoo
    Txz,        // Slackware
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageManagerCommands {
    pub sync: String,           // "pacman -Sy"
    pub install: String,        // "pacman -S --noconfirm {packages}"
    pub remove: String,         // "pacman -R {packages}"
    pub upgrade: String,        // "pacman -Su --noconfirm"
    pub search: String,         // "pacman -Ss {query}"
    pub bootstrap: String,      // "basestrap {root} {packages}"
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
pub enum BootloaderType {
    Grub,
    Refind,
    Efistub,
    Syslinux,
    Lilo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InitramfsTool {
    Mkinitcpio,     // Artix
    Dracut,         // Void, Gentoo
    InitramfsTools, // Devuan
    Mkinitfs,       // Alpine
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

    /// Supports Secure Boot (without shim)
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

    /// Supports Flatpak
    pub flatpak: bool,

    /// Musl-compatible
    pub musl: bool,
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
        profile
    }
}
```

### 9.2 Init-Aware Package Resolution

```rust
impl PackageManagerProfile {
    /// Resolve package name with init system awareness
    pub fn resolve_package(&self, canonical: &str, init: &InitSystemProfile) -> String {
        // Check for init-specific package first
        let init_specific = format!("{}{}", canonical, init.package_suffix);
        if let Some(mapped) = self.package_map.get(&init_specific) {
            return mapped.clone();
        }

        // Fall back to canonical mapping
        self.package_map
            .get(canonical)
            .cloned()
            .unwrap_or_else(|| canonical.to_string())
    }
}

// Example:
// canonical = "elogind", init.package_suffix = "-runit"
// First tries: "elogind-runit" -> "elogind-runit"
// If not found: "elogind" -> mapped name or "elogind"
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

    /// Enable a service by canonical name
    pub fn enable(&self, cmd: &CommandRunner, root: &Path, canonical: &str) -> Result<()> {
        let service_name = self.resolve_service(canonical);
        let command = self.commands.enable.replace("{service}", &service_name);
        cmd.run_in_chroot(&root.to_string_lossy(), &command)
    }
}
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
// Template: "basestrap {root} {packages}"
// Vars: {"root": "/mnt", "packages": "base linux runit elogind-runit"}
// Result: "basestrap /mnt base linux runit elogind-runit"
```

### 9.5 Cross-Distribution Installation

```rust
/// Install to a target with potentially different distribution
pub fn cross_install(
    host: &DistributionProfile,
    target: &DistributionProfile,
    root: &Path,
    packages: &[&str],
) -> Result<()> {
    // Resolve packages for target distribution
    let target_packages: Vec<String> = packages
        .iter()
        .map(|p| target.package_manager.resolve_package(p, &target.init_system))
        .collect();

    // Use host package manager with target root
    let cmd_template = &target.package_manager.commands.bootstrap;
    let packages_str = target_packages.join(" ");

    let mut vars = HashMap::new();
    vars.insert("root", root.to_str().unwrap());
    vars.insert("packages", &packages_str);

    let command = target.package_manager.commands.expand(cmd_template, &vars);

    CommandRunner::new(false).run("sh", &["-c", &command])
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

        // Artix Linux (runit default)
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
                    bootstrap: "basestrap {root} {packages}".into(),
                },
                package_map: artix_package_map(),
                // ...
            },
            init_system: InitSystemProfile {
                init_type: InitSystemType::Runit,
                service_dir: "/etc/runit/sv".into(),
                enabled_dir: "/run/runit/service".into(),
                commands: InitSystemCommands {
                    enable: "ln -sf /etc/runit/sv/{service} /run/runit/service/".into(),
                    disable: "rm /run/runit/service/{service}".into(),
                    start: "sv up {service}".into(),
                    stop: "sv down {service}".into(),
                    status: "sv status {service}".into(),
                    list_enabled: "ls /run/runit/service/".into(),
                },
                packages: vec!["runit".into(), "elogind-runit".into()],
                service_map: runit_service_map(),
                package_suffix: "-runit".into(),
            },
            boot: BootProfile {
                bootloaders: vec![BootloaderType::Grub, BootloaderType::Refind],
                default_bootloader: BootloaderType::Grub,
                initramfs_tool: InitramfsTool::Mkinitcpio,
                kernel_path: "/boot/vmlinuz-{variant}".into(),
                initramfs_path: "/boot/initramfs-{variant}.img".into(),
                // ...
            },
            capabilities: CapabilitySet {
                uefi: true,
                bios: true,
                secure_boot: false,
                fde: true,
                btrfs: true,
                zfs: true,
                lvm: true,
                raid: true,
                flatpak: true,
                musl: false,
            },
        });

        // Void Linux
        m.insert("void".into(), DistributionProfile {
            identity: DistroIdentity {
                id: "void".into(),
                id_like: vec![],
                family: DistroFamily::Void,
                name: "Void Linux".into(),
                version: None,
                release_model: ReleaseModel::Rolling,
                libc: LibC::Glibc, // or Musl variant
            },
            package_manager: PackageManagerProfile {
                manager_type: PackageManagerType::Xbps,
                package_format: PackageFormat::Xbps,
                commands: PackageManagerCommands {
                    sync: "xbps-install -S".into(),
                    install: "xbps-install -y {packages}".into(),
                    remove: "xbps-remove {packages}".into(),
                    upgrade: "xbps-install -Su".into(),
                    search: "xbps-query -Rs {query}".into(),
                    bootstrap: "XBPS_ARCH={arch} xbps-install -S -R {mirror} -r {root} {packages}".into(),
                },
                package_map: void_package_map(),
                // ...
            },
            init_system: InitSystemProfile {
                init_type: InitSystemType::Runit,
                service_dir: "/etc/sv".into(),
                enabled_dir: "/var/service".into(),
                commands: InitSystemCommands {
                    enable: "ln -sf /etc/sv/{service} /var/service/".into(),
                    disable: "rm /var/service/{service}".into(),
                    start: "sv up {service}".into(),
                    stop: "sv down {service}".into(),
                    status: "sv status {service}".into(),
                    list_enabled: "ls /var/service/".into(),
                },
                packages: vec!["runit".into(), "void-repo-nonfree".into()],
                service_map: void_service_map(),
                package_suffix: "".into(), // Void doesn't use suffix
            },
            // ...
        });

        // Alpine Linux
        m.insert("alpine".into(), DistributionProfile {
            identity: DistroIdentity {
                id: "alpine".into(),
                id_like: vec![],
                family: DistroFamily::Alpine,
                name: "Alpine Linux".into(),
                version: Some("3.19".into()),
                release_model: ReleaseModel::PointRelease,
                libc: LibC::Musl,
            },
            package_manager: PackageManagerProfile {
                manager_type: PackageManagerType::Apk,
                package_format: PackageFormat::Apk,
                commands: PackageManagerCommands {
                    sync: "apk update".into(),
                    install: "apk add {packages}".into(),
                    remove: "apk del {packages}".into(),
                    upgrade: "apk upgrade".into(),
                    search: "apk search {query}".into(),
                    bootstrap: "apk -X {mirror} -U --allow-untrusted --root {root} --initdb add {packages}".into(),
                },
                package_map: alpine_package_map(),
                // ...
            },
            init_system: InitSystemProfile {
                init_type: InitSystemType::OpenRC,
                service_dir: "/etc/init.d".into(),
                enabled_dir: "/etc/runlevels/default".into(),
                commands: InitSystemCommands {
                    enable: "rc-update add {service} default".into(),
                    disable: "rc-update del {service} default".into(),
                    start: "rc-service {service} start".into(),
                    stop: "rc-service {service} stop".into(),
                    status: "rc-service {service} status".into(),
                    list_enabled: "rc-status default".into(),
                },
                packages: vec!["openrc".into()],
                service_map: openrc_service_map(),
                package_suffix: "".into(),
            },
            // ...
        });

        // Devuan
        m.insert("devuan".into(), DistributionProfile {
            identity: DistroIdentity {
                id: "devuan".into(),
                id_like: vec!["debian".into()],
                family: DistroFamily::Debian,
                name: "Devuan GNU/Linux".into(),
                version: Some("5.0".into()),
                release_model: ReleaseModel::PointRelease,
                libc: LibC::Glibc,
            },
            package_manager: PackageManagerProfile {
                manager_type: PackageManagerType::Apt,
                package_format: PackageFormat::Deb,
                commands: PackageManagerCommands {
                    sync: "apt update".into(),
                    install: "apt install -y {packages}".into(),
                    remove: "apt remove {packages}".into(),
                    upgrade: "apt upgrade -y".into(),
                    search: "apt search {query}".into(),
                    bootstrap: "debootstrap --arch={arch} {release} {root} {mirror}".into(),
                },
                package_map: devuan_package_map(),
                // ...
            },
            init_system: InitSystemProfile {
                init_type: InitSystemType::SysVinit,
                service_dir: "/etc/init.d".into(),
                enabled_dir: "/etc/rc2.d".into(), // Default runlevel 2
                commands: InitSystemCommands {
                    enable: "update-rc.d {service} defaults".into(),
                    disable: "update-rc.d {service} remove".into(),
                    start: "/etc/init.d/{service} start".into(),
                    stop: "/etc/init.d/{service} stop".into(),
                    status: "/etc/init.d/{service} status".into(),
                    list_enabled: "ls /etc/rc2.d/S*".into(),
                },
                packages: vec!["sysvinit-core".into(), "elogind".into()],
                service_map: sysvinit_service_map(),
                package_suffix: "".into(),
            },
            // ...
        });

        // Chimera Linux
        m.insert("chimera".into(), DistributionProfile {
            identity: DistroIdentity {
                id: "chimera".into(),
                id_like: vec![],
                family: DistroFamily::Alpine,
                name: "Chimera Linux".into(),
                version: None,
                release_model: ReleaseModel::Rolling,
                libc: LibC::Musl,
            },
            package_manager: PackageManagerProfile {
                manager_type: PackageManagerType::Apk,
                package_format: PackageFormat::Apk,
                commands: PackageManagerCommands {
                    sync: "apk update".into(),
                    install: "apk add {packages}".into(),
                    remove: "apk del {packages}".into(),
                    upgrade: "apk upgrade".into(),
                    search: "apk search {query}".into(),
                    bootstrap: "apk --root {root} --initdb add {packages}".into(),
                },
                package_map: chimera_package_map(),
                // ...
            },
            init_system: InitSystemProfile {
                init_type: InitSystemType::Dinit,
                service_dir: "/etc/dinit.d".into(),
                enabled_dir: "/etc/dinit.d/boot.d".into(),
                commands: InitSystemCommands {
                    enable: "dinitctl enable {service}".into(),
                    disable: "dinitctl disable {service}".into(),
                    start: "dinitctl start {service}".into(),
                    stop: "dinitctl stop {service}".into(),
                    status: "dinitctl status {service}".into(),
                    list_enabled: "dinitctl list".into(),
                },
                packages: vec!["dinit-chimera".into()],
                service_map: dinit_service_map(),
                package_suffix: "".into(),
            },
            // ...
        });

        m
    };
}
```

### 10.2 Package Mapping Functions

```rust
fn artix_package_map() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("kernel".into(), "linux".into());
    m.insert("kernel-headers".into(), "linux-headers".into());
    m.insert("firmware".into(), "linux-firmware".into());
    m.insert("grub-efi".into(), "grub".into());
    m.insert("networkmanager".into(), "networkmanager".into());
    m.insert("networkmanager-runit".into(), "networkmanager-runit".into());
    m.insert("networkmanager-openrc".into(), "networkmanager-openrc".into());
    m.insert("iwd".into(), "iwd".into());
    m.insert("iwd-runit".into(), "iwd-runit".into());
    m.insert("cryptsetup".into(), "cryptsetup".into());
    m.insert("btrfs".into(), "btrfs-progs".into());
    m.insert("sudo".into(), "sudo".into());
    m.insert("doas".into(), "opendoas".into());
    m
}

fn void_package_map() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("kernel".into(), "linux".into());
    m.insert("kernel-headers".into(), "linux-headers".into());
    m.insert("firmware".into(), "linux-firmware".into());
    m.insert("grub-efi".into(), "grub-x86_64-efi".into());
    m.insert("networkmanager".into(), "NetworkManager".into());
    m.insert("iwd".into(), "iwd".into());
    m.insert("cryptsetup".into(), "cryptsetup".into());
    m.insert("btrfs".into(), "btrfs-progs".into());
    m.insert("sudo".into(), "sudo".into());
    m.insert("doas".into(), "opendoas".into());
    m
}

fn alpine_package_map() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("kernel".into(), "linux-lts".into());
    m.insert("kernel-headers".into(), "linux-lts-dev".into());
    m.insert("firmware".into(), "linux-firmware".into());
    m.insert("grub-efi".into(), "grub-efi".into());
    m.insert("networkmanager".into(), "networkmanager".into());
    m.insert("iwd".into(), "iwd".into());
    m.insert("cryptsetup".into(), "cryptsetup".into());
    m.insert("btrfs".into(), "btrfs-progs".into());
    m.insert("sudo".into(), "sudo".into());
    m.insert("doas".into(), "doas".into());
    m
}
```

### 10.3 Service Mapping Functions

```rust
fn runit_service_map() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("getty".into(), "agetty-tty1".into());
    m.insert("dhcp".into(), "dhcpcd".into());
    m.insert("ssh".into(), "sshd".into());
    m.insert("cron".into(), "cronie".into());
    m.insert("ntp".into(), "ntpd".into());
    m.insert("syslog".into(), "socklog-unix".into());
    m.insert("dbus".into(), "dbus".into());
    m.insert("elogind".into(), "elogind".into());
    m.insert("seatd".into(), "seatd".into());
    m.insert("networkmanager".into(), "NetworkManager".into());
    m.insert("iwd".into(), "iwd".into());
    m
}

fn openrc_service_map() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("getty".into(), "agetty".into());
    m.insert("dhcp".into(), "dhcpcd".into());
    m.insert("ssh".into(), "sshd".into());
    m.insert("cron".into(), "crond".into());
    m.insert("ntp".into(), "ntpd".into());
    m.insert("syslog".into(), "syslog-ng".into());
    m.insert("dbus".into(), "dbus".into());
    m.insert("elogind".into(), "elogind".into());
    m.insert("seatd".into(), "seatd".into());
    m.insert("networkmanager".into(), "networkmanager".into());
    m.insert("iwd".into(), "iwd".into());
    m
}

fn dinit_service_map() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("getty".into(), "agetty-tty1".into());
    m.insert("dhcp".into(), "dhcpcd".into());
    m.insert("ssh".into(), "sshd".into());
    m.insert("cron".into(), "crond".into());
    m.insert("ntp".into(), "ntpd".into());
    m.insert("syslog".into(), "syslog-ng".into());
    m.insert("dbus".into(), "dbus".into());
    m.insert("seatd".into(), "seatd".into());
    m.insert("networkmanager".into(), "networkmanager".into());
    m.insert("iwd".into(), "iwd".into());
    m
}
```

### 10.4 Profile Configuration File Format

```toml
# /etc/deploytix/profiles.d/custom.toml

[identity]
id = "my-distro"
id_like = ["artix", "arch"]
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
bootstrap = "basestrap {root} {packages}"

[package_manager.package_map]
kernel = "linux-custom"
networkmanager = "networkmanager"

[init_system]
type = "runit"
service_dir = "/etc/runit/sv"
enabled_dir = "/run/runit/service"
package_suffix = "-runit"

[init_system.packages]
base = ["runit", "elogind-runit"]

[init_system.commands]
enable = "ln -sf /etc/runit/sv/{service} /run/runit/service/"
disable = "rm /run/runit/service/{service}"
start = "sv up {service}"
stop = "sv down {service}"

[init_system.service_map]
ssh = "sshd"
dhcp = "dhcpcd"
cron = "cronie"

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
musl = false
```

---

## Appendix A: Quick Reference Tables

### A.1 Distribution Detection Cheat Sheet

| Check | Command/Path | Result Interpretation |
|-------|--------------|----------------------|
| Primary ID | `grep ^ID= /etc/os-release` | `ID=artix` → Artix |
| Family | `grep ^ID_LIKE= /etc/os-release` | `ID_LIKE=arch` → Arch family |
| Package Manager | `which pacman xbps-install apk apt` | First found = active |
| Init System | `cat /proc/1/comm` | `runit`, `init`, `s6-svscan`, `dinit` |
| C Library | `ldd --version 2>&1 \| head -1` | `musl` or `GNU` |

### A.2 Bootstrap Command Quick Reference

```bash
# Artix (runit)
basestrap /mnt base linux linux-firmware runit elogind-runit

# Artix (OpenRC)
basestrap /mnt base linux linux-firmware openrc elogind-openrc

# Artix (s6)
basestrap /mnt base linux linux-firmware s6-base elogind-s6

# Artix (dinit)
basestrap /mnt base linux linux-firmware dinit elogind-dinit

# Void (glibc)
XBPS_ARCH=x86_64 xbps-install -S -R https://repo-default.voidlinux.org/current -r /mnt base-system

# Void (musl)
XBPS_ARCH=x86_64-musl xbps-install -S -R https://repo-default.voidlinux.org/current/musl -r /mnt base-system

# Alpine
apk -X http://dl-cdn.alpinelinux.org/alpine/latest-stable/main \
    -U --allow-untrusted --root /mnt --initdb add alpine-base

# Devuan
debootstrap --arch=amd64 daedalus /mnt http://deb.devuan.org/merged

# Chimera
apk --root /mnt --initdb add base-full
```

### A.3 Service Enable Commands by Init

| Init | Enable SSH | Enable NetworkManager |
|------|------------|----------------------|
| runit | `ln -s /etc/runit/sv/sshd /run/runit/service/` | `ln -s /etc/runit/sv/NetworkManager /run/runit/service/` |
| OpenRC | `rc-update add sshd default` | `rc-update add networkmanager default` |
| s6 | `touch /etc/s6/adminsv/default/contents.d/sshd && s6-db-reload` | Similar |
| dinit | `dinitctl enable sshd` | `dinitctl enable networkmanager` |
| sysvinit | `update-rc.d ssh defaults` | `update-rc.d network-manager defaults` |

### A.4 Artix Init System Package Suffixes

| Package | runit | OpenRC | s6 | dinit |
|---------|-------|--------|-----|-------|
| elogind | `elogind-runit` | `elogind-openrc` | `elogind-s6` | `elogind-dinit` |
| NetworkManager | `networkmanager-runit` | `networkmanager-openrc` | `networkmanager-s6` | `networkmanager-dinit` |
| seatd | `seatd-runit` | `seatd-openrc` | `seatd-s6` | `seatd-dinit` |
| cronie | `cronie-runit` | `cronie-openrc` | `cronie-s6` | `cronie-dinit` |
| iwd | `iwd-runit` | `iwd-openrc` | `iwd-s6` | `iwd-dinit` |
| cups | `cups-runit` | `cups-openrc` | `cups-s6` | `cups-dinit` |

---

*Documentation generated: 2026-01-30*
*Classification schema version: 1.1 (systemd-free)*
