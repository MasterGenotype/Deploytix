# Deploytix

A portable Rust CLI and GUI application for automated deployment of **Artix Linux** to removable media and disks. Configuration-driven with TOML files, supporting multiple init systems, filesystems, desktop environments, and optional LUKS2 encryption.

Can also be built into a package and included in an ISO for installation via bootable media.

> **Artix Linux Only** — Deploytix requires Artix-specific tools (`basestrap`, `artix-chroot`, `artools`) that are not available on Arch or other distributions. The host system running the installer must be Artix Linux.

## What Deploytix Does

Deploytix automates the entire process of installing Artix Linux onto a target disk — from partitioning and encryption through base system installation, bootloader configuration, user creation, and desktop environment setup. It replaces the manual sequence of `sfdisk` → `mkfs` → `basestrap` → `artix-chroot` → manual configuration with a single reproducible operation driven by a TOML config file or interactive wizard.

### The Installation Pipeline

The installer executes a **6-phase pipeline**, where each phase builds on the previous one. If any phase fails, an emergency cleanup handler automatically unmounts filesystems, deactivates LVM, and closes LUKS containers.

**Phase 1 — Prepare.** Validates the configuration, detects the target disk, computes the partition layout based on disk size, and presents a confirmation prompt. No changes are made to disk yet.

**Phase 2 — Partition, Format & Mount.** Writes a GPT partition table via `sfdisk`, then sets up the storage stack. This phase branches based on which features are enabled:
- **Plain:** Format each partition with the chosen filesystem and mount them.
- **Multi-volume LUKS:** Create separate LUKS2 containers on each data partition (Root, Usr, Var, Home), format the mapped devices as btrfs, and mount.
- **LVM Thin:** Create a single LUKS2 container on the LVM PV partition, set up a volume group with a thin pool, create thin volumes (root, usr, var, home), format each as btrfs, and mount.
- **Subvolumes:** When btrfs subvolumes are enabled, the root partition is formatted once and subvolumes (`@`, `@home`, `@var`, `@log`, `@snapshots`) are created inside it, then mounted individually with their own mount options.
- **Preserve Home:** When reinstalling, the existing partition table is verified instead of rewritten. The `/home` partition, LUKS container, or `@home` subvolume is left untouched while all system partitions are reformatted.

**Phase 3 — Base System.** Installs the base Artix system into the mounted target using `basestrap` (Artix's equivalent of `pacstrap`). The package list is assembled dynamically from the chosen init system, filesystem tools, encryption tools, and any desktop packages. Generates `/etc/fstab` from partition UUIDs. For encrypted systems, generates `/etc/crypttab` and deploys keyfiles into the initramfs so only the root volume requires a passphrase at boot.

**Phase 4 — System Configuration.** Enters the target via `artix-chroot` and configures:
- Locale, timezone, keymap, hostname
- User account creation (with optional `chown -R` fixup for preserved home directories)
- Encrypted home directory setup via gocryptfs + pam_mount (if enabled)
- mkinitcpio configuration with dynamically generated hooks for the specific encryption and filesystem setup
- GRUB bootloader installation with kernel parameters for encrypted root, integrity, and hibernation
- Network backend (iwd standalone or NetworkManager + iwd)
- Init system service enablement (runit/OpenRC/s6/dinit)

**Phase 5 — Desktop Environment.** Installs the selected desktop environment (KDE Plasma, GNOME, XFCE) with display manager, compositor, and Artix-specific service packages. Skipped for headless/server installs.

**Phase 6 — Finalize.** Regenerates the initramfs with `mkinitcpio -P`, unmounts all filesystems in reverse order, exports ZFS pools (if used), and closes all LUKS containers.

### How It Works Under the Hood

**CommandRunner abstraction.** Every system command (`mkfs`, `cryptsetup`, `mount`, `chroot`, etc.) goes through a `CommandRunner` that supports dry-run mode. In dry-run, commands are printed but never executed, allowing full previews of destructive operations.

**Proportional partition sizing.** After allocating fixed partitions (EFI 512 MiB, Boot 2 GiB, Swap 2×RAM clamped to 4–20 GiB), the remaining disk space is distributed across data partitions using weighted proportions — a 128 GiB disk and a 2 TiB disk both get sensible sizes without manual tuning.

**Feature-driven pipeline.** The installer doesn't use separate code paths per layout. Instead, `run_phases()` checks feature flags (encryption, LVM thin, subvolumes, preserve_home) and each step is a no-op if its feature is disabled. This keeps the code composable rather than branching on layout types.

**Automatic dependency resolution.** Before installation begins, Deploytix checks for required host packages (`sfdisk`, `cryptsetup`, `btrfs-progs`, etc.) and offers to install any that are missing via `pacman`.

**Signal-safe cleanup.** SIGINT/SIGTERM are caught by a signal handler. On interruption, the emergency cleanup runs — unmounting filesystems, deactivating LVM, killing orphaned `cryptsetup` processes, and closing LUKS containers — preventing the system from being left in a half-configured state.

## Features

- **Fully Portable**: Single static binary built with musl — no external runtime dependencies
- **CLI & GUI**: Interactive CLI wizard or egui-based graphical step-by-step installer
- **Configuration-Driven**: TOML-based configs for reproducible, unattended installations
- **Proportional Partitioning**: Automatically sizes partitions using weighted proportions relative to total disk capacity
- **Multiple Init Systems**: runit, OpenRC, s6, dinit
- **Filesystem Choice**: btrfs (default), ext4, xfs, f2fs, ZFS
- **Desktop Environments**: KDE Plasma, GNOME, XFCE, or headless/server
- **Network Backends**: iwd (standalone) or NetworkManager + iwd
- **LUKS2 Encryption**: Optional multi-volume encryption with keyfile-based automatic unlocking
- **dm-integrity**: Optional per-sector HMAC-SHA256 integrity protection on encrypted volumes
- **Boot Encryption**: Optional LUKS1 encryption on `/boot` (GRUB-compatible)
- **LVM Thin Provisioning**: Space-efficient overprovisioned layout with LUKS + LVM
- **Btrfs Subvolumes**: Automatic subvolume creation (`@`, `@home`, `@var`, `@log`, `@snapshots`)
- **Preserve Home**: Reinstall the system without overwriting `/home` — works across partition, subvolume, and multi-volume LUKS layouts
- **Encrypted Home**: Per-user gocryptfs encryption with automatic pam_mount unlock on login
- **Secure Boot**: Optional signing via sbctl, shim (MOK), or manual keys
- **Swap Options**: Traditional partition, swap file + ZRAM, or ZRAM-only
- **Custom Partitions**: Define your own partition layout with per-partition encryption overrides
- **Bootloader**: GRUB with auto-generated kernel parameters for encryption, integrity, and hibernation
- **mkinitcpio Hook Constructor**: Automatically generates correct initramfs hooks based on configuration
- **Automatic Dependency Checking**: Detects and installs missing host packages before proceeding
- **Dry-Run Mode**: Preview all operations without making changes
- **Signal-Safe Cleanup**: Catches SIGINT/SIGTERM and cleanly undoes partial installations

## Installation

### From Source

```bash
# Install Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build (CLI only)
git clone https://github.com/MasterGenotype/Deploytix
cd Deploytix
cargo build --release

# Binary: target/release/deploytix
```

### With GUI Support

The GUI is built as a separate binary using egui (glow backend with X11/Wayland support):

```bash
cargo build --release --features gui

# Binaries: target/release/deploytix (CLI) and target/release/deploytix-gui (GUI)
```

GUI build requires system libraries: `libxcb`, `libxkbcommon`, `libwayland`, `libGL`.

### Static Binary (Portable)

Builds a fully statically-linked binary with musl — zero runtime dependencies, runs on any x86_64 Linux:

```bash
# Prerequisites
rustup target add x86_64-unknown-linux-musl
# On Debian/Ubuntu: sudo apt install musl-tools
# On Artix/Arch: sudo pacman -S musl

cargo build --release --target x86_64-unknown-linux-musl
# Or shorthand: cargo portable

# Binary: target/x86_64-unknown-linux-musl/release/deploytix
```

> **Note**: The GUI binary cannot be built with musl due to X11/Wayland library dependencies. The portable build produces the CLI binary only.

### make install

The Makefile provides targets for building and installing both binaries system-wide:

```bash
make install            # Build CLI + GUI, install to /usr/bin with .desktop and polkit policy
make install-cli        # CLI only
make install-all        # CLI + GUI + desktop entry + polkit
make install-portable   # Static musl binary
make uninstall          # Remove all installed files
```

## Usage

### GUI Installer

```bash
sudo ./target/release/deploytix-gui
```

The GUI provides a 7-step wizard: Disk → Partitions → System → User → Network → Review → Install.

### CLI Interactive Installer

```bash
sudo ./deploytix
```

### With Configuration File

```bash
# Generate a sample config
./deploytix generate-config -o my-config.toml

# Edit to taste
nano my-config.toml

# Run installation
sudo ./deploytix install -c my-config.toml
```

### Other Commands

```bash
# List available disks
./deploytix list-disks

# Validate a config file without installing
./deploytix validate my-config.toml

# Dry-run (preview only)
sudo ./deploytix -n install

# Generate desktop file (auto-detect desktop environment)
./deploytix generate-desktop-file -o deploytix-gui.desktop

# Generate desktop file for specific DE
./deploytix generate-desktop-file --de kde -o deploytix-gui.desktop

# Cleanup: unmount partitions
sudo ./deploytix cleanup

# Cleanup: unmount and wipe partition table
sudo ./deploytix cleanup -w

# Cleanup: target a specific device
sudo ./deploytix cleanup --device /dev/sda --wipe
```

## Configuration

Example `deploytix.toml`:

```toml
[disk]
device = "/dev/sda"
layout = "standard"       # standard, minimal, lvmthin, custom
filesystem = "btrfs"      # btrfs, ext4, xfs, f2fs, zfs
encryption = false
swap_type = "partition"   # partition, filezram, zramonly
use_subvolumes = false
preserve_home = false     # true to keep /home during reinstall

[system]
init = "runit"            # runit, openrc, s6, dinit
bootloader = "grub"       # grub (only supported option)
timezone = "America/New_York"
locale = "en_US.UTF-8"
keymap = "us"
hostname = "artix"
secureboot = false        # optional Secure Boot signing

[user]
name = "user"
password = "changeme"
groups = ["wheel", "video", "audio", "network", "log"]
encrypt_home = false      # gocryptfs encrypted home directory

[network]
backend = "iwd"           # iwd, networkmanager

[desktop]
environment = "kde"       # kde, gnome, xfce, none
```

## Partition Layouts

After allocating fixed-size partitions (EFI, Boot, Swap), the remaining disk space is distributed across data partitions using proportional weight differentials. Each partition receives a share of the remaining capacity based on its assigned weight, so the layout scales naturally from small drives to large ones — a 128 GiB disk and a 2 TiB disk both get sensible partition sizes without manual tuning.

### Standard (7-partition)

- **EFI** — 512 MiB → `/boot/efi`
- **Boot** — 2 GiB → `/boot` (LegacyBIOSBootable)
- **Swap** — 2×RAM, clamped 4–20 GiB
- **Root** — 6.4% of remaining → `/`
- **Usr** — 26.8% of remaining → `/usr`
- **Var** — 5.4% of remaining → `/var`
- **Home** — remainder → `/home`

Supports optional LUKS2 encryption on data partitions (Root, Usr, Var, Home). When enabled, each encrypted partition uses a separate LUKS2 container — Root is unlocked with a passphrase and remaining volumes unlock automatically via keyfiles stored in the initramfs.

### Minimal (4-partition)

- **EFI** — 512 MiB → `/boot/efi`
- **Boot** — 2 GiB → `/boot` (LegacyBIOSBootable)
- **Swap** — 2×RAM, clamped 4–20 GiB
- **Root** — remainder → `/`

Supports both UEFI and Legacy BIOS boot. When using btrfs, subvolumes (@, @home, @var, @log, @snapshots) are created automatically.

### LVM Thin (LUKS + LVM thin provisioning)

- **EFI** — 512 MiB → `/boot/efi`
- **Boot** — 2 GiB → `/boot`
- **Swap** — optional
- **LVM PV** — remainder → LUKS2-encrypted physical volume
  - Thin volumes: root, usr, var, home (space-efficient overprovisioning)

### Custom (user-defined partitions)

The Custom layout lets you define your own data partitions. EFI (512 MiB), Boot (2 GiB), and Swap are prepended automatically. Set `size_mib = 0` on exactly one partition to use the remaining disk space.

```toml
[disk]
device = "/dev/sda"
layout = "custom"
filesystem = "ext4"
encryption = false

[[disk.custom_partitions]]
mount_point = "/"
size_mib = 30720      # 30 GiB

[[disk.custom_partitions]]
mount_point = "/var"
size_mib = 10240      # 10 GiB

[[disk.custom_partitions]]
mount_point = "/home"
size_mib = 0          # Consumes all remaining space
```

**Custom partition fields:**
- `mount_point` (required): Absolute path, e.g. `/`, `/home`, `/data`. Cannot be `/boot` or `/boot/efi` (reserved).
- `size_mib` (required): Size in MiB. Use `0` for one partition to fill the remaining disk space.
- `label` (optional): Partition label. If omitted, derived from the last path component (e.g. `/home` → `HOME`).
- `encryption` (optional): Per-partition encryption override. Inherits from `disk.encryption` when omitted.

## Architecture

```
src/
├── main.rs              # CLI entry point (clap)
├── gui_main.rs          # GUI entry point (egui, requires --features gui)
├── lib.rs               # Library root (re-exports for GUI binary)
├── config/              # TOML config parsing, interactive wizard, validation
├── disk/                # Block device detection, partition layout computation,
│                        #   sfdisk scripting, filesystem formatting, btrfs subvolumes,
│                        #   ZFS pool/dataset creation, LVM thin provisioning
├── install/             # Installer orchestrator (6-phase pipeline), basestrap,
│                        #   chroot mounting, fstab/crypttab generation
├── configure/           # In-chroot system configuration: bootloader (GRUB),
│                        #   encryption (LUKS2/LUKS1), users, gocryptfs, locale,
│                        #   mkinitcpio hooks, network services, swap (ZRAM/file),
│                        #   keyfiles, Secure Boot
├── desktop/             # Desktop environment package lists and post-install setup
│                        #   (KDE Plasma, GNOME, XFCE)
├── cleanup/             # Unmount and optional disk wipe
├── gui/                 # egui wizard panels and app state (7-step installer)
├── resources/           # Embedded resources and templates
└── utils/               # CommandRunner (dry-run aware), error types (DeploytixError),
                         #   dependency checker, signal handlers, interactive prompts
```

All system commands execute through a `CommandRunner` abstraction that respects dry-run mode, allowing safe previews of every operation. Missing host dependencies are detected automatically and can be installed via pacman before the installation begins.

## Requirements

**Host system (Artix Linux only):**

- `basestrap` and `artix-chroot` (from `artools`) — Artix-specific; not available on Arch
- `pacman` — package manager
- `sfdisk` — partition table creation (from `util-linux`)
- `mkfs.vfat` (`dosfstools`), `mkfs.ext4` (`e2fsprogs`), and filesystem-specific tools (`btrfs-progs`, `xfsprogs`, `f2fs-tools`)
- `grub-install` / `grub-mkconfig` (if using GRUB bootloader)
- `cryptsetup` (if using LUKS2 encryption)
- `pvcreate` / `vgcreate` / `lvcreate` from `lvm2` (if using LVM Thin layout)
- `gocryptfs` + `pam_mount` (if using encrypted home directories)
- Root privileges

Deploytix will check for missing dependencies at startup and offer to install them automatically via `pacman`.

**Minimum disk size:**

- Standard / LVM Thin layout: ~75 GiB
- Minimal layout: ~25 GiB

## Development

```bash
# Development build
cargo build

# Release build
cargo build --release

# GUI build
cargo build --release --features gui

# Portable static binary (musl)
cargo portable

# Lint
cargo clippy -- -D warnings

# Format check
cargo fmt -- --check

# Run tests
cargo test
```

## License

MIT
