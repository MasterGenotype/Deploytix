# Deploytix

A portable Rust CLI and GUI application for automated deployment of Artix Linux to removable media and disks. Configuration-driven with TOML files, supporting multiple init systems, filesystems, desktop environments, and optional multi-volume LUKS2 encryption.

## Features

- **Fully Portable**: Single static binary built with musl — no external runtime dependencies
- **CLI & GUI**: Interactive CLI wizard or egui-based graphical step-by-step installer
- **Configuration-Driven**: TOML-based configs for reproducible, unattended installations
- **Dynamic Partitioning**: Auto-adjusts partition sizes relative to disk capacity and RAM
- **Multiple Init Systems**: runit, OpenRC, s6, dinit
- **Filesystem Choice**: ext4, btrfs, xfs, f2fs
- **Desktop Environments**: KDE Plasma, GNOME, XFCE, or headless/server
- **Network Backends**: iwd, NetworkManager, or ConnMan with optional dnscrypt-proxy
- **LUKS2 Encryption**: Multi-volume encrypted partitions with keyfile-based automatic unlocking
- **Bootloaders**: GRUB or systemd-boot
- **mkinitcpio Hook Constructor**: Automatically generates correct initramfs hooks based on configuration
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

```bash
cargo build --release --features gui

# Binaries: target/release/deploytix and target/release/deploytix-gui
```

### Static Binary (Portable)

```bash
rustup target add x86_64-unknown-linux-musl

cargo build --release --target x86_64-unknown-linux-musl
# Or shorthand: cargo portable

# Binary: target/x86_64-unknown-linux-musl/release/deploytix
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

# Dry-run (preview only)
sudo ./deploytix -n install

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
layout = "standard"       # standard, minimal, crypto_subvolume
filesystem = "btrfs"
encryption = false

[system]
init = "runit"            # runit, openrc, s6, dinit
bootloader = "grub"       # grub, systemd-boot
timezone = "America/New_York"
locale = "en_US.UTF-8"
keymap = "us"
hostname = "artix"

[user]
name = "user"
password = "changeme"
groups = ["wheel", "video", "audio", "network", "log"]

[network]
backend = "iwd"           # iwd, networkmanager, connman
dns = "dnscrypt-proxy"

[desktop]
environment = "kde"       # kde, gnome, xfce, none
```

## Partition Layouts

### Standard (7-partition)

| Partition | Size | Mount |
|-----------|------|-------|
| EFI | 512 MiB | /boot/efi |
| Boot | 2 GiB | /boot |
| Swap | 2×RAM (4–20 GiB) | — |
| Root | 6.4% of remaining | / |
| Usr | 26.8% of remaining | /usr |
| Var | 5.4% of remaining | /var |
| Home | Remainder | /home |

### Minimal (3-partition)

| Partition | Size | Mount |
|-----------|------|-------|
| EFI | 512 MiB | /boot/efi |
| Swap | 2×RAM (4–20 GiB) | — |
| Root | Remainder | / |

### CryptoSubvolume (multi-volume LUKS2)

| Partition | Size | Encryption | Mount |
|-----------|------|------------|-------|
| EFI | 512 MiB | None | /boot/efi |
| Boot | 2 GiB | None | /boot |
| Swap | 2×RAM (4–20 GiB) | LUKS2 | — |
| Root | 6.4% of remaining | LUKS2 | / |
| Usr | 26.8% of remaining | LUKS2 | /usr |
| Var | 5.4% of remaining | LUKS2 | /var |
| Home | Remainder | LUKS2 | /home |

Each encrypted partition uses a separate LUKS2 container. Root is unlocked with a passphrase; remaining volumes unlock automatically via keyfiles stored in the initramfs.

## Architecture

```
src/
├── main.rs              # CLI entry point (clap)
├── gui_main.rs          # GUI entry point (egui)
├── config/              # TOML config parsing, interactive wizard
├── disk/                # Disk detection, partition layout computation, formatting
├── install/             # basestrap, chroot, fstab/crypttab generation
├── configure/           # Bootloader, encryption, users, locale, services, hooks
├── desktop/             # Desktop environment package lists and setup
├── cleanup/             # Unmount and optional wipe
├── gui/                 # egui wizard panels and app state
└── utils/               # Command runner (with dry-run), error types, prompts
```

All system commands execute through a `CommandRunner` abstraction that respects dry-run mode, allowing safe previews of every operation.

## Requirements

**Host system (running the installer):**

- `basestrap` (from artools)
- `pacman`
- `sfdisk`
- `mkfs.*` utilities
- `grub-install` / `grub-mkconfig` (if using GRUB)
- `cryptsetup` (if using encryption)
- Root privileges

**Minimum disk size:**

- Standard / CryptoSubvolume layout: ~75 GiB
- Minimal layout: ~25 GiB

## Development

```bash
# Build
cargo build

# Lint
cargo clippy -- -D warnings

# Format check
cargo fmt -- --check
```

## License

GPL-3.0-or-later
