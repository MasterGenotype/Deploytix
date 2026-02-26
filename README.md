# Deploytix

Can also be built into a package and included in a ISO for installation via bootable media.

A portable Rust CLI and GUI application for automated deployment of **Artix Linux** to removable media and disks. Configuration-driven with TOML files, supporting multiple init systems, filesystems, desktop environments, and optional LUKS2 encryption.

> **Artix Linux Only** — Deploytix requires Artix-specific tools (`basestrap`, `artix-chroot`, `artools`) that are not available on Arch or other distributions. The host system running the installer must be Artix Linux.

## Features

- **Fully Portable**: Single static binary built with musl — no external runtime dependencies
- **CLI & GUI**: Interactive CLI wizard or egui-based graphical step-by-step installer
- **Configuration-Driven**: TOML-based configs for reproducible, unattended installations
- **Proportional Partitioning**: Automatically sizes partitions using a weighted proportion relative to total disk capacity — larger disks get proportionally larger partitions for each mount point
- **Multiple Init Systems**: runit, OpenRC, s6, dinit
- **Filesystem Choice**: btrfs (default), ext4, xfs, f2fs
- **Desktop Environments**: KDE Plasma, GNOME, XFCE, or headless/server
- **Network Backends**: iwd (standalone) or NetworkManager + iwd
- **LUKS2 Encryption**: Optional encryption layer on any layout with keyfile-based automatic unlocking
- **LVM Thin Provisioning**: Space-efficient overprovisioned layout with LUKS + LVM
- **Bootloader**: GRUB (only officially supported bootloader)
- **Secure Boot**: Optional signing via sbctl, shim (MOK), or manual keys
- **Swap Options**: Traditional partition, swap file + ZRAM, or ZRAM-only
- **Btrfs Subvolumes**: Automatic subvolume creation (@, @home, @var, @log, @snapshots) when using btrfs
- **mkinitcpio Hook Constructor**: Automatically generates correct initramfs hooks based on configuration
- **Automatic Dependency Checking**: Detects and installs missing host packages before proceeding
- **Dry-Run Mode**: Preview all operations without making changes

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

# Dry-run (preview only)
sudo ./deploytix -n install

# Generate desktop file (auto-detect desktop environment)
./deploytix generate-desktop-file -o deploytix-gui.desktop

# Generate desktop file for specific DE
./deploytix generate-desktop-file --de kde -o deploytix-gui.desktop
./deploytix generate-desktop-file --de gnome -o deploytix-gui.desktop
./deploytix generate-desktop-file --de xfce -o deploytix-gui.desktop

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
filesystem = "btrfs"      # btrfs, ext4, xfs, f2fs
encryption = false
swap_type = "partition"   # partition, filezram, zramonly

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
├── config/              # TOML config parsing, interactive wizard
├── disk/                # Disk detection, partition layout computation, formatting
├── install/             # Installer orchestrator, basestrap, chroot, fstab/crypttab
├── configure/           # Bootloader, encryption, users, locale, services, hooks, swap
├── desktop/             # Desktop environment package lists and setup
├── cleanup/             # Unmount and optional wipe
├── gui/                 # egui wizard panels and app state
├── resources/           # Embedded resources and templates
└── utils/               # CommandRunner (dry-run aware), error types, dependency checker
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
