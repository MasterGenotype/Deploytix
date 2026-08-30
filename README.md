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

Running the GUI on an X11 session additionally requires `libX11`, `libXcursor`, and `libxkbcommon-x11` at runtime (winit loads them dynamically). Artix/Arch: `pacman -S libx11 libxcursor libxkbcommon-x11`; Debian/Ubuntu: `apt install libx11-6 libxcursor1 libxkbcommon-x11-0`.

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

# Immutable-root systems (see "Living with a Deployed System")
deploytix update [pkgs...] [--keep N] [--reboot]    # Transactional update
deploytix rollback [id|@] [--list] [--reboot]       # Roll back to a snapshot set

# Global flags
deploytix -v ...       # Verbose output
deploytix -n ...       # Dry-run (preview; applies to update/rollback)
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

**Phase 5 — Desktop & Packages.** Installs the selected desktop environment with display manager. Then conditionally installs Wine, gaming packages (Steam, gamescope), session switching scripts, yay AUR helper, AUR packages, btrfs tools, sysctl tweaks, handheld controller quirks, Handheld Daemon, Decky Loader, and evdevhook2.

**Phase 6 — Finalize.** Regenerates the initramfs, unmounts all filesystems in reverse order, exports ZFS pools if applicable, and closes all LUKS containers.

## Living with a Deployed System

What day-to-day life looks like on a machine deploytix installed depends on whether you enabled snapshots and the immutable root.

### Standard install (no grub-btrfs)

A conventional system: `/` is read-write, you update with `pacman -Syu`, and there's nothing special to learn. If you chose btrfs, your data still lives on subvolumes (`@`, `@home`, `@var`, …) so you *can* add snapper later, but nothing is enforced.

### With snapshots (`install_grub_btrfs`)

`snapper` takes read-only snapshots of `/` (`@`) and GRUB grows an **"Artix Linux snapshots"** submenu. If an update breaks the system you can pick an older snapshot from GRUB to boot into it. Booting a read-only snapshot layers a **disk-backed ephemeral overlay** over it, so the system is usable but changes made while booted into a snapshot are discarded on reboot. `/usr`, `/var`, and `/home` remain live. This is a recovery aid, not full immutability.

### Transactional immutable root (`immutable_root`)

This is the flagship mode — openSUSE MicroOS/Aeon-style semantics on Artix. Expect the following:

- **`/` and `/usr` are read-only.** `/lib`, `/lib64`, `/bin`, `/sbin` are symlinks into `/usr`, so they're covered too. `/etc` is a **writable** subvolume (`@etc`); `/var` and `/home` are writable and persistent. Stray writes to the rest of `/` (e.g. `/tmp`, `/root`) go to an **ephemeral overlay** and are cleared on reboot.
- **You don't run `pacman -Syu` directly** — `/usr` is read-only, so it can't succeed anyway. Interactive shells get a friendly nudge toward `deploytix update` before it even tries; use `deploytix update` instead.
- **Updates are transactional and atomic.** `sudo deploytix update` builds a *new* snapshot set from the current one, runs the upgrade **inside** it, and only switches to it on the **next reboot**. If an update fails midway, the half-built set is discarded and your running system is untouched. Add package names to install them (`deploytix update firefox`), `--keep N` to control how many old sets are retained, `--reboot` to reboot automatically.
- **Rollback is instant and reversible.** `sudo deploytix rollback --list` shows your snapshot sets; `sudo deploytix rollback <id>` (or `@` for the base install) repoints the next boot. Nothing is deleted, so you can roll "forward" again. Each set restores `{@, @usr, @etc}` together, so the whole OS state is consistent — no "new config on old binaries" skew.
- **Everyday example:**

  ```bash
  sudo deploytix update              # stage a full upgrade in a new set
  sudo reboot                        # activate it
  # ...something's off?
  sudo deploytix rollback            # step back to the previous set
  sudo reboot
  ```

- **Recovery if a staged update won't boot:** pick the previous entry (or a `snapshot`) from the GRUB menu, then `sudo deploytix rollback` to make it the default. `/boot` (kernel + initramfs) is a shared partition, so a rollback restores userspace but keeps the most recently installed kernel — the boot machinery is version-independent, so this is safe; only kernel *contents* aren't rolled back.

- **Graphical updater:** immutable deployments also get **Deploytix Update** (`deploytix-update-gui`, in the application menu), which drives the same transactional machinery — full upgrade, install repo packages or local `.pkg.tar.zst` files, and a snapshot list showing exactly which packages each update added, upgraded or removed, with rollback per entry. Every update (from the GUI *or* the CLI) records its package diff under `/var/lib/deploytix/history/`, which is what makes that list possible: the pacman database lives on the shared `/var` and is not snapshotted, so nothing else on disk knows what a given snapshot changed. It ships as its own package and is installed **only** when `immutable_root` is set — a mutable deployment never receives the binary, the desktop entry or the polkit action.

See **[docs/IMMUTABLE_SYSTEM.md](docs/IMMUTABLE_SYSTEM.md)** for the full model (subvolume roles, the `.deploytix-pair` marker, the boot pointer, and caveats).

#### On LVM thin: A/B dual-slot with dm-verity

The description above is the **btrfs** backend. If you enable `immutable_root` on an **LVM thin** layout (`use_lvm_thin = true`) you get the LVM-native backend instead — same `deploytix update`/`rollback` commands, different mechanism:

- Two root logical volumes (`root_a`/`root_b`, each including `/usr`) alternate. The active slot is **read-only and dm-verity integrity-checked** — on-disk tampering causes an I/O error, not corrupt data.
- `deploytix update` builds the **inactive** slot (rsync the running root → seal it with a fresh dm-verity hash) and flips the boot pointer on reboot; `deploytix rollback` flips back. The running slot is never touched.
- `/etc` is a writable overlay; `/var` and `/home` are shared and persistent.
- Recovery: no auto boot-count fallback — edit `deploytix.slot`/`deploytix.roothash` at the GRUB prompt, or boot the good slot and `deploytix rollback`.

See **[docs/IMMUTABLE_LVM_AB.md](docs/IMMUTABLE_LVM_AB.md)** for the full A/B model.

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
display_manager = "greetd"     # greetd (auto-login), sddm, gdm, lightdm, none

[packages]
install_yay = true             # AUR helper (built from source)
install_wine = true            # Wine compatibility layer
install_gaming = true          # Steam, gamescope (Bazzite fork)
install_session_switching = true  # gamescope ↔ desktop via greetd
install_btrfs_tools = true     # snapper, btrfs-assistant (via yay)
install_grub_btrfs = true      # snapshot boot menu + snapper root config (btrfs only)
sysctl_gaming_tweaks = true    # vm.max_map_count, swappiness, etc.
sysctl_network_performance = true  # BBR, fq, larger buffers
install_hhd = true             # Handheld Daemon (gamepad remapping, TDP)
install_decky_loader = true    # Steam plugin framework
install_evdevhook2 = true      # Cemuhook UDP motion server
# handheld_controller_quirks   # omit = auto-detect Legion Go family; true/false to force
gpu_drivers = ["amd"]          # nvidia, amd, intel
```

### Partition Configuration

EFI (512 MiB), Boot (2 GiB), and Swap (when `swap_type = "partition"`) are always auto-prepended. You define your data partitions in `[[disk.partitions]]`:

- `mount_point` (required) — absolute path, e.g. `/`, `/home`, `/var`. Cannot be `/boot` or `/boot/efi`.
- `size_mib` (required) — size in MiB. Exactly one partition may use `0` to fill remaining space.
- `label` (optional) — partition label. Derived from mount point if omitted (`/home` → `HOME`).
- `encryption` (optional) — per-partition encryption override. Inherits from `disk.encryption` when omitted.

Default partitions when none are specified: `/` (20 GiB), `/usr` (30 GiB), `/var` (10 GiB), `/home` (remainder).

> **Not sure how big your target disk should be?** See
> [docs/DISK_SPACE_GUIDE.md](docs/DISK_SPACE_GUIDE.md) — a tutorial on sizing
> recommendations by installation media (USB/removable, SSD/NVMe, HDD) and
> feature set (desktop environment, gaming/handheld stack, encryption, btrfs
> subvolumes).

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
- **Gamescope Updates** — Deploys `deploytix-update-gamescope` (with an "Update Gamescope" desktop entry) which rebuilds gamescope from the same fork with the exact same meson options every time. AUR rebuilds of gamescope break the Steam session, so a pacman hook aborts any gamescope update not made through this tool and `IgnorePkg = gamescope-git` keeps `-Syu` from touching it. On immutable installs it detects the read-only root and installs the rebuilt package through `deploytix update` (active on the next reboot) instead of `pacman -U`.
- **Session Switching** — Deploys greetd-based scripts for switching between a gamescope (Steam Deck-style) session and a desktop session. Includes `deploytix-session-manager`, `session-select`, `return-to-gamemode`, PAM configs, and a `steamos-session-select` compatibility symlink.
- **Handheld Daemon (HHD)** — Gamepad remapping, TDP control, per-game profiles (AUR: `hhd-git`). Writes init-specific service files.
- **Decky Loader** — Steam plugin framework (AUR: `decky-loader-bin`). Writes init-specific service files.
- **evdevhook2** — Cemuhook UDP motion server for DualShock/DualSense/Joy-Con controllers (AUR: `evdevhook2-git`). Installs udev rules and service files.
- **Handheld Controller Quirks** — Stops the controllers on Lenovo Legion Go family handhelds (Legion Go, Legion Go 2, Legion Go S) repeatedly disconnecting and reconnecting: pins USB runtime power management off for the pads, binds them to `xpad` on kernels that predate their IDs, and opens their hidraw nodes to the session user. Applied automatically when the installing host's DMI identifies one of those machines; `handheld_controller_quirks = true`/`false` forces the decision. See `docs/HANDHELD_CONTROLLER_QUIRKS.md`.
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
vendor/                    # Vendored submodules: tkg-gui, gamescope
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
