# Deploytix

A portable Rust CLI and GUI application for automated deployment of **Artix Linux** to removable media and disks. Configuration-driven with TOML files, supporting multiple init systems, filesystems, desktop environments, LUKS2 encryption, LVM thin provisioning, and a gaming/handheld device stack.

Can also be built into a package and included in an ISO for installation via bootable media.

> **Artix Linux Only** — Deploytix requires Artix-specific tools (`basestrap`, `artix-chroot`, `artools`) that are not available on Arch or other distributions. The host system running the installer must be Artix Linux.

## Installation

### From Source

```bash
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
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
# Or shorthand: cargo portable

# Binary: target/x86_64-unknown-linux-musl/release/deploytix
```

> The GUI binary cannot be built with musl due to X11/Wayland library dependencies. The portable build produces the CLI binary only.

### make install

```bash
make install            # Build CLI + GUI, install to /usr/bin with .desktop and polkit policy
make install-cli        # CLI only
make install-all        # CLI + GUI + desktop entry + polkit
make install-portable   # Static musl binary
make install-gcc        # GCC/glibc linked binary
make uninstall          # Remove all installed files
```

## Usage

### GUI Installer

```bash
sudo deploytix-gui
```

The GUI provides a 3-step wizard: **Configure → Review → Install**. The configure step includes panels for disk selection, partition editing, system settings, user creation, network/desktop, and handheld gaming options.

From the Review step you can run a **Rehearsal** installation (writes to disk, then wipes) to test the full pipeline before committing to a real install. Configuration can be saved to a TOML file from the GUI.

### CLI Interactive Installer

```bash
sudo deploytix
```

Runs the interactive configuration wizard and proceeds to install.

### With Configuration File

```bash
# Generate a sample config
deploytix generate-config -o my-config.toml

# Edit to taste
nano my-config.toml

# Run installation
sudo deploytix install -c my-config.toml
```

### CLI Commands

```bash
deploytix install [-c config.toml] [-d /dev/sdX]   # Install (wizard or config-driven)
deploytix list-disks [--all]                        # List available target disks
deploytix validate <config.toml>                    # Validate a config file
deploytix generate-config [-o path.toml]            # Generate a sample config
deploytix rehearse [-c config.toml] [-l log.log]    # Full rehearsal install (writes + wipes disk)
deploytix cleanup [-d /dev/sdX] [--wipe]            # Unmount and optionally wipe
deploytix deps <subcommand>                         # Package dependency tracking
deploytix generate-desktop-file [--de kde] [-o f]   # Generate .desktop launcher

# Global flags
deploytix -v ...       # Verbose output
```

## Installation Pipeline

The installer executes a **feature-driven pipeline** where each step checks its own feature flags and is a no-op when disabled. If any phase fails, a signal-safe emergency cleanup handler unmounts filesystems, deactivates LVM, kills orphaned `cryptsetup` processes, and closes LUKS containers.

**Phase 1 — Prepare.** Validates configuration, detects the target disk, computes the partition layout, checks host dependencies (offering to install missing ones via `pacman`), and presents a confirmation prompt.

**Phase 2 — Partition & Storage Stack.** Writes a GPT partition table via `sfdisk`, then branches based on enabled features:
- **Plain:** Format each partition with the chosen filesystem and mount.
- **Multi-volume LUKS:** Create separate LUKS2 containers on each data partition (Root, Usr, Var, Home), format mapped devices, and mount.
- **LVM Thin:** Create a single LUKS2 container on the LVM PV partition, set up a volume group with a thin pool, create thin volumes, format, and mount.
- **Btrfs subvolumes:** When btrfs is selected, subvolumes (`@`, `@home`, `@var`, `@log`, `@snapshots`) are created automatically and mounted individually.
- **ZFS:** Create ZFS pools and datasets alongside non-ZFS partitions (EFI, swap).
- **Preserve Home:** When reinstalling, the existing `/home` partition/subvolume/LUKS container is left untouched.

**Phase 3 — Base System.** Installs the base Artix system via `basestrap` with a dynamically-assembled package list. Generates `/etc/fstab` from UUIDs. For encrypted layouts, generates `/etc/crypttab` and deploys keyfiles into the initramfs.

**Phase 4 — System Configuration.** Enters the target via `artix-chroot` and configures locale, timezone, keymap, hostname, user account, mkinitcpio hooks, GRUB bootloader, network backend, init system services, Secure Boot (if enabled), GPU drivers, and swap (ZRAM/file).

**Phase 5 — Desktop & Packages.** Installs the selected desktop environment with display manager. Then conditionally installs Wine, gaming packages (Steam, gamescope), session switching scripts, yay AUR helper, AUR packages, btrfs tools, sysctl tweaks, Handheld Daemon, Decky Loader, evdevhook2, and Modular mod manager.

**Phase 6 — Finalize.** Regenerates the initramfs, unmounts all filesystems in reverse order, exports ZFS pools if applicable, and closes all LUKS containers.

## Configuration

Example `deploytix.toml`:

```toml
[disk]
device = "/dev/sda"
filesystem = "btrfs"           # btrfs, ext4, xfs, zfs, f2fs
boot_filesystem = "btrfs"      # defaults to ext4; btrfs uses @boot subvolume
encryption = true
encryption_password = "passphrase"
luks_mapper_name = "Crypt-Root"
boot_encryption = false
integrity = false              # dm-integrity (HMAC-SHA256) on encrypted volumes
keyfile_enabled = true
use_subvolumes = true          # auto-set to true when filesystem = btrfs
use_lvm_thin = false
swap_type = "zramonly"         # partition, filezram, zramonly
zram_algorithm = "zstd"
preserve_home = false

# User-defined data partitions (EFI + Boot + Swap are auto-prepended)
[[disk.partitions]]
mount_point = "/"
size_mib = 46080

[[disk.partitions]]
mount_point = "/usr"
size_mib = 66560

[[disk.partitions]]
mount_point = "/var"
size_mib = 40960

[[disk.partitions]]
mount_point = "/home"
size_mib = 0                   # 0 = use remaining disk space

[system]
init = "runit"                 # runit, openrc, s6, dinit
bootloader = "grub"
timezone = "America/Vancouver"
locale = "en_US.UTF-8"
keymap = "us"
hostname = "artix"
hibernation = false
secureboot = false             # sbctl, shim (MOK), or manual keys
secureboot_method = "sbctl"

[user]
name = "user"
password = "changeme"
groups = ["wheel", "video", "audio", "input", "render", "network", "log", "seat"]
sudoer = true

[network]
backend = "networkmanager"     # iwd, networkmanager

[desktop]
environment = "kde"            # kde, gnome, xfce, none

[packages]
install_yay = true             # AUR helper (built from source)
install_wine = true            # Wine compatibility layer
install_gaming = true          # Steam, gamescope (Bazzite fork)
install_session_switching = true  # gamescope ↔ desktop via greetd
install_btrfs_tools = true     # snapper, btrfs-assistant (via yay)
install_modular = true         # Modular mod manager
sysctl_gaming_tweaks = true    # vm.max_map_count, swappiness, etc.
sysctl_network_performance = true  # BBR, fq, larger buffers
install_hhd = true             # Handheld Daemon (gamepad remapping, TDP)
install_decky_loader = true    # Steam plugin framework
install_evdevhook2 = true      # Cemuhook UDP motion server
gpu_drivers = ["amd"]          # nvidia, amd, intel
```

### Partition Configuration

EFI (512 MiB), Boot (2 GiB), and Swap (when `swap_type = "partition"`) are always auto-prepended. You define your data partitions in `[[disk.partitions]]`:

- `mount_point` (required) — absolute path, e.g. `/`, `/home`, `/var`. Cannot be `/boot` or `/boot/efi`.
- `size_mib` (required) — size in MiB. Exactly one partition may use `0` to fill remaining space.
- `label` (optional) — partition label. Derived from mount point if omitted (`/home` → `HOME`).
- `encryption` (optional) — per-partition encryption override. Inherits from `disk.encryption` when omitted.

Default partitions when none are specified: `/` (20 GiB), `/usr` (30 GiB), `/var` (10 GiB), `/home` (remainder).

## Rehearsal

**Rehearsal** (`deploytix rehearse`) is the true dry-run: it executes the full installation pipeline on the real target disk with every command recorded, then wipes the disk to restore pristine state. The result is a detailed report showing exactly what happened and where it failed. This is destructive to the target device — it writes for real, then cleans up.

Also available from the GUI Review step.

## Package Dependency Tracking

Deploytix includes a built-in dependency tracker for Artix/Arch packages backed by pacman/libalpm metadata (sync DBs, `pactree`, `expac`). It never scrapes the Artix website.

### Subcommands

```bash
deploytix deps resolve <package>                # Full runtime closure
deploytix deps tree <package>                   # Human-readable tree
deploytix deps reverse <package>                # Reverse dependencies
deploytix deps graph <package> [-o pkg.dot]     # Graphviz DOT output
deploytix deps plan-install <package>           # What pacman -S would install
deploytix deps metadata <package>               # Full normalized metadata
deploytix deps compare <pkg-a> <pkg-b>          # Diff two packages
```

Common flags: `--config <path>`, `--dbpath <path>`, `--root <path>`, `--include-optional`, `--include-make`, `--include-check`, `--json`, `--dot`, `--offline <fixture.json>`, `--clean-root` (plan-install only).

## Gaming & Handheld Features

The `[packages]` section provides a full gaming/handheld device stack:

- **Steam + Gamescope** — Installs Steam and builds the Bazzite-maintained gamescope compositor from vendored source (`vendor/gamescope`).
- **Session Switching** — Deploys greetd-based scripts for switching between a gamescope (Steam Deck-style) session and a desktop session. Includes `deploytix-session-manager`, `session-select`, `return-to-gamemode`, PAM configs, and a `steamos-session-select` compatibility symlink.
- **Handheld Daemon (HHD)** — Gamepad remapping, TDP control, per-game profiles (AUR: `hhd-git`). Writes init-specific service files.
- **Decky Loader** — Steam plugin framework (AUR: `decky-loader-bin`). Writes init-specific service files.
- **evdevhook2** — Cemuhook UDP motion server for DualShock/DualSense/Joy-Con controllers (AUR: `evdevhook2-git`). Installs udev rules and service files.
- **Modular** — Game mod manager from vendored source (`vendor/Modular-1`).
- **Wine** — Wine compatibility layer packages.
- **GPU Drivers** — NVIDIA, AMD, and/or Intel driver stacks.
- **Sysctl Tweaks** — Gaming performance (`vm.max_map_count`, swappiness) and network performance (BBR, fq, larger socket buffers, ECN).

## Architecture

```
src/
├── main.rs                # CLI entry point (clap subcommands)
├── gui_main.rs            # GUI entry point (egui, --features gui)
├── lib.rs                 # Library root (re-exports all modules)
├── config/                # TOML config parsing (DeploymentConfig), interactive wizard, validation
├── disk/                  # Block device detection, partition layout computation, sfdisk scripting,
│                          #   filesystem formatting, btrfs subvolumes, ZFS pools, LVM thin provisioning
├── install/               # Installer orchestrator (feature-driven pipeline), basestrap execution,
│                          #   chroot mounting, fstab/crypttab generation
├── configure/             # In-chroot system configuration: bootloader (GRUB), encryption (LUKS2/LUKS1),
│                          #   users, locale, mkinitcpio hooks, network services, swap (ZRAM/file),
│                          #   keyfiles, Secure Boot, GPU drivers, packages (Wine/gaming/AUR),
│                          #   session switching scripts, services, greetd
├── desktop/               # Desktop environment package lists and post-install (KDE Plasma, GNOME, XFCE)
├── cleanup/               # Unmount and optional disk wipe
├── rehearsal/             # Full rehearsal installation (write → record → wipe → report)
├── pkgdeps/               # Package dependency tracking (pacman/libalpm backend)
│   ├── model.rs           # Normalized Package, Dep, EdgeKind, DepClosure types
│   ├── source.rs          # MetadataSource trait + MockSource for tests/offline mode
│   ├── pacman.rs          # Production backend: pacman / pactree / expac
│   ├── resolver.rs        # Recursive closure, virtual provider resolution, reverse-dep walking
│   ├── graph.rs           # Graphviz DOT serializer
│   └── cli.rs             # Subcommand handlers and formatters
├── gui/                   # egui wizard panels and app state
│   ├── app.rs             # Main DeploytixGui application
│   ├── state.rs           # WizardStep, DiskState, SystemState, UserState, PackagesState, InstallState
│   ├── theme.rs           # Custom egui theme
│   ├── widgets.rs         # Shared UI widgets
│   └── panels/            # configure, disk_config, disk_selection, handheld_gaming,
│                          #   network_desktop, progress, summary, system_config, user_config
├── resources/             # Embedded resources compiled into the binary
│   ├── audio.rs           # Theme music playback (rodio, WAV)
│   ├── alsa_noop.c        # ABI-correct C shim for ALSA error suppression
│   ├── autostart/         # User autostart scripts
│   └── session_switching/ # greetd session manager, gamescope launcher, PAM configs, IPC scripts
└── utils/                 # CommandRunner (dry-run aware, recording support), DeploytixError (thiserror),
                           #   dependency checker, signal handlers, interactive prompts

src-rehearsal/
└── main.rs                # Standalone rehearsal binary entry point

iso/                       # ISO build scripts and profile for bootable Deploytix media
vendor/                    # Vendored submodules: tkg-gui, gamescope, Modular-1
ref/                       # Original bash installer and mkinitcpio hook reference scripts
docs/                      # Detailed specs: crypto+btrfs integration, crypttab hooks, session switching, etc.
tests/                     # Integration tests: pkgdeps_integration
```

### Key Patterns

**CommandRunner** — All system commands go through `CommandRunner` which supports dry-run mode and optional recording (used by rehearsal to capture every command executed). Use `cmd.run()` for host commands and `cmd.run_in_chroot()` for chroot execution.

**Feature-driven pipeline** — The installer doesn't branch on layout types. `run_phases()` checks feature flags (encryption, LVM thin, subvolumes, preserve_home, gaming, etc.) and each step is a no-op when its feature is disabled.

**Pacman signature recovery** — All chroot `pacman -S` calls go through `pacman_install_chroot()`, which automatically retries with keyring refresh and falls back to relaxed SigLevel on persistent signature failures.

**Signal-safe cleanup** — SIGINT/SIGTERM are caught and trigger emergency cleanup: unmounting filesystems, deactivating LVM, killing orphaned `cryptsetup` processes, and closing LUKS containers.

## Requirements

**Host system (Artix Linux only):**

- `basestrap` and `artix-chroot` (from `artools`)
- `pacman` — package manager
- `sfdisk` — partition table creation (from `util-linux`)
- `mkfs.vfat` (`dosfstools`), `mkfs.ext4` (`e2fsprogs`), and filesystem-specific tools (`btrfs-progs`, `xfsprogs`, `f2fs-tools`)
- `grub-install` / `grub-mkconfig`
- `cryptsetup` (if using encryption)
- `pvcreate` / `vgcreate` / `lvcreate` from `lvm2` (if using LVM Thin)
- Root privileges

Deploytix checks for missing dependencies at startup and offers to install them via `pacman`.

## Development

```bash
cargo build                           # Development build
cargo build --release                 # Release build
cargo build --release --features gui  # GUI build
cargo portable                        # Static musl binary
cargo clippy -- -D warnings           # Lint
cargo fmt -- --check                  # Format check
cargo test --all-features             # Run tests
```

See [BUILD.md](BUILD.md) for detailed build instructions, Makefile targets, release profile settings, and feature flags.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE) for the full text.
