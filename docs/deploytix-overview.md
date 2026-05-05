# Deploytix — Technical Overview

## Table of Contents

1. [Project Summary](#project-summary)
2. [Core Components](#core-components)
3. [Component Interactions](#component-interactions)
4. [Deployment Architecture](#deployment-architecture)
5. [Runtime Behaviour](#runtime-behaviour)
6. [Installation Pipeline](#installation-pipeline)

## Project Summary

Deploytix is a Rust-based automated installer for Artix Linux that replaces the manual `basestrap → artix-chroot → grub-install` sequence with a single tool. It produces working systems on either removable media or fixed disks, supports four init systems (runit / openrc / s6 / dinit), five filesystems (btrfs / ext4 / xfs / zfs / f2fs), LUKS2 multi-volume encryption with optional LUKS1 `/boot` and dm-integrity, LVM thin provisioning, btrfs subvolumes, ZRAM/swap-file/swap-partition combinations, three SecureBoot enrollment methods, and a large optional package roster (Wine, Steam + gamescope, Decky Loader, Handheld Daemon, evdevhook2, sysctl tweaks). It ships as three binaries — `deploytix` (CLI + interactive wizard), `deploytix-gui` (egui 3-step wizard), `deploytix-rehearsal` (full installer + automatic disk wipe for testing) — all backed by a single library crate (`src/lib.rs`).

## Core Components

### Library root

- **`src/lib.rs`** — re-exports the public module tree (`cleanup`, `config`, `configure`, `desktop`, `disk`, `install`, `pkgdeps`, `rehearsal`, `resources`, `utils`) plus an opt-in `gui` module behind the `gui` Cargo feature.

### Configuration

- **`src/config/deployment.rs`** — `DeploymentConfig` is the entire configurable surface (disk, system, user, network, desktop, packages). Validated by `DeploymentConfig::validate()` against 16+ cross-field rules (encryption requires password, integrity requires encryption, LVM thin not allowed with ZFS, `preserve_home` requires `/home` partition or subvolumes, session-switching requires gaming + DE, etc.). `from_file()`, `from_wizard(device)`, `sample()` are the three constructors.
  - Key sub-types: `DiskConfig`, `SystemConfig`, `UserConfig`, `NetworkConfig`, `DesktopConfig`, `PackagesConfig`, `CustomPartitionEntry`.
  - Enum sets: `Filesystem` (Btrfs default), `InitSystem` (Runit default), `Bootloader` (Grub only), `NetworkBackend` (Iwd default), `DesktopEnvironment`, `SwapType`, `SecureBootMethod`, `GpuDriverVendor`.

### Disk subsystem

- **`src/disk/layouts.rs`** — `ComputedLayout { partitions: Vec<PartitionDef>, subvolumes, planned_thin_volumes, total_mib }`. Single computation function `compute_layout_from_config()` builds the layout from `DiskConfig` + disk size: prepends EFI (512 MiB) + Boot (2 GiB) + optional Swap (`2 × RAM` clamped to 4–20 GiB), then user partitions in order. Encryption flags are applied as a post-pass (`apply_encryption_flags`); btrfs subvolumes are attached to data partitions by mount point (`@`, `@home`, `@usr`, `@var`, `@log`); LVM thin collapses all data partitions into a single LVM PV (`apply_lvm_thin_to_layout`).
- **`src/disk/partitioning.rs`** — `apply_partitions()` writes the GPT via `sfdisk` with a script that respects the device's logical block size (read from `/sys/block/<dev>/queue/logical_block_size`, falls back to 512). 1 MiB-aligned partition starts; LegacyBIOSBootable attribute set on the BOOT partition.
- **`src/disk/formatting.rs`** — wraps `mkfs.{vfat,ext4,btrfs,xfs,f2fs}` and `zpool` calls; handles btrfs-subvolume creation/mount; re-exports `ZFS_DATASETS` and `ZFS_BOOT_DATASET` for fstab generation.
- **`src/disk/lvm.rs`** — pvcreate / vgcreate / lvcreate (thin pool + thin LVs), `lv_path()`, `deactivate_vg()`.
- **`src/disk/detection.rs`** — `list_block_devices(all)` from `lsblk`, `BlockDevice { path, size_bytes, model, device_type }`, `partition_path(device, n)` (handles `nvme`/`mmcblk` `p`-suffix), `get_ram_mib()` from `/proc/meminfo`.
- **`src/disk/volumes.rs`** — multi-volume LUKS orchestration helpers used by the installer.

### Install pipeline

- **`src/install/installer.rs`** — `Installer` struct holds `config`, `cmd: CommandRunner`, computed `layout`, vectors of `LuksContainer` / `VolumeKeyfile` / `ThinVolumeDef`, optional progress callback, optional rehearsal recorder. `Installer::run()` installs SIGINT/SIGTERM handlers, runs `prepare()`, then dispatches the rest of the pipeline through `run_phases()` with emergency cleanup on any error or interrupt.
- **`src/install/basestrap.rs`** — builds the package list, ensures the custom `[deploytix]` repo is reachable (ISO embedded → pre-built `.pkg.tar.zst` cache → makepkg from source → temporary local repo + generated pacman.conf), ensures Arch `[extra]` repo is added when needed, runs `basestrap` with retry-on-network-error (max 3 attempts, 5 s delay).
- **`src/install/chroot.rs`** — `mount_partitions`, `mount_partitions_preserve`, `mount_partitions_zfs`, `mount_boot_btrfs_subvolume`, `unmount_all` (deepest-first sort). Handles three mount modes: regular, btrfs-subvolume, ZFS-dataset.
- **`src/install/fstab.rs`** — three fstab generators (`generate_fstab`, `generate_fstab_multi_volume`, `generate_fstab_lvm_thin`). Sets `pass=1` only for ext4 root, `pass=2` for other ext4 mounts, `pass=0` for everything else (btrfs/xfs/f2fs/zfs).
- **`src/install/crypttab.rs`** — generates `/etc/crypttab` with mapper-name → UUID → keyfile entries; LUKS1 `/boot` always uses `luks,discard` (no integrity); LUKS2 data uses `luks` (integrity enabled) or `luks,discard` (no integrity).

### Configure (in-chroot)

All modules under `src/configure/` operate on the chroot at `/install`:

- **`bootloader.rs`** — GRUB EFI install. Standalone EFI binary with embedded modules (including `cryptodisk luks luks2 gcry_*`). Generates `/etc/default/grub` with the right `cryptdevice=` / `rd.luks.uuid=` / `resume=` parameters per layout. Installs a pacman hook that reinstalls GRUB on kernel/grub updates (required for encrypted systems).
- **`mkinitcpio.rs`** — feature-driven HOOKS construction. Multi-LUKS uses `crypttab-unlock + mountcrypt` (custom hooks, no `filesystems` hook). LVM thin uses `encrypt + filesystems + usr` (+ `crypttab-unlock` if `boot_encryption`). Plain encryption uses `encrypt + filesystems`. Hooks `keyboard keymap consolefont`, `lvm2`, `btrfs`/`zfs`, `usr`, and `resume` are added conditionally. MODULES include `vfat fat nls_cp437 nls_iso8859_1` always, plus `dm_crypt dm_mod dm_integrity dm_thin_pool` per feature. FILES include `/etc/crypttab` and per-volume keyfiles for multi-LUKS.
- **`hooks.rs`** — generates the custom `crypttab-unlock` and `mountcrypt` initcpio hooks (runtime + install scripts) when multi-LUKS or LVM thin + boot encryption are active.
- **`encryption.rs`** — `LuksContainer { device, mapper_name, mapped_path, volume_name }`. `setup_multi_volume_encryption()`, `setup_single_luks()`, `setup_single_luks_with_integrity()`, `setup_boot_encryption()` (LUKS1 + pbkdf2 + sha512 for GRUB compatibility), `close_multi_luks()` (reverse-order close), `resolve_mapper_name()` (disambiguates active names by appending `-1`…`-99`).
- **`keyfiles.rs`** — generates 512-byte keyfiles in `/etc/cryptsetup-keys.d/cryptroot.key` etc., adds them via `cryptsetup luksAddKey`, sets mode `000` on files / `700` on directory.
- **`locale.rs`** — locale.conf, locale.gen, vconsole.conf, hwclock; `create_dinit_keymap_service` for dinit (other inits handle keymap natively).
- **`users.rs`** — useradd inside chroot, sets sudoer status by appending to `/etc/sudoers.d/wheel`.
- **`network.rs`** — writes iwd or NetworkManager configs; iwd needs `EnableNetworkConfiguration=true`; NetworkManager wraps iwd as backend.
- **`services.rs`** — `enable_service()` per init: runit creates `runsvdir/default/<svc>` symlink; openrc runs `rc-update add <svc> default` in chroot; s6 writes a touch file in `adminsv/default/contents.d` to a `<svc>-srv` directory; dinit creates `boot.d/<svc>` symlink. Notably blacklists installation of `elogind-<init>` (conflicts with `seatd-<init>`) and skips enabling `elogind` (only the base PAM module is needed). `pacman -S --needed` installs the `<svc>-<init>` packages before enabling.
- **`greetd.rs`** — writes `/etc/greetd/config.toml`, autologin tweaks, S6 directory layout (no `greetd-s6` package exists in Artix repos).
- **`session_switching.rs`** — installs gamescope ↔ desktop session manager scripts from `src/resources/session_switching/` into the chroot (greetd-greeter PAM, deploytix-session-manager.sh, return-to-gamemode.sh, etc.).
- **`packages.rs`** (largest, ~1500 lines) — Wine, gaming (Steam + gamescope), yay (built from source), AUR packages (zen-browser), btrfs tools (snapper, btrfs-assistant), HHD, Decky Loader, evdevhook2, sysctl gaming/network tweaks, GPU drivers (NVIDIA/AMD/Intel), user autostart entries, and a chroot-aware `pacman_install_chroot()` helper.
- **`secureboot.rs`** — three implementations: sbctl (auto), manual keys (PK/KEK/db), shim + MOK enrollment.
- **`swap.rs`** — ZRAM (fixed 4 GiB device), swap file (allocated by btrfs `truncate + chattr +C` or ext4 `fallocate`), per-init service files.

### Desktop (in-chroot)

- **`src/desktop/{kde,gnome,xfce,none}.rs`** — DE-specific basestrap-additional package lists and post-install configuration (display manager hookup, default applications). `desktop::generate_desktop_file(&de, &bindir)` produces a `.desktop` file for the GUI launcher.

### Cleanup

- **`src/cleanup/mod.rs`** — `Cleaner::cleanup(device, wipe)`: unmounts everything under `/install` (deepest-first), kills orphaned cryptsetup processes (PPID==1 detection), closes all `/dev/mapper/Crypt-*` and `/dev/mapper/temporary-cryptsetup-*` mappings, optionally writes blank GPT (sfdisk → fdisk fallback).

### Rehearsal

- **`src/rehearsal/mod.rs`** — `run_rehearsal(&config) -> RehearsalReport`: creates a recording channel, arms a `DiskWipeGuard`, runs the real installer with `skip_confirm = true`, drains the recorder channel, calls `wipe_guard.wipe_now()`, returns the report.
- **`src/rehearsal/guard.rs`** — `DiskWipeGuard { device, armed }` with `Drop` impl that performs the cleanup-+-wipe sequence (unmount → close LUKS → wipefs → blank GPT) unconditionally if still armed. Disarmed only after a successful explicit `wipe_now()`.
- **`src/rehearsal/report.rs`** — `RehearsalReport { records, short_circuited_at, disk_wiped, total_duration }` with three renderers: `print_table()` (colored CLI), `to_log_lines()` (GUI), `write_to_file()` (full detail log).

### Pkgdeps (independent subsystem)

- **`src/pkgdeps/`** — package-dependency query subsystem unrelated to the install pipeline. Exposed via `deploytix deps {resolve,tree,reverse,graph,plan-install,metadata,compare}` subcommand.
  - `model.rs` — `Package`, `Dep`, `EdgeKind`, `DepClosure`.
  - `source.rs` — `MetadataSource` trait + `MockSource` for tests.
  - `pacman.rs` — production backend that shells out to `pacman -Si`, `pactree`, `expac` through `CommandRunner`.
  - `resolver.rs` — recursive closure with virtual-provider resolution and conflict detection.
  - `graph.rs` — Graphviz DOT output equivalent to `pactree -s -g`.
  - `cli.rs` — subcommand handlers (`cmd_resolve`, `cmd_tree`, `cmd_reverse`, `cmd_graph`, `cmd_plan_install`, `cmd_metadata`, `cmd_compare`).

### Resources (embedded)

- **`src/resources/audio.rs`** — embeds `theme.wav` (61 MB) via `include_bytes!`, plays a looped `rodio::Sink` for the duration of the program. `suppress_alsa_errors()` calls into a C shim built by `build.rs` (`src/resources/alsa_noop.c`) that registers a no-op `snd_lib_error_handler_t` so PCM-underrun spam during heavy disk I/O is silenced. `ensure_audio_env()` restores `XDG_RUNTIME_DIR` when running as root via sudo/pkexec.
- **`src/resources/session_switching/`** — shell scripts and PAM/desktop files embedded into the binary at build time and dropped into the chroot when `install_session_switching = true`.
- **`src/resources/autostart/audio-startup.sh`** — user autostart entry installed unconditionally.

### GUI

- **`src/gui_main.rs`** — entry point. Acquires `O_CREAT | O_EXCL` lock at `/tmp/deploytix-gui.lock` (single-instance enforcement); a `LockGuard` removes the lock file on Drop. Starts theme audio. Launches eframe with `with_fullscreen(true)`.
- **`src/gui/app.rs`** — `DeploytixGui` (eframe `App`). 3-step wizard: `Configure → Summary → Installing`. The Configure step is a 3-column `egui::Ui::columns(3, …)` grid: column 1 = Disk (selection + filesystem + encryption + swap + partition table), column 2 = System (init/locale/timezone/secureboot) + User account, column 3 = Network/Desktop + Gaming/Handheld. Background install/rehearsal threads communicate progress via `mpsc::channel<InstallMessage>` (Status/Progress/Log/Finished/Error/RehearsalResults).
- **`src/gui/state.rs`** — `WizardStep` enum + `DiskState`, `SystemState`, `UserState`, `PackagesState`, `InstallState` sub-structs. `InstallMessage` enum.
- **`src/gui/panels/`** — one file per logical column section (`disk_selection`, `disk_config`, `system_config`, `user_config`, `network_desktop`, `handheld_gaming`, `summary`, `progress`).

### Utils

- **`src/utils/command.rs`** — `CommandRunner { dry_run, recorder: Option<Sender<OperationRecord>> }`. Three execution methods: `run` (interrupt-aware), `run_in_chroot` (uses `artix-chroot` if available, falls back to `chroot`), `force_run` (ignores interrupt flag — used in cleanup). Every command can be recorded as an `OperationRecord { command, stdout, stderr, exit_code, duration, success }`.
- **`src/utils/error.rs`** — `DeploytixError` (thiserror) variants: `NotRoot`, `DeviceNotFound`, `NotBlockDevice`, `DeviceMounted`, `DiskTooSmall { size_mib, required_mib }`, `PartitionError`, `FilesystemError`, `MountError`, `ChrootError`, `CommandFailed { command, stderr }`, `CommandNotFound`, `ConfigError`, `ValidationError`, `UserCancelled`, `Interrupted`, `Io(#[from] std::io::Error)`, `TomlParse`, `TomlSerialize`, `Nix`. `pub type Result<T> = std::result::Result<T, DeploytixError>`.
- **`src/utils/signal.rs`** — `INTERRUPTED: AtomicBool`, `SIGNAL_COUNT: AtomicUsize`, `CAUGHT_SIGNAL: AtomicUsize`. First SIGINT/SIGTERM sets the flag and writes a message via async-signal-safe `write(2)`; second invocation restores `SIG_DFL` and re-raises. `reraise()` re-raises the original signal at clean shutdown so the parent shell sees the right exit code.
- **`src/utils/deps.rs`** — `ensure_dependencies()` checks host binaries (`sfdisk`, `mkfs.*`, `cryptsetup`, `pvcreate`, `grub-install`, `basestrap`, …) against a binary→package map and runs `pacman -S --noconfirm` to install missing ones.
- **`src/utils/prompt.rs`** — `dialoguer` wrappers (`prompt_select`, `prompt_input`, `prompt_password`, `prompt_confirm`, `prompt_multi_select`, `warn_confirm`).

## Component Interactions

### CLI invocation flow

```
main() in src/main.rs
  ├─ Cli::parse() (clap)
  ├─ init_logging(verbose)
  ├─ resources::audio::play_theme_loop() ── handle held until program exit
  └─ dispatch:
      Install         → cmd_install   → DeploymentConfig::{from_file|from_wizard}
                                      → config.validate()
                                      → Installer::new(config, false).run()
      ListDisks       → cmd_list_disks → disk::detection::list_block_devices
      Validate        → cmd_validate
      GenerateConfig  → cmd_generate_config (writes deploytix.toml)
      Cleanup         → cmd_cleanup → cleanup::Cleaner::new(false).cleanup(...)
      Rehearse        → cmd_rehearse → rehearsal::run_rehearsal(&config)
      Deps            → cmd_deps     → pkgdeps::cli::cmd_*
      GenerateDesktopFile → cmd_generate_desktop_file
```

### GUI invocation flow

```
main() in src/gui_main.rs
  ├─ acquire lock /tmp/deploytix-gui.lock (O_CREAT|O_EXCL, mode 0600)
  ├─ install LockGuard (Drop removes lock file)
  ├─ tracing_subscriber::fmt().with_env_filter("info").init()
  ├─ resources::audio::play_theme_loop()
  └─ eframe::run_native("Deploytix", fullscreen, DeploytixGui::new)

DeploytixGui::update (every frame)
  ├─ if disk.refreshing → list_block_devices(false)
  ├─ if install.receiver.is_some() → poll_install_messages + ctx.request_repaint
  ├─ TopBottomPanel header (step indicator)
  ├─ TopBottomPanel footer (Back / Next / Install / Close)
  └─ CentralPanel match step:
      Configure  → panels::configure::show(disk, system, user, packages)
                   → 3-column grid of sub-panels
                   → returns config_valid
      Summary    → panels::summary::show
                   → may set save_requested / rehearsal_requested
      Installing → panels::progress::show
```

### Install pipeline call graph

```
Installer::run()
  ├─ signal::install_signal_handlers()
  ├─ self.prepare()
  │   ├─ utils::deps::ensure_dependencies(...)
  │   ├─ disk::detection::get_device_info(...)
  │   ├─ disk::layouts::compute_layout_from_config(...)
  │   ├─ disk::layouts::print_layout_summary(...)
  │   └─ utils::prompt::warn_confirm(...) [skip if skip_confirm]
  └─ self.run_phases()
      ├─ self.partition_disk()         → disk::partitioning::apply_partitions
      ├─ branch on uses_lvm_thin/uses_multi_luks/zfs:
      │   • LVM thin    → setup_lvm_thin → format_lvm_volumes → mount_lvm_volumes
      │   • Multi-LUKS  → setup_multi_volume_encryption → format_multi_volume_partitions → mount_multi_volume_partitions
      │   • ZFS         → format_partitions → mount_partitions_zfs
      │   • Plain       → format_partitions → mount_partitions
      ├─ self.install_base_system()     → install::basestrap::run_basestrap
      ├─ generate_fstab[_lvm_thin|_multi_volume]
      ├─ if encryption → setup_keyfiles + generate_crypttab(_multi_volume|_lvm_thin)
      ├─ if !partition swap → configure_swap (zram/swap-file)
      ├─ self.configure_system()       — IN CHROOT —
      │   ├─ pacman-key --init / --populate artix
      │   ├─ configure::locale::configure_locale
      │   ├─ configure::users::create_user
      │   ├─ configure::mkinitcpio::configure_mkinitcpio
      │   ├─ configure::bootloader::install_bootloader[_with_layout]
      │   ├─ configure::bootloader::create_grub_reinstall_hook (encrypted only)
      │   ├─ configure::network::configure_network
      │   ├─ configure::greetd::configure_greetd
      │   └─ configure::services::enable_services
      ├─ if encryption → configure::hooks::install_custom_hooks
      ├─ if secureboot → configure::secureboot::setup_secureboot
      ├─ install GPU drivers / desktop / wine / gaming / yay / AUR / btrfs tools / autostart / sysctl / HHD / Decky / evdevhook2
      └─ self.finalize()
          ├─ run_in_chroot "mkinitcpio -P"
          ├─ install::chroot::unmount_all
          ├─ if zfs → disk::formatting::export_zfs_pools
          ├─ close_luks (boot) — close before root volumes
          ├─ close_multi_luks (reverse: home → var → usr → root)
          └─ if LVM-thin LUKS → deactivate_vg + close
```

Emergency cleanup on `Err` or `signal::is_interrupted()`:

```
Installer::emergency_cleanup()
  ├─ /proc/mounts → unmount everything under /install (deepest-first; force_run; lazy fallback)
  ├─ if use_lvm_thin → vgchange -an <vg>
  ├─ Self::kill_orphaned_cryptsetup() (PPID==1 cryptsetup procs, SIGTERM → SIGKILL)
  └─ /dev/mapper/{Crypt-*,temporary-cryptsetup-*} → cryptsetup close (reverse-sorted)
```

### Rehearsal interaction

```
rehearsal::run_rehearsal(&config)
  ├─ mpsc::channel<OperationRecord>
  ├─ DiskWipeGuard::new(device)            ── Drop performs wipe if armed
  ├─ Installer::new(config, false)
  │   .with_skip_confirm(true)
  │   .with_recorder(tx)
  │   .run()
  ├─ rx.iter().collect()                    ── Sender dropped with installer
  ├─ wipe_guard.wipe_now()                  ── unmount → close LUKS → wipefs → blank GPT
  └─ RehearsalReport { records, short_circuited_at, disk_wiped, total_duration }
```

## Deployment Architecture

### Build toolchain

```
Cargo.toml
  package: deploytix v1.3.0, edition 2021, license GPL-3.0-or-later
  [[bin]] deploytix          → src/main.rs           (always built)
  [[bin]] deploytix-gui      → src/gui_main.rs       (requires --features gui)
  [[bin]] deploytix-rehearsal → src-rehearsal/main.rs (always built)
  [features]
    default = []
    gui     = ["dep:eframe", "dep:egui"]
  [build-dependencies] cc = "1"
  [profile.release] opt-level="z", lto=true, codegen-units=1, panic="abort", strip=true
```

`build.rs` compiles `src/resources/alsa_noop.c` into a static library and emits `cargo:rustc-link-arg=$OUT_DIR/libalsa_noop.a` so the no-op ALSA error handler is unconditionally linked.

`.cargo/config.toml` defines two aliases:

- `cargo gcc-build` — glibc dynamic linker
- `cargo portable` — musl static linker (zero runtime deps)

`Makefile` shortcuts: `make build`, `make gui`, `make portable`, `make install` (PREFIX overridable), `make lint`, `make fmt`, `make test`.

### Native dependency loading

- The compiled binary is statically self-contained (in portable mode), but at runtime it shells out to: `sfdisk`, `wipefs`, `partprobe`, `udevadm`, `mkfs.{vfat,ext4,btrfs,xfs,f2fs}`, `mkswap`, `swapon`/`swapoff`, `mount`/`umount`, `blkid`, `cryptsetup`, `pvcreate`/`vgcreate`/`lvcreate`/`vgchange`, `mkinitcpio`, `grub-install`/`grub-mkconfig`, `basestrap`, `artix-chroot`/`chroot`, `pacman`/`pacman-key`, `repo-add`, `makepkg`, `sudo`, `efibootmgr`, `sbctl`/`sbsigntools`, `zpool`/`zfs`. Missing host binaries are detected by `utils::deps::ensure_dependencies()` and installed automatically via `pacman -S --noconfirm`.
- The host system **must** be Artix (not Arch) because `basestrap`, `artix-chroot`, and `artools` are Artix-only.

### Configuration files

| File | Format | Purpose |
|------|--------|---------|
| `deploytix.toml` (cwd or `-c`) | TOML | Full `DeploymentConfig` schema; produced by `deploytix generate-config` or saved from the GUI Summary screen |
| `/etc/pacman.conf` (host, runtime) | INI | May be augmented with `[deploytix]` and `[extra]` sections by `prepare_deploytix_repo` and `ensure_arch_repos`; modified copy written to `/tmp/deploytix-pacman.conf` and passed to `basestrap -C` |
| `com.deploytix.gui.policy` | XML | Polkit action allowing the GUI to gain root |
| `deploytix-gui.desktop` | Desktop Entry | Launcher; per-user copy generated by `deploytix generate-desktop-file` |
| `iso/profile/deploytix/profile.yaml` | YAML | artools/iso build profile for the live ISO |
| `iso/profile/deploytix/live-overlay/usr/share/grub/cfg/{defaults,kernels}.cfg` | GRUB cfg | Live-ISO bootloader configuration |
| `iso/profile/deploytix/root-overlay/etc/mkinitcpio.conf.d/cow-persistence.conf` | mkinitcpio | COW persistence for the live ISO |
| `iso/gamescope-pkg/PKGBUILD` | Bash | Custom gamescope build for the [deploytix] repo |
| `pkg/PKGBUILD` | Bash | `deploytix-git` and `deploytix-gui-git` Arch packages |

The TOML schema is exhaustively defined by serde-derive on `DeploymentConfig` and its sub-structs in `src/config/deployment.rs`; every field has a `#[serde(default = "…")]` so a minimal config is accepted.

### Lock files and state

- `/tmp/deploytix-gui.lock` — single-instance lock for the GUI binary (held by an open file descriptor; removed by `LockGuard::drop`).
- `/tmp/deploytix/partition_script` — sfdisk script (deleted after successful partitioning).
- `/tmp/deploytix-pacman.conf` — generated pacman.conf (kept for the duration of the install).
- `/tmp/deploytix-local-repo/` — local pacman repo built from located/built `.pkg.tar.zst` files.
- `/tmp/deploytix_btrfs_*` — temporary mountpoints used during subvolume creation.
- `/tmp/deploytix_rehearsal_wipe` — wipe script used by `DiskWipeGuard`.
- `/install` — the chroot mountpoint (constant `INSTALL_ROOT` in `installer.rs`).

### Update / distribution

- Two PKGBUILDs in `pkg/PKGBUILD` produce `deploytix-git` and `deploytix-gui-git`.
- Releases are pre-built `.pkg.tar.zst` archives under `releases/v<ver>-r<n>-g<sha>/` (currently `v1.2.6-r9-ge34a93c`).
- `iso/build-deploytix-iso.sh` builds a complete live ISO via artools; `iso/write-deploytix-usb.sh` writes the resulting ISO to a USB stick.
- During installation, `deploytix-git`, `deploytix-gui-git`, and `tkg-gui-git` are part of the basestrap package list — the installer reinstalls itself onto the target so the system can be redeployed from itself.

### Platform behaviour

- Linux only. `build.rs` only runs the `alsa_noop.c` step on Linux (`#[cfg(target_os = "linux")]`); other platforms get a stub that does nothing.
- BIOS support exists via the `LegacyBIOSBootable` GPT attribute on the BOOT partition and a BIOS Boot partition GUID, but GRUB is installed in EFI mode (`x86_64-efi`).
- ARM is not supported.

## Runtime Behaviour

### CLI startup sequence

| # | Code location | Action |
|---|---------------|--------|
| 1 | `src/main.rs:226` | `Cli::parse()` |
| 2 | `src/main.rs:227` | `init_logging(verbose)` — sets RUST_LOG via env-filter |
| 3 | `src/main.rs:230` | `resources::audio::play_theme_loop()` (looped theme.wav) |
| 4 | `src/main.rs:232–262` | dispatch on subcommand (default = interactive Install wizard) |
| 5a | `src/main.rs:269` | check `geteuid().is_root()` for Install/Cleanup/Rehearse |
| 5b | `src/main.rs:275–281` | load TOML or run `from_wizard(device)` |
| 5c | `src/main.rs:284` | `config.validate()` |
| 5d | `src/main.rs:287–288` | `Installer::new(config, false).run()` |

### GUI startup sequence

| # | Code location | Action |
|---|---------------|--------|
| 1 | `src/gui_main.rs:16–32` | acquire `O_CREAT \| O_EXCL` lock at `/tmp/deploytix-gui.lock` |
| 2 | `src/gui_main.rs:35–41` | install `LockGuard` (Drop removes lock) |
| 3 | `src/gui_main.rs:44–47` | `tracing_subscriber::fmt`, env-filter "info" |
| 4 | `src/gui_main.rs:50` | `play_theme_loop()` |
| 5 | `src/gui_main.rs:52–58` | `eframe::NativeOptions { fullscreen: true }` |
| 6 | `src/gui_main.rs:60–64` | `eframe::run_native("Deploytix", options, DeploytixGui::new)` |

`DeploytixGui::new` calls `theme::apply(&cc.egui_ctx)` which sets the dark colour palette and font sizes used by every widget.

### Installer phase ordering

The `run_phases` method (`src/install/installer.rs:157`) runs strictly in order. Conditional phases no-op when their feature flag is off:

| Phase | Method | Feature flag |
|-------|--------|--------------|
| 0   | `prepare()` | always (deps + layout + confirm) |
| 1   | `partition_disk()` | always (skipped only if `preserve_home`) |
| 2.1 | `setup_lvm_thin → format_lvm_volumes → mount_lvm_volumes` | `use_lvm_thin` |
| 2.2 | `setup_multi_volume_encryption → format_multi_volume_partitions → mount_multi_volume_partitions` | `encryption && !use_lvm_thin` |
| 2.3 | `format_partitions → mount_partitions_zfs` | `filesystem == Zfs && !encryption` |
| 2.4 | `format_partitions → mount_partitions` | (else) |
| 3   | `install_base_system` | always |
| 3.5 | `generate_fstab[_lvm_thin|_multi_volume]` (+ `append_swap_file_entry`) | always |
| 3.6 | `setup_keyfiles + generate_crypttab[_lvm_thin|_multi_volume]` | encryption |
| 3.7 | `configure_swap` | `swap_type != Partition` |
| 4   | `configure_system` | always |
| 4.5 | `install_custom_hooks` | encryption |
| 4.6 | `setup_secureboot` | `secureboot` |
| 4.7 | `install_gpu_drivers` | `gpu_drivers.len() > 0` |
| 5.0 | `install_desktop` | always (no-op if `None`) |
| 5.1 | `install_wine_packages` | `install_wine` |
| 5.2 | `install_gaming_packages` | `install_gaming` |
| 5.25| `install_session_switching` | `install_session_switching` |
| 5.3 | `install_yay` | `install_yay` |
| 5.35| `install_aur_packages` | `install_yay` |
| 5.4 | `install_btrfs_tools` | `install_btrfs_tools` |
| 5.5 | `install_autostart_entries` | always |
| 5.6 | `install_sysctl_gaming` | `sysctl_gaming_tweaks` |
| 5.65| `install_sysctl_network_performance` | `sysctl_network_performance` |
| 5.7 | `install_hhd + enable_service("hhd")` | `install_hhd` |
| 5.8 | `install_decky_loader + enable_service("plugin_loader")` | `install_decky_loader` |
| 5.85| `install_evdevhook2 + enable_service("evdevhook2")` | `install_evdevhook2` |
| 6   | `finalize()` | always (`mkinitcpio -P` → unmount → close LUKS) |

### Background tasks

- **Theme audio**: A `rodio::Sink` runs on a rodio-managed thread for the entire program lifetime; `AudioHandle` owns the `OutputStream` and `Sink`, both of which stop when dropped at program exit.
- **GUI install thread**: `start_installation` and `start_rehearsal` spawn a `std::thread`. The thread sends `InstallMessage` values through an `mpsc::channel`; the GUI polls in `update()` whenever `install.receiver.is_some()` and calls `ctx.request_repaint()` to drive the next frame.
- **No async runtime**: the project does not depend on tokio/async-std; everything is synchronous + threads.

### Error handling strategy

- Top level (`main()`): `anyhow::Result<()>`; propagation prints the chain and exits non-zero.
- Module operations: `crate::utils::error::Result<T>` (= `Result<T, DeploytixError>`).
- `Installer::run` catches every `Err` and any `signal::is_interrupted()` flag, runs `emergency_cleanup`, calls `signal::reraise()` if interrupted (so the parent shell sees `128 + signo`), and finally returns the original error.
- `DiskWipeGuard::drop` runs a best-effort cleanup-+-wipe with all errors logged at `warn!` and never propagated — panicking inside Drop is undefined behaviour.

### Memory / resource lifecycle

- LUKS containers are tracked in `Installer.luks_containers`, `luks_boot_container`, and `luks_lvm_container`. They are closed in reverse order during `finalize()` and via `emergency_cleanup` on failure.
- LVM VG is deactivated via `vgchange -an <vg>` in both `finalize()` (when LVM thin + LUKS) and `emergency_cleanup`.
- Mount points under `/install` are unmounted deepest-first via `/proc/mounts` parsing.
- ZFS pools are exported via `disk::formatting::export_zfs_pools` in `finalize` if either data or boot filesystem is ZFS.
- Audio: `AudioHandle` lives on the stack in `main()`; dropped only at program exit.
- The GUI lock file is owned by `LockGuard`; Drop removes the file (also runs on panic via stack unwind, though the binary uses `panic = "abort"` in release).

## Installation Pipeline

The installation is the dominant data-transformation pipeline. Each stage transforms the disk and/or the chroot at `/install`.

| # | Stage | Input | Output | Key files |
|---|-------|-------|--------|-----------|
| 1 | Layout computation | `DiskConfig`, `disk_mib` | `ComputedLayout { partitions, subvolumes?, planned_thin_volumes? }` | `src/disk/layouts.rs:383 compute_layout_from_config` |
| 2 | Partitioning | `ComputedLayout`, device | GPT partition table on device | `src/disk/partitioning.rs:36 generate_sfdisk_script`, `:112 apply_partitions` |
| 3 | LVM thin (optional) | LVM PV partition | VG + thin pool + thin LVs | `src/disk/lvm.rs` |
| 4 | LUKS layer (optional) | data partitions or LVM PV | `/dev/mapper/Crypt-{Root,Usr,Var,Home,Boot,Lvm}` | `src/configure/encryption.rs:511 setup_multi_volume_encryption`, `:269 setup_boot_encryption`, `:419 setup_single_luks` |
| 5 | Filesystem creation | mapped or raw partitions | mkfs'd filesystems (or ZFS pool/datasets) | `src/disk/formatting.rs format_all_partitions`, `format_efi`, `format_boot_partition`, `create_zfs_pool`, `create_zfs_datasets` |
| 6 | Btrfs subvolumes (optional) | btrfs filesystem | `@`, `@home`, `@usr`, `@var`, `@log` (or per-partition `@<name>`) | `src/disk/formatting.rs create_btrfs_subvolumes`, `mount_btrfs_subvolumes`, `src/install/chroot.rs mount_boot_btrfs_subvolume` |
| 7 | Mounting | layout + filesystems | populated `/install` tree | `src/install/chroot.rs:16 mount_partitions`, `:127 mount_partitions_zfs`, `:211 mount_partitions_with_subvolumes` |
| 8 | Custom repo prep | basestrap package list | `/tmp/deploytix-pacman.conf` (+ optional `/tmp/deploytix-local-repo/`) | `src/install/basestrap.rs:784 prepare_deploytix_repo`, `:891 ensure_arch_repos` |
| 9 | Basestrap | packages list, install_root | populated chroot at `/install` | `src/install/basestrap.rs:967 run_basestrap_with_retries` |
| 10 | fstab | layout, UUIDs | `/install/etc/fstab` | `src/install/fstab.rs:86 generate_fstab`, `:350 generate_fstab_multi_volume`, `:510 generate_fstab_lvm_thin` |
| 11 | Keyfiles + crypttab | LUKS containers, password | `/install/etc/cryptsetup-keys.d/crypt*.key` + `/etc/crypttab` | `src/configure/keyfiles.rs:118 setup_keyfiles_for_volumes`, `src/install/crypttab.rs:118 generate_crypttab_multi_volume` |
| 12 | Swap config | `swap_type`, init | per-init zram service or swap file | `src/configure/swap.rs setup_zram`, swap-file allocator |
| 13 | Pacman keyring | chroot | initialised + populated keyring | `installer.rs:776` (`pacman-key --init`, `pacman -Sy artix-keyring`, `pacman-key --populate artix`) |
| 14 | Locale + user + mkinitcpio | chroot, config | `/etc/{locale.conf,locale.gen,vconsole.conf,passwd,sudoers.d/wheel,mkinitcpio.conf}` | `src/configure/{locale,users,mkinitcpio}.rs` |
| 15 | Bootloader | chroot, layout, encryption flags | `/boot/efi/EFI/Artix/grubx64.efi`, `/boot/grub/grub.cfg`, `/etc/default/grub` | `src/configure/bootloader.rs install_bootloader[_with_layout]`, `create_grub_reinstall_hook` |
| 16 | Network | chroot | iwd or NetworkManager configs + service files | `src/configure/network.rs configure_network` |
| 17 | greetd + services | chroot | `/etc/greetd/config.toml` + per-init service enables | `src/configure/{greetd,services}.rs` |
| 18 | Custom initcpio hooks | chroot, encryption flags | `/usr/lib/initcpio/{hooks,install}/{crypttab-unlock,mountcrypt}` | `src/configure/hooks.rs install_custom_hooks` |
| 19 | SecureBoot (optional) | chroot, secureboot_method | enrolled keys + signed shim/grub | `src/configure/secureboot.rs setup_secureboot` |
| 20 | Optional packages | chroot, packages config | GPU drivers / DE / Wine / gaming / yay / AUR / btrfs-tools / sysctl / HHD / Decky / evdevhook2 | `src/configure/packages.rs` (~1500 LOC) |
| 21 | Initramfs regeneration | chroot | rebuilt `/boot/initramfs-*.img` | `installer.rs:967 (run_in_chroot "mkinitcpio -P")` |
| 22 | Cleanup | chroot, LUKS handles | unmounted FS, closed LUKS, exported ZFS | `src/install/chroot.rs unmount_all`, `src/configure/encryption.rs close_multi_luks`, `src/disk/lvm.rs deactivate_vg`, `src/disk/formatting.rs export_zfs_pools` |

### Hook selection (mkinitcpio)

| Encryption mode | Hooks (after `keyboard keymap consolefont`) |
|-----------------|---------------------------------------------|
| None, plain FS | `[btrfs|zfs?] filesystems [usr?] [resume?]` |
| Multi-LUKS (encryption && !lvm_thin) | `lvm2 crypttab-unlock mountcrypt` (NO `filesystems` — mountcrypt handles all) |
| LVM thin + plain | `lvm2 [btrfs|zfs?] filesystems usr [resume?]` |
| LVM thin + encryption | `lvm2 encrypt [btrfs|zfs?] filesystems usr [resume?]` |
| LVM thin + encryption + boot_encryption | `lvm2 encrypt crypttab-unlock [btrfs|zfs?] filesystems usr [resume?]` |

### Layout-by-flag matrix

| `encryption` | `use_lvm_thin` | `boot_encryption` | filesystem | Outcome |
|---|---|---|---|---|
| F | F | F | btrfs | Plain btrfs with subvolumes (`@/@home/@usr/@var/@log`) |
| F | F | F | ext4/xfs/f2fs | Plain partitions, separate `/usr` `/var` `/home` |
| F | F | F | zfs | ZFS rpool + bpool + datasets |
| T | F | F | any non-zfs | Multi-LUKS: separate `Crypt-Root/Usr/Var/Home` + plain `/boot` |
| T | F | T | any non-zfs | Multi-LUKS + LUKS1 `Crypt-Boot` (GRUB-readable) |
| F | T | F | any non-zfs | LVM thin: single LVM PV + thin pool + thin LVs (`root`, `usr`, `var`, `home`) |
| T | T | F | any non-zfs | LUKS-on-LVM: `Crypt-LVM` → LVM PV → thin LVs |
| T | T | T | any non-zfs | LUKS-on-LVM + LUKS1 `Crypt-Boot` |

### Embedded resources

| Resource | Path | Embedded via |
|----------|------|--------------|
| Theme audio | `theme.wav` (61 MB) | `include_bytes!("../../theme.wav")` in `src/resources/audio.rs:8` |
| ALSA error suppression | `src/resources/alsa_noop.c` | Compiled as static lib by `build.rs`; linked via `cargo:rustc-link-arg` |
| Audio autostart | `src/resources/autostart/audio-startup.sh` | Read from disk during build via `include_str!` (in `configure::packages`) and dropped into chroot |
| Session-switching scripts | `src/resources/session_switching/*` | Same pattern; written into chroot when `install_session_switching = true` |

### Notable stub / incomplete behaviour

- **`DeploymentConfig::validate()` cannot be unit-tested in isolation** — the test source explicitly notes this (`src/config/deployment.rs:1414–1423`). It checks block-device existence first, before any pure rule. The fix proposed in the source: extract pure rules into a separate `validate_config_rules()`.
- **`compute_layout_from_config` has dead code suppression** (`let _ = default_opts.as_str();` at `src/disk/layouts.rs:446`) — remnant of incomplete refactor.
- **`mount_partitions_zfs`** uses hardcoded dataset names from `ZFS_DATASETS` constant; user-supplied dataset layouts are not yet supported.
- **CLAUDE.md is stale.** It describes a 7-step GUI wizard, but the actual GUI is 3 steps with a 3-column unified configure panel. It does not mention the `pkgdeps`, `rehearsal`, or `resources` modules, the `deploytix-rehearsal` binary, or the `Rehearse` / `Deps` / `GenerateDesktopFile` subcommands. CLAUDE.md says global flags include `-n/--dry-run` but the CLI struct in `src/main.rs:121–132` only has `-v/--verbose` (commit `2b63d6e` is titled "rehearsal installations, prior to PreFlight/Dry-Run removal" — dry-run was pruned from the CLI surface, but the `CommandRunner.dry_run` plumbing remains in place so it can be re-exposed if needed).
- **Retry logic is network-only.** `run_basestrap_with_retries` only retries on patterns matching common pacman network errors; non-network errors fail immediately on the first attempt.
- **Single bootloader.** The `Bootloader` enum has only `Grub`; despite the `match config.system.bootloader` guard structure that anticipates other bootloaders, no other variant exists.
