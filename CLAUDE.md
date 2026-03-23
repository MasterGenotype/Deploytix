# CLAUDE.md

Guidance for AI assistants working with the Deploytix codebase.

## Project Overview

Deploytix is a Rust-based automated installer for **Artix Linux** that deploys to removable media and disks. It provides both CLI and GUI (egui) interfaces with TOML-driven configuration for reproducible deployments.

**Version**: 1.2.0 | **License**: MIT | **Rust Edition**: 2021

## Build Commands

```bash
# Development
cargo build                              # Debug CLI build
cargo build --features gui               # Debug CLI + GUI build

# Release
make build                               # Release CLI (cargo build --release)
make gui                                 # Release GUI (--features gui)
make portable                            # Static musl binary (x86_64-unknown-linux-musl)
make gcc                                 # glibc target (x86_64-unknown-linux-gnu)

# Installation
make install                             # GUI + CLI to /usr/bin with .desktop & polkit
make install-cli                         # CLI only
make install-all                         # Both binaries + desktop + polkit
make install-portable                    # Portable musl binary

# Code quality
make fmt                                 # cargo fmt
make lint                                # cargo clippy --all-features -- -D warnings
make test                                # cargo test --all-features
make clean                               # cargo clean
```

## Testing

No test suite currently exists. When tests are added:

```bash
cargo test                    # Run all tests
cargo test --lib              # Unit tests only
cargo test --test integration # Integration tests
cargo test --all-features     # Include GUI tests
```

## Architecture

### Directory Structure

```
src/
├── main.rs              # CLI entry point (clap subcommands)
├── gui_main.rs          # GUI entry point (--features gui)
├── lib.rs               # Library root, re-exports for GUI binary
├── config/              # TOML parsing, interactive wizard, validation
│   └── deployment.rs    # DeploymentConfig, DiskConfig, SystemConfig
├── disk/                # Block device detection, partitioning, formatting
│   ├── detection.rs     # lsblk parsing, device info
│   ├── layouts.rs       # Partition layout computation
│   ├── partitioning.rs  # sfdisk scripting
│   ├── formatting.rs    # mkfs, btrfs subvolumes
│   ├── lvm.rs           # LVM thin provisioning
│   └── volumes.rs       # Volume management
├── install/             # 6-phase installer pipeline
│   ├── installer.rs     # Main Installer struct, run() method
│   ├── basestrap.rs     # Artix base system installation
│   ├── chroot.rs        # artix-chroot operations
│   ├── fstab.rs         # Filesystem table generation
│   └── crypttab.rs      # Encryption table generation
├── configure/           # In-chroot system configuration
│   ├── bootloader.rs    # GRUB installation and kernel params
│   ├── encryption.rs    # LUKS2/LUKS1 setup, multi-volume
│   ├── keyfiles.rs      # Automatic keyfile unlocking
│   ├── mkinitcpio.rs    # Initramfs hook generation
│   ├── users.rs         # User creation, groups
│   ├── locale.rs        # Timezone, locale, keymap
│   ├── network.rs       # iwd/NetworkManager config
│   ├── services.rs      # Init system service enablement
│   ├── swap.rs          # ZRAM/swapfile setup
│   ├── secureboot.rs    # Secure Boot signing
│   ├── hooks.rs         # Custom mkinitcpio hooks
│   └── packages.rs      # Package lists by init system
├── desktop/             # Desktop environment installers (KDE, GNOME, XFCE, none)
├── gui/                 # egui-based GUI wizard (app.rs, panels.rs)
├── cleanup/             # Unmount/wipe operations
├── resources/           # Embedded templates and resources
└── utils/               # Shared utilities
    ├── command.rs       # CommandRunner (dry-run aware)
    ├── error.rs         # DeploytixError enum (thiserror)
    ├── deps.rs          # Host dependency checker
    ├── signal.rs        # SIGINT/SIGTERM handlers
    └── prompt.rs        # Interactive user prompts
```

### Installation Pipeline

`Installer::run()` orchestrates 6 phases:
1. **Prepare** — Validate config, compute partition layout, user confirmation
2. **Partition** — GPT partitioning via sfdisk
3. **Format & Mount** — mkfs, optional LUKS encryption, btrfs subvolumes
4. **Basestrap** — Install base system with `basestrap` (Artix-specific)
5. **Configure** — In-chroot setup: bootloader, users, locale, services, network
6. **Finalize** — mkinitcpio regeneration, unmount, close LUKS

### Partition Layouts

Defined in `disk/layouts.rs` via `ComputedLayout`:
- **Standard** — 7-partition: EFI, Boot, Swap, Root, Usr, Var, Home
- **Minimal** — 3-partition: EFI, Swap, Root
- **LVM Thin** — LVM thin provisioning layout
- **Custom** — User-defined partitioning
- **CryptoSubvolume** — Multi-volume LUKS2 with separate encrypted partitions

## Key Patterns & Conventions

### CommandRunner

**All external commands must go through `CommandRunner`**, which respects dry-run mode:

```rust
let cmd = CommandRunner::new(dry_run);
cmd.run("sfdisk", &["/dev/sda"])?;           // Direct execution
cmd.run_in_chroot("/mnt", "mkinitcpio -P")?; // Chroot execution
```

- Returns `Ok(None)` in dry-run mode, `Ok(Some(output))` when executed
- Checks `signal::is_interrupted()` before running

### Error Handling

- Domain errors: `DeploytixError` enum with `thiserror` derive
- Type alias: `type Result<T> = std::result::Result<T, DeploytixError>`
- Top-level: `anyhow::Result` for flexibility
- All error variants must have descriptive messages

### Signal Safety

- Check `signal::is_interrupted()` before long-running operations
- Custom atomic-based signal handlers prevent partial installations

### Logging

- Use `tracing::{info, debug, warn, error}` — not `println!`
- `colored` crate for CLI output formatting (separate from logging)
- `--verbose` flag enables debug-level tracing

### Configuration

- TOML-based with `serde` Serialize/Deserialize
- Comprehensive defaults for all settings
- Interactive wizard fallback via `dialoguer`
- Validation runs before installation begins

### Module Organization

- Each subsystem is a module with internal submodules
- Public re-exports via `pub use` in `mod.rs`
- No circular dependencies between modules

## Artix-Specific Notes

This is an **Artix Linux** project, not Arch:
- Uses `basestrap` (from `artools` package) instead of `pacstrap`
- Uses `artix-chroot` when available, falls back to plain `chroot`
- Package naming follows Artix convention: `{package}-{init}` (e.g., `iwd-runit`)
- Supports 4 init systems: runit, OpenRC, s6, dinit
- `InitSystem` enum provides `base_package()`, `service_dir()`, `enabled_dir()` methods

## Feature Flags

```toml
[features]
gui = ["dep:eframe", "dep:egui"]
```

Build with `--features gui` or `--all-features` to include the GUI binary.

## Release Build Profile

Release builds are optimized for size:
- `opt-level = "z"` (size optimization)
- `lto = true` (link-time optimization)
- `codegen-units = 1` (maximum LTO effectiveness)
- `panic = "abort"` (no unwinding)
- `strip = true` (remove debug symbols)

## CI/CD

GitHub Actions workflow (`.github/workflows/release.yml`) on version tags (`v*`):
1. **Lint** — `cargo fmt --check` + `cargo clippy --all-features -D warnings`
2. **Build CLI** — Static musl binary
3. **Build GUI** — glibc with X11/Wayland support
4. **Release** — Publish artifacts to GitHub Releases

Both formatting and clippy must pass before release.

## Filesystem Rules

### Btrfs Boot Partition

When btrfs is the boot filesystem:
1. Format boot partition as btrfs
2. Mount to temporary mountpoint
3. Create `@boot` subvolume
4. Unmount
5. Remount with `subvol=@boot`

## CLI Subcommands

```bash
deploytix install [--config FILE] [--device /dev/sdX]
deploytix list-disks [--all]
deploytix validate FILE
deploytix generate-config [--output FILE]
deploytix cleanup [--device /dev/sdX] [--wipe]
deploytix generate-desktop-file [--de kde|gnome|xfce|none]
```

Global flags: `--verbose` (debug logging), `--dry-run` / `-n` (simulate only)

## Reference Materials

- `ref/` — Original bash implementation scripts
- `docs/INTEGRATION_GUIDE_CRYPTO_BTRFS.md` — Multi-volume LUKS + btrfs
- `docs/CRYPTTAB_HOOKS_DOCUMENTATION.md` — Custom mkinitcpio hooks
- `docs/SECUREBOOT_GRUB_SETUP.md` — Secure Boot implementation
- `docs/PROJECT_OVERVIEW.md` — High-level architecture
- `docs/test-coverage-proposal.md` — Testing strategy
