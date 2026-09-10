# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Deploytix is an automated Artix Linux deployment installer written in Rust. It provides both an interactive CLI wizard and an egui-based GUI for deploying Artix Linux to removable media and disks. It replaces manual installation sequences (partitioning, encryption, basestrap, chroot configuration) with a single tool supporting multiple init systems, filesystems, desktop environments, LUKS2 encryption, LVM thin provisioning, and btrfs subvolumes.

**Artix-specific**: Requires `basestrap` and `artix-chroot` — these are not available on Arch Linux. They come from `artools-base`; upstream split `artools` into `artools-base`, `artools-pkg` and `artools-iso`, and deploytix installs all three (`ARTOOLS_PACKAGES` in `src/utils/deps.rs`).

## Build Commands

```bash
cargo build                              # Dev build
cargo build --release                    # Release CLI binary
cargo build --release --features gui     # Release CLI + both GUI binaries
cargo clippy --all-features -- -D warnings  # Lint (warnings are errors)
cargo fmt -- --check                     # Format check
cargo test --all-features                # Run tests
```

**Makefile shortcuts:**
- `make` / `make build` — release CLI
- `make gui` — release build of all four binaries
- `make install` — build CLI + GUI, install the GUI with its desktop entry and
  polkit policy to `$(PREFIX)/bin` (default `PREFIX=/usr`); `install-all` adds
  the CLI, `install-update-gui` adds the updater (immutable machines only)
- `make lint` / `make fmt` / `make test`

The Makefile install targets call `sudo` themselves — do not prefix them with it.

**Cargo alias** (defined in `.cargo/config.toml`):
- `cargo gcc-build` — build with an explicit glibc linker

**No static musl build.** The CLI links `libasound` for theme audio (`rodio`) and
there is no static ALSA to link against, so a self-contained binary is not
achievable. `build.rs` also compiles `src/resources/alsa_noop.c`, so a C compiler
and ALSA headers are build prerequisites. See `BUILD.md`.

## Architecture

### 6-Phase Installation Pipeline

`Installer::run()` in `src/install/installer.rs` orchestrates:
1. **Prepare** — compute partition layout, user confirmation
2. **Partition** — generate and apply sfdisk script
3. **Format & Mount** — filesystem creation, LUKS setup, btrfs subvolumes
4. **Basestrap** — install base system packages
5. **Configure** — in-chroot system configuration (bootloader, users, locale, network, services)
6. **Finalize** — mkinitcpio, unmount, close LUKS

The pipeline is feature-driven: each step checks flags (encryption, LVM thin, subvolumes, immutable root, `disk.recovery.reuse_home`) and no-ops when disabled, rather than branching on layout type.

### Module Responsibilities

| Module | Purpose |
|--------|---------|
| `config/` | TOML config parsing (`DeploymentConfig`), validation, interactive wizard |
| `disk/` | Block device detection, partition layout computation (`ComputedLayout`), sfdisk scripting, formatting |
| `install/` | Installer orchestrator, basestrap, chroot ops, fstab/crypttab generation |
| `configure/` | In-chroot config: bootloader (GRUB), encryption, mkinitcpio hooks, locale, users, network, services, SecureBoot, handheld controller quirks |
| `desktop/` | DE-specific package lists and setup (KDE, GNOME, XFCE, none) |
| `cleanup/` | Unmount and optional disk wipe |
| `gui/` | egui wizard panels (7-step), behind `--features gui` |
| `utils/` | `CommandRunner` (dry-run aware), `DeploytixError`, prompts, signal handlers |

### Key Patterns

**CommandRunner**: All system commands (`mkfs`, `cryptsetup`, `mount`, etc.) go through `CommandRunner` which respects dry-run mode. Use `cmd.run()` for host commands and `cmd.run_in_chroot()` for chroot execution.

**Partition Layout Abstraction**: `ComputedLayout` and `PartitionDef` in `disk/layouts.rs` are generic across all layout types. Downstream code (`format_all_partitions()`, `generate_fstab()`, `generate_crypttab()`) works identically for Standard, Minimal, LVM Thin, and Custom layouts. Encryption and LVM are applied as layers, not separate code paths.

**Proportional Partitioning**: Fixed partitions (EFI 512 MiB, Boot 2 GiB, Swap 2×RAM clamped 4–20 GiB) are allocated first; remaining space is distributed by weighted proportions.

**Swap**: three modes — `partition`, `file_zram` (ZRAM + on-disk swap file),
`zram_only`. The swap file's path is layout-dependent: `/swap/swapfile`
normally, `/var/swap/swapfile` on an immutable root (`/` is read-only, and on
btrfs a snapshot invalidates the physical extents `resume_offset=` names). Its
allocation is filesystem-dependent — `btrfs filesystem mkswapfile` on btrfs,
`fallocate` on ext, `dd` everywhere else, because `swapon` rejects the unwritten
extents `fallocate` leaves on XFS and F2FS. See `docs/SWAP_AUDIT.md`.

**Init System Abstraction**: `InitSystem` enum provides `base_package()`, `service_dir()`, `enabled_dir()`. Package naming follows Artix convention: `{package}-{init}` (e.g., `iwd-runit`).

**Signal-Safe Cleanup**: SIGINT/SIGTERM handlers catch interruptions and automatically unmount filesystems and close LUKS containers.

**Idle Inhibition**: `utils::idle::keep_awake()` returns an RAII guard that holds
every idle inhibitor the host supports — kernel VT blanking off, X screensaver +
DPMS off via `xset`, and an `elogind-inhibit`/`systemd-inhibit` child holding a
`sleep:idle:handle-lid-switch` lock. Every layer is best-effort and releases on
drop. Held for `Installer::run()` and for `deploytix update`/`rollback`; skipped
in dry-run. A blanked screen mid-`basestrap` reads as a hang and gets the machine
power-cycled, so this is data safety, not comfort.

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
deploytix update [pkgs...] [--keep N] [--reboot]   # Transactional update (immutable root)
deploytix rollback [id|@] [--list] [--reboot]      # Roll back to a snapshot set
```

Global flags: `-v`/`--verbose` (debug logging), `-n`/`--dry-run` (preview only)

## Transactional Immutable Root

`immutable_root = true` gives a read-only, integrity-checked OS with atomic
`deploytix update`/`rollback`. Two backends, chosen by the disk layout (they are
mutually exclusive), share the `deploytix update`/`rollback` CLI via runtime
dispatch in `src/main.rs` (`immutable::lvm_ab::detect()`):

**btrfs backend** (requires `install_grub_btrfs`). `/` and `/usr` are mounted
read-only, `/etc` lives on a writable `@etc` subvolume, and `{@, @usr, @etc}` are
snapshotted as atomic sets that roll back together. Updates build a new writable
snapshot set + `pacman` in a chroot, activated on reboot. Boot-pointer changes
regenerate grub.cfg inside a scratch chroot of the target set — never against the
live overlay `/`, where `grub-probe` would fail. `src/immutable/` (snapshot sets,
boot pointer, update/rollback, lockdown). See `docs/IMMUTABLE_SYSTEM.md`.

**LVM A/B backend** (requires `use_lvm_thin`). A/B dual-slot with **dm-verity**
read-only roots: two root LVs (`root_a`/`root_b`, each including `/usr`) alternate,
integrity-checked against a per-slot hash LV. `/etc` is a writable overlay;
`/var`/`home` are shared. `deploytix update` rsyncs the active root into the
inactive slot, `pacman`s it in a chroot, `veritysetup format`s a fresh hash, and
repoints the boot pointer (a sed of `deploytix.slot=`/`deploytix.roothash=` in
`grub.cfg` — no grub-mkconfig). `src/immutable/lvm_ab.rs`, `src/configure/verity.rs`,
the `verity-ab` hook in `src/configure/hooks.rs`. See `docs/IMMUTABLE_LVM_AB.md`.

Both block direct `pacman -Syu` via the read-only `/usr` plus a `/etc/profile.d`
interactive nudge (not a pacman hook, which would break `basestrap`/`pacman -r`
image builds and deploys).

**Per-image pacman database.** `/var` is shared across every set/slot, so a
database living at `/var/lib/pacman` described the newest state while the files
rolled back — `pacman -Qkk` then reported missing files for packages it claimed
were installed. The database is therefore stored at `/usr/lib/sysimage/pacman`
(inside `@usr` on btrfs, inside the root LV on LVM A/B) and bind-mounted back
onto `/var/lib/pacman` via an fstab `bind,nofail` entry, so it rolls back with
the system. `DBPath` stays at its default — the bind is the whole mechanism.
Older installs are migrated inside the next transaction without touching the
running system's copy. `src/immutable/pacman_db.rs`.

**Composing updates within a session.** Both backends distinguish what is
*running* (from `/proc/cmdline`: `rootflags=subvol=` or `deploytix.slot=`) from
what is *staged* for the next boot (the boot pointer), via
`immutable::SessionState::pending()`. A second `deploytix update` before
rebooting builds on the staged set/slot rather than beside it, so the two
compose. On the A/B backend the build target is derived from the running slot
and asserted never to be it — building into the running slot would mount a live
dm-verity data device read-write. `deploytix rollback` with no argument discards
a staged set and returns to the running one. Transactions are serialised by an
flock on `/run/deploytix-update.lock`. See `docs/IMMUTABLE_SET_COMPOSITION.md`.

## Working Directories

deploytix never writes its working files to `/tmp`. A *deployed immutable* host
has no writable `/tmp` on the LVM A/B backend (`/` is a read-only dm-verity image
and only `/etc` is overlaid), and a `/tmp` path is invisible from inside a
transactional chroot, which rbinds only `/var`, `/home` and `/boot`. Two homes,
picked by `utils::paths`:

- `/run/deploytix` (`runtime_path`) — mount points and generated scripts: small,
  root-only, worthless after a reboot.
- `/var/cache/deploytix` (`cache_path`) — the local `[deploytix]` repo
  (`repo/`) and the generated `pacman.conf` passed to `basestrap -C`. Large, and
  on the one filesystem a transactional chroot can see.

The ISO build script follows the same rule: its package staging directory sits
beside the artools workspace (so `-w` moves it onto the build disk), never
`/tmp`.

## Reference Materials

- `ref/` — original bash installer scripts (implementation reference)
- `docs/` — detailed specs for crypto+btrfs integration, custom mkinitcpio hooks, SecureBoot setup
- `iso/` — scripts and profiles for building bootable Artix ISOs with deploytix pre-installed
- `pkg/PKGBUILD` — Arch packaging for deploytix-git and deploytix-gui-git
