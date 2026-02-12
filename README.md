# Deploytix
WIP
A portable Rust CLI application for automated deployment of Artix Linux to removable media and disks.

## Features

- **Fully Portable**: Single static binary with embedded resources, no external dependencies
- **Dynamic Partitioning**: Auto-adjusts partition sizes relative to disk capacity
- **Multiple Init Systems**: Support for runit, OpenRC, s6, and dinit
- **Filesystem Choice**: ext4, btrfs, xfs, f2fs
- **Desktop Environments**: KDE Plasma, GNOME, XFCE, or headless/server
- **Network Configuration**: iwd, NetworkManager, or ConnMan with optional dnscrypt-proxy
- **mkinitcpio Hook Constructor**: Automatically generates correct hooks based on configuration
- **LUKS Encryption**: Optional full-disk encryption support (WIP)
- **Dry-Run Mode**: Preview all operations without making changes

## Installation

### From Source

```bash
# Install Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Clone and build
git clone https://github.com/superphenotype/deploytix
cd deploytix
cargo build --release

# Binary will be at target/release/deploytix
```

### Static Binary (Portable)

```bash
# Add musl target
rustup target add x86_64-unknown-linux-musl

# Build static binary
cargo portable
# Or: cargo build --release --target x86_64-unknown-linux-musl

# Binary will be at target/x86_64-unknown-linux-musl/release/deploytix
```

## Usage

### GUI Installation (Recommended)

```bash
# Build with GUI support
cargo build --release --features gui

# Run GUI as root
sudo ./target/release/deploytix-gui
```

The GUI provides a step-by-step wizard for:
- Selecting target disk
- Configuring partitions, filesystem, and encryption
- Setting up system options (init, bootloader, locale)
- Creating user account
- Choosing network backend and desktop environment
- Reviewing configuration before installation

### CLI Interactive Installation

```bash
# Run as root
sudo ./deploytix
```

### With Configuration File

```bash
# Generate sample config
./deploytix generate-config -o my-config.toml

# Edit configuration
nano my-config.toml

# Run installation with config
sudo ./deploytix install -c my-config.toml
```

### List Available Disks

```bash
./deploytix list-disks
```

### Dry-Run Mode

```bash
sudo ./deploytix -n install
```

### Cleanup

```bash
# Unmount partitions
sudo ./deploytix cleanup

# Unmount and wipe partition table
sudo ./deploytix cleanup -w
```

## Configuration

Example `deploytix.toml`:

```toml
[disk]
device = "/dev/sda"
layout = "standard"  # standard, minimal
filesystem = "btrfs"
encryption = false

[system]
init = "runit"
bootloader = "grub"
timezone = "America/New_York"
locale = "en_US.UTF-8"
keymap = "us"
hostname = "artix"

[user]
name = "user"
password = "changeme"
groups = ["wheel", "video", "audio", "network", "log"]

[network]
backend = "iwd"
dns = "dnscrypt-proxy"

[desktop]
environment = "kde"  # kde, gnome, xfce, none
```

## Partition Layouts

### Standard (7-partition)
| Partition | Size | Mount |
|-----------|------|-------|
| EFI | 512 MiB | /boot/efi |
| Boot | 2 GiB | /boot |
| Swap | 2×RAM (4-20 GiB) | - |
| Root | 6.4% of remaining | / |
| Usr | 26.8% of remaining | /usr |
| Var | 5.4% of remaining | /var |
| Home | Remainder | /home |

### Minimal (3-partition)
| Partition | Size | Mount |
|-----------|------|-------|
| EFI | 512 MiB | /boot/efi |
| Swap | 2×RAM (4-20 GiB) | - |
| Root | Remainder | / |

## Requirements

**On the host system (running the installer):**
- `basestrap` (from artools)
- `pacman`
- `sfdisk` or `fdisk`
- `mkfs.*` utilities
- `grub` (for GRUB bootloader)

**Minimum disk size:**
- Standard layout: ~75 GiB
- Minimal layout: ~25 GiB

## License

GPL-3.0-or-later

## Contributing

Contributions welcome! Please open an issue or pull request.

Co-Authored-By: Warp <agent@warp.dev>
