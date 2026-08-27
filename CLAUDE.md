# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Deploytix is an automated Artix Linux deployment installer written in Rust. It provides both an interactive CLI wizard and an egui-based GUI for deploying Artix Linux to removable media and disks. It replaces manual installation sequences (partitioning, encryption, basestrap, chroot configuration) with a single tool supporting multiple init systems, filesystems, desktop environments, LUKS2 encryption, LVM thin provisioning, and btrfs subvolumes.

**Artix-specific**: Requires `basestrap`, `artix-chroot`, and `artools` — these are not available on Arch Linux.

## Build Commands

```bash
cargo build                              # Dev build
cargo build --release                    # Release CLI binary
cargo build --release --features gui     # Release CLI + GUI binary
cargo portable                           # Static musl binary (zero runtime deps)
cargo clippy --all-features -- -D warnings  # Lint (warnings are errors)
cargo fmt -- --check                     # Format check
cargo test --all-features                # Run tests (233: 223 unit + 10 integration)
```

**Makefile shortcuts:**
- `make` / `make build` — release CLI
- `make gui` — release GUI
- `make portable` — static musl build
- `make install` — build GUI + install to `~/.local/bin` (override with `PREFIX=`)
- `make lint` / `make fmt` / `make test`

**Cargo aliases** (defined in `.cargo/config.toml`):
- `cargo gcc-build` — build with glibc linker
- `cargo portable` — build with musl (static)

## Architecture

### 6-Phase Installation Pipeline

`Installer::run()` in `src/install/installer.rs` orchestrates:
1. **Prepare** — compute partition layout, user confirmation
2. **Partition** — generate and apply sfdisk script
3. **Format & Mount** — filesystem creation, LUKS setup, btrfs subvolumes
4. **Basestrap** — install base system packages
5. **Configure** — in-chroot system configuration (bootloader, users, locale, network, services)
6. **Finalize** — mkinitcpio, unmount, close LUKS

The pipeline is feature-driven: each step checks flags (encryption, LVM thin, subvolumes, preserve_home) and no-ops when disabled, rather than branching on layout type.

### Module Responsibilities

| Module | Purpose |
|--------|---------|
| `config/` | TOML config parsing (`DeploymentConfig`), validation, interactive wizard |
| `disk/` | Block device detection, partition layout computation (`ComputedLayout`), sfdisk scripting, formatting |
| `install/` | Installer orchestrator, basestrap, chroot ops, fstab/crypttab generation |
| `configure/` | In-chroot config: bootloader (GRUB), encryption, mkinitcpio hooks, locale, users, network, services, SecureBoot |
| `desktop/` | DE-specific package lists and setup (KDE, GNOME, XFCE, none) |
| `cleanup/` | Unmount and optional disk wipe |
| `gui/` | egui wizard panels (7-step), behind `--features gui` |
| `utils/` | `CommandRunner` (dry-run aware), `DeploytixError`, prompts, signal handlers |

### Key Patterns

**CommandRunner**: All system commands (`mkfs`, `cryptsetup`, `mount`, etc.) go through `CommandRunner` which respects dry-run mode.

- `cmd.run(prog, args)` — host commands, argv (no shell).
- `cmd.run_in_chroot_argv(root, argv)` — **prefer this** for chroot commands.
  No shell, so a username or package name cannot be re-parsed as syntax.
- `cmd.run_in_chroot(root, "…")` — the shell escape hatch, `bash -c`. Use it
  only when the command genuinely needs pipes, redirection, `&&`, or command
  substitution. There is no quoting layer, so anything interpolated in is
  parsed by bash.
- `cmd.run_with_stdin(...)` / `cmd.run_in_chroot_argv_stdin(...)` — for
  credentials. The payload is piped, never placed in argv (where
  `/proc/<pid>/cmdline` exposes it) and never logged or recorded.

**Secrets**: passphrases and passwords are `utils::secret::Secret`, a
`#[serde(transparent)]` string newtype whose `Debug` prints `<redacted>`.
`DeploymentConfig` derives `Debug`, so any new credential field must be
`Secret` or a single `{:?}` will leak it. `Secret` has no `Display`; reaching
the plaintext is always an explicit `.as_str()`.

**Partition Layout Abstraction**: `ComputedLayout` and `PartitionDef` in `disk/layouts.rs` are generic across all layout types. Downstream code (`format_all_partitions()`, `generate_fstab()`, `generate_crypttab()`) works identically for Standard, Minimal, LVM Thin, and Custom layouts. Encryption and LVM are applied as layers, not separate code paths.

**Proportional Partitioning**: Fixed partitions (EFI 512 MiB, Boot 2 GiB, Swap 2×RAM clamped 4–20 GiB) are allocated first; remaining space is distributed by weighted proportions.

**Init System Abstraction**: `InitSystem` enum provides `base_package()`, `service_dir()`, `enabled_dir()`. Package naming follows Artix convention: `{package}-{init}` (e.g., `iwd-runit`).

**Signal-Safe Cleanup**: SIGINT/SIGTERM handlers catch interruptions and automatically unmount filesystems and close LUKS containers.

### Dual Binary Setup

- `src/main.rs` — CLI entry point (always built)
- `src/gui_main.rs` — GUI entry point (only with `--features gui`)
- `src/lib.rs` — library root re-exporting modules for the GUI binary

### Error Handling

`DeploytixError` (thiserror) for domain errors, `anyhow::Result` at the top level. Module operations return `utils::error::Result<T>`.

## Filesystem Rules

### Btrfs Boot Partition

When btrfs is selected for `/boot`, it must use a subvolume:
1. Format as btrfs → 2. Mount → 3. Create `@boot` subvolume → 4. Unmount → 5. Remount with `subvol=@boot`

## CLI Subcommands

```
deploytix                                    # Interactive wizard
deploytix install [-c config] [-d device]    # Install from config or interactive
deploytix list-disks [--all]                 # List available disks
deploytix validate <config>                  # Validate config file
deploytix generate-config [-o file]          # Generate sample config
deploytix cleanup [--device] [--wipe]        # Unmount and optionally wipe
```

Global flags: `-v`/`--verbose` (debug logging), `-n`/`--dry-run` (preview only)

`--dry-run` prints every command instead of running it. It is a **best-effort**
preview: the guarantee comes from `CommandRunner` plus early `is_dry_run()`
returns at the destructive call sites, not from a sandbox. `deploytix rehearse`
is the full-fidelity alternative — it performs a real install on the target and
then wipes it.

## Reference Materials

- `ref/` — original bash installer scripts (implementation reference)
- `docs/` — detailed specs for crypto+btrfs integration, custom mkinitcpio hooks, SecureBoot setup
- `iso/` — scripts and profiles for building bootable Artix ISOs with deploytix pre-installed
- `pkg/PKGBUILD` — Arch packaging for deploytix-git and deploytix-gui-git
