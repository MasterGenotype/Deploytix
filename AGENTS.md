# AGENTS.md

This file provides guidance to WARP (warp.dev) when working with code in this repository.

## Build Commands

```bash
# Development build
cargo build

# Release build
cargo build --release

# Build with GUI (egui-based)
cargo build --release --features gui

# Static portable binary (musl) - single binary, no dependencies
cargo portable
# Equivalent to: cargo build --release --target x86_64-unknown-linux-musl

# Code quality
cargo clippy -- -D warnings
cargo fmt -- --check
```

## Testing

No test suite currently exists. When implementing tests:

```bash
cargo test                    # Run all tests
cargo test --lib              # Unit tests only
cargo test --test integration # Integration tests
```

## Architecture

### Module Structure

- **`config/`** - TOML configuration parsing (`DeploymentConfig`) and interactive wizard
- **`disk/`** - Block device detection, partition layout computation, sfdisk scripting, filesystem formatting
- **`install/`** - `Installer` orchestrator, basestrap execution, chroot operations, fstab/crypttab generation
- **`configure/`** - System configuration in chroot: bootloader, users, locale, mkinitcpio hooks, network services
- **`desktop/`** - Desktop environment package lists and post-install setup (KDE, GNOME, XFCE)
- **`cleanup/`** - Unmount and optional disk wipe operations
- **`utils/`** - `CommandRunner` (dry-run aware), error types (`DeploytixError`), interactive prompts

### Key Patterns

**CommandRunner**: All system commands go through `CommandRunner` which respects dry-run mode. Use `cmd.run()` for external commands and `cmd.run_in_chroot()` for chroot execution.

**Installation Flow**: The `Installer::run()` method orchestrates the 6-phase installation:
1. Prepare (compute layout, user confirmation)
2. Partition disk (sfdisk)
3. Format + mount (or LUKS setup for CryptoSubvolume)
4. Basestrap (install base system)
5. Configure system (in chroot)
6. Finalize (mkinitcpio, unmount, close LUKS)

**Partition Layouts**: Computed in `disk/layouts.rs` using `ComputedLayout`. Three layouts exist:
- `Standard`: 7-partition (EFI, Boot, Swap, Root, Usr, Var, Home)
- `Minimal`: 3-partition (EFI, Swap, Root)
- `CryptoSubvolume`: Multi-volume LUKS2 with separate encrypted partitions for Root/Usr/Var/Home

**Init System Abstraction**: `InitSystem` enum provides `base_package()`, `service_dir()`, `enabled_dir()` methods for runit/OpenRC/s6/dinit differences.

### Artix-Specific Notes

- Uses `basestrap` (from artools) instead of pacstrap for base system installation
- Uses `artix-chroot` when available, falls back to plain chroot
- Package naming follows Artix convention: `{package}-{init}` (e.g., `iwd-runit`, `grub-runit`)
- mkinitcpio hooks differ from Arch; custom hooks in `configure/hooks.rs` for encryption

### Error Handling

Uses `DeploytixError` (thiserror) for domain errors and `anyhow::Result` at the top level. Module-specific operations return `utils::error::Result<T>`.

## Reference Materials

The `ref/` directory contains shell script references from the original implementation:
- `Deploytix` - Original bash installer
- `hooks_*`, `install_*` - mkinitcpio hook implementations for crypttab-unlock
- `artix-chroot` - Reference chroot script

The `docs/` directory contains detailed specifications:
- `INTEGRATION_GUIDE_CRYPTO_BTRFS.md` - Multi-volume LUKS + BTRFS encryption implementation
- `CRYPTTAB_HOOKS_DOCUMENTATION.md` - Custom mkinitcpio hooks for keyfile-based unlocking
