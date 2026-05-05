# Deploytix — Validation Workflow

This document defines the test procedures used to validate a Deploytix change. Tests are ordered to match the installer's pipeline; a failure in an early test invalidates results from later tests.

## Test Environment Setup

### Required hardware / VMs

- An Artix Linux host (cannot be Arch — `basestrap`, `artix-chroot`, `artools` are Artix-only).
- A target block device that is **not** the host root disk. Acceptable: a USB stick (USB 3 strongly recommended for basestrap throughput), a spare SSD, or a virtual disk attached to a libvirt/qemu guest. Minimum size: 32 GiB. Recommended: 128 GiB.
- For SecureBoot tests: a UEFI machine (not a VM unless OVMF is configured) with SecureBoot enabled in firmware setup mode.

### Required reference files

| File | Purpose |
|------|---------|
| `deploytix.toml` (project root) | Canonical reference config used by the rehearsal CI path; the `device =` field must be edited per session |
| `iso/build-deploytix-iso.sh` output | Live ISO containing the embedded `[deploytix]` repo (recommended for clean-host validation) |
| `pkg/PKGBUILD` | Source of `deploytix-git` and `deploytix-gui-git`; verify it builds before running install tests |

### Config baseline

Before each session, reset `deploytix.toml` to a known minimum that exercises the install path (replace `device =` with the test target):

```toml
[disk]
device = "/dev/sdX"          # CHANGE per session
filesystem = "btrfs"
encryption = false
swap_type = "zramonly"
[[disk.partitions]]
mount_point = "/"
size_mib = 0                 # remainder
[system]
init = "runit"
hostname = "test-artix"
[user]
name = "tester"
password = "tester"
[network]
backend = "iwd"
[desktop]
environment = "none"
```

The matrix tests below override individual fields from this baseline.

### How to observe failures

| Channel | Where | What to look for |
|---------|-------|------------------|
| Stdout/stderr | Terminal during `deploytix install` | `[INFO]` / `[WARN]` / `[ERROR]` lines from `tracing` |
| Verbose log | `RUST_LOG=debug deploytix install -v …` | Per-command output via `CommandRunner::run` |
| Rehearsal report | `rehearsal.log` (CLI) or `print_table()` output | Pass/fail of every recorded `OperationRecord` |
| Kernel ring buffer | `dmesg -T` | LUKS / btrfs / GRUB messages from kernel modules |
| `/proc/mounts` | live during install | Whether mounts under `/install` succeeded |
| `/dev/mapper/` | live during install | Active LUKS containers (Crypt-Root, Crypt-Boot, etc.) |
| `journalctl -k` (post-boot on installed system) | After first reboot | Boot-time errors (initramfs hooks, mkinitcpio, GRUB) |

## Pre-Test Checklist

Each gate must pass before proceeding. Failures **block** all later tests.

```bash
# G1. Running as root
[ "$(id -u)" -eq 0 ] || echo "FAIL G1: must run as root"

# G2. Host is Artix, not Arch
grep -q '^ID=artix' /etc/os-release || echo "FAIL G2: host is not Artix"

# G3. Required Artix tooling present
for bin in basestrap artix-chroot pacman-key sfdisk wipefs cryptsetup pvcreate \
           vgcreate lvcreate mkinitcpio grub-install grub-mkconfig blkid \
           mkfs.vfat mkfs.btrfs mkfs.ext4; do
  command -v "$bin" >/dev/null 2>&1 || echo "MISSING G3 binary: $bin"
done

# G4. Target device is a block device, not currently mounted, not the root disk
TARGET=/dev/sdX                       # SET FIRST
ROOT_PARENT=$(lsblk -no PKNAME "$(findmnt -no SOURCE /)")
[ -b "$TARGET" ]                                       || echo "FAIL G4: $TARGET not a block device"
[ "$(basename "$TARGET")" != "$ROOT_PARENT" ]          || echo "FAIL G4: $TARGET is the host root disk"
! findmnt -nro SOURCE | grep -q "^$TARGET"             || echo "FAIL G4: $TARGET has mounted partitions"

# G5. /install does not have stale mounts from a previous run
! grep -q ' /install ' /proc/mounts                    || echo "FAIL G5: /install has stale mounts (run: deploytix cleanup)"

# G6. No stray Crypt-* mappings left from a previous run
[ -z "$(ls /dev/mapper/Crypt-* /dev/mapper/temporary-cryptsetup-* 2>/dev/null)" ] \
  || echo "FAIL G6: stale dm mappings (run: deploytix cleanup)"

# G7. Custom [deploytix] packages are reachable (one of these MUST be true)
PKG_OK=0
[ -d /var/lib/deploytix-repo ] && grep -q '^\[deploytix\]' /etc/pacman.conf && PKG_OK=1
ls pkg/deploytix-git-*.pkg.tar.zst >/dev/null 2>&1 && PKG_OK=1
[ -f pkg/PKGBUILD ] && PKG_OK=1
[ "$PKG_OK" -eq 1 ] || echo "FAIL G7: no path to custom [deploytix] packages — run iso/build-deploytix-iso.sh first"

# G8. deploytix binary is built
[ -x target/release/deploytix ] || echo "FAIL G8: cargo build --release first"

# G9. Disk has at least 32 GiB
DISK_BYTES=$(blockdev --getsize64 "$TARGET")
[ "$DISK_BYTES" -ge "$((32 * 1024**3))" ] || echo "FAIL G9: $TARGET is smaller than 32 GiB"
```

## Test Procedures

Tests are organised by pipeline stage in execution order. Stop and investigate at the first failure — later tests assume earlier ones passed.

### T0 — Configuration validation (no disk needed)

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T0a** | `DeploymentConfig::validate()` cross-field rules | `target/release/deploytix validate deploytix.toml` (with valid `device`) | Exit 0 + `✓ Configuration is valid` | Inspect printed `ValidationError` text; cross-reference with rules in `src/config/deployment.rs:1037-1294` |
| **T0b** | TOML parse | edit a field to invalid value (e.g. `swap_type = "garbage"`); rerun `deploytix validate` | Exit ≠ 0 with `TomlParse` error | If accepts garbage, `serde(rename_all = "lowercase")` may be missing on enum |
| **T0c** | Sample generation round-trip | `target/release/deploytix generate-config -o /tmp/sample.toml && target/release/deploytix validate /tmp/sample.toml` | First exits 0; second fails on missing block device only — all schema rules pass | If schema rule fails, `DeploymentConfig::sample()` is out of sync with `validate()` |

### T1 — Layout computation (no disk write)

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T1a** | Pure layout math | `cargo test --all-features layouts::tests` | All `floor_align`, `clamp`, `calculate_swap_mib`, `get_luks_partitions`, `standard_subvolumes` tests pass | Inspect `src/disk/layouts.rs:552-682` |
| **T1b** | Encryption flag layering | Set `encryption = true`, `use_lvm_thin = false` in baseline; run `deploytix install -c deploytix.toml -v` and observe the printed layout summary | `is_luks=true` for ROOT/USR/VAR/HOME, `false` for EFI/BOOT/SWAP | Inspect `apply_encryption_flags` in `src/disk/layouts.rs:470-476` |
| **T1c** | LVM-thin collapse | Set `use_lvm_thin = true`; observe layout summary | Single LVM partition replaces data partitions; `planned_thin_volumes` populated with `root`, `usr`, `var`, `home` | Inspect `apply_lvm_thin_to_layout` in `src/disk/layouts.rs:483-550` |
| **T1d** | Btrfs subvolume mount-clearing | Set `filesystem = "btrfs"` with default 4-partition layout; observe summary | ROOT partition prints `MOUNT = -` (cleared because mounted via `subvol=@`); other btrfs data partitions retain their mount points but get a `subvolume_name` set | Inspect `src/disk/layouts.rs:454-458` |

### T2 — Partitioning

Run with the baseline config and `device` pointing at the test disk.

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T2a** | sfdisk script generation | `RUST_LOG=debug deploytix install -c deploytix.toml -v 2>&1 \| tee install.log` and abort at the confirmation prompt; inspect the dumped layout | `Partition layout (total: …)` table prints; sizes align to 4 MiB; remainder partition has `size = remainder` | Inspect `generate_sfdisk_script` in `src/disk/partitioning.rs:36-109` |
| **T2b** | Logical block size detection | On a 4096-byte-sector NVMe, observe `sector-size: 4096` in the dumped sfdisk script | `sector-size: 4096` printed; partition starts/sizes scaled correctly | Inspect `logical_sector_size` in `src/disk/partitioning.rs:23-33` |
| **T2c** | Real partitioning | Confirm the install at the prompt; after partition step, abort with Ctrl-C; run `sfdisk -l $TARGET` | GPT label present; expected number of partitions; LegacyBIOSBootable attribute on BOOT (partition 2) | Inspect `apply_partitions` in `src/disk/partitioning.rs:112-177` |
| **T2d** | preserve_home verification | Set `preserve_home = true` on a disk with no existing partition table; run `deploytix install` | Fails fast with `preserve_home: expected partition X (Y) does not exist on …` | Inspect `verify_existing_partitions` in `src/install/installer.rs:605-664` |

### T3 — Encryption layer (only if `encryption = true`)

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T3a** | Multi-LUKS setup | Set `encryption = true`, `use_lvm_thin = false`; run install through the encryption stage | `/dev/mapper/Crypt-Root`, `Crypt-Usr`, `Crypt-Var`, `Crypt-Home` all present; `cryptsetup status Crypt-Root` shows `cipher: aes-xts-plain64`, `keysize: 512 bits`, type `LUKS2` | Inspect `setup_multi_volume_encryption` in `src/configure/encryption.rs:511-594` |
| **T3b** | Mapper name disambiguation | Manually pre-create `/dev/mapper/Crypt-Root` with a dummy LUKS container, then run install | Installer logs `Mapper name 'Crypt-Root' already in use, disambiguating` and uses `Crypt-Root-1` | Inspect `resolve_mapper_name` in `src/configure/encryption.rs:42-56` |
| **T3c** | LUKS1 boot encryption | Set `boot_encryption = true`; observe install | `/dev/mapper/Crypt-Boot` present; `cryptsetup status Crypt-Boot` shows `type: LUKS1`, `hash: sha512`, no `pbkdf` line (LUKS1 uses pbkdf2 implicitly) | Inspect `setup_boot_encryption` and `luks_format_v1` in `src/configure/encryption.rs:269-380` |
| **T3d** | dm-integrity | Set `integrity = true, encryption = true`; observe install | `cryptsetup status Crypt-Root` shows `integrity: hmac(sha256)`, `sector size: 4096 bytes` | Inspect `luks_format_integrity` in `src/configure/encryption.rs:140-211`; LUKS2 only |
| **T3e** | Password-via-stdin | grep the install log for any literal occurrence of the encryption password | Password never appears on any command line in stdout/stderr/tracing | Inspect stdin pipe usage in `src/configure/encryption.rs:185-199` |

### T4 — LVM thin (only if `use_lvm_thin = true`)

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T4a** | VG + thin pool | After phase 2.1, run `vgs && lvs` from the host | VG `vg0`, thin pool `thinpool` at 95% of VG, thin volumes `root`/`usr`/`var`/`home` exist | Inspect `setup_lvm_thin` and `src/disk/lvm.rs` |
| **T4b** | LVM-on-LUKS layering | `use_lvm_thin = true, encryption = true`; observe install | `/dev/mapper/Crypt-LVM` present; LVM PV is on top of the mapper, not the raw partition | Inspect single-LUKS path in `src/configure/encryption.rs:419-505` |
| **T4c** | LVM thin + boot encryption | All three flags true; verify | Both `Crypt-LVM` (LUKS2) and `Crypt-Boot` (LUKS1) present; crypttab has both entries | Inspect `generate_crypttab_lvm_thin` |

### T5 — Filesystem creation & mounting

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T5a** | btrfs subvolumes (default layout) | Default config (btrfs); after mount stage `btrfs subvolume list /install` | Subvolume `@` listed; for multi-partition layouts also `@home`, `@usr`, `@var`, `@log` on their respective btrfs filesystems | Inspect `create_btrfs_subvolumes` in `src/disk/formatting.rs` and `mount_partitions_with_subvolumes` in `src/install/chroot.rs:211-316` |
| **T5b** | btrfs `/boot` subvolume | `filesystem = "btrfs"` (boot derived to btrfs); after mount stage `btrfs subvolume list /install/boot` | Subvolume `@boot` listed; `findmnt /install/boot \| grep subvol=@boot` matches | Inspect `mount_boot_btrfs_subvolume` in `src/install/chroot.rs:325-343` |
| **T5c** | ZFS pool + datasets | `filesystem = "zfs"`; after mount stage `zpool list && zfs list` | Pools `rpool` and `bpool` present; datasets at expected mount points (legacy mountpoints) | Inspect `create_zfs_pool`, `create_zfs_datasets`, `mount_zfs_datasets` in `src/disk/formatting.rs` |
| **T5d** | preserve_home behaviour | First install with `/home` partition; second install with `preserve_home = true`, change hostname | Second install does not reformat HOME; existing files survive; `findmnt /install/home` succeeds | Inspect `mount_partitions_preserve` in `src/install/chroot.rs:27-43` |
| **T5e** | mount ordering | After mount stage, `findmnt -R /install \| awk '{print $1}'` | Mount points sorted by depth (shallowest first); `/install/boot` mounted before `/install/boot/efi` | Inspect mount-order sort in `src/install/chroot.rs:79-82` |

### T6 — Basestrap

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T6a** | Custom repo discovery | On a clean Artix host with no `[deploytix]` section in pacman.conf, no built packages, no live-ISO repo, run install | Fails with `Cannot resolve custom packages: …` and the multi-line "To fix" message | Inspect `prepare_deploytix_repo` in `src/install/basestrap.rs:784-875` |
| **T6b** | Pre-built `.pkg.tar.zst` discovery | Place pre-built archives in `pkg/`; run install | Logs `Found N pre-built package file(s); creating temporary repo`; `/tmp/deploytix-local-repo/` populated | Inspect `locate_prebuilt_packages` in `src/install/basestrap.rs:409-497` |
| **T6c** | Build-from-source fallback | Remove pre-built packages, keep `pkg/PKGBUILD`; run install as `sudo` (so `SUDO_USER` is set) | Logs `Building deploytix-git from …`; new `.pkg.tar.zst` files appear in `pkg/` | Inspect `build_package_from_source` in `src/install/basestrap.rs:591-669` |
| **T6d** | Network retry | Block outbound HTTP via firewall during basestrap; observe | Three attempts, 5 s delay between; final error message includes `failed to retrieve some files` or similar | Inspect `run_basestrap_with_retries` in `src/install/basestrap.rs:967-1038` |
| **T6e** | Arch [extra] injection | On a host without `[extra]` configured, run install with package(s) from [extra] | Log line `Arch [extra] repository not configured; adding it`; `/tmp/deploytix-pacman.conf` contains `[extra]` block | Inspect `ensure_arch_repos` in `src/install/basestrap.rs:891-930` |

### T7 — fstab / crypttab

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T7a** | fstab pass numbers | After phase 3.5, `cat /install/etc/fstab` | ext4 `/` → pass 1; other ext4 → pass 2; btrfs/xfs/f2fs/zfs → pass 0 | Unit-tested in `src/install/fstab.rs:651-694`; if mismatch, inspect `fsck_pass` |
| **T7b** | btrfs fstab subvol options | btrfs config; observe fstab | `subvol=@,defaults,noatime,compress=zstd` for `/`; `subvol=@boot,…` for `/boot` | Inspect `generate_fstab_with_subvolumes` in `src/install/fstab.rs:200-332` |
| **T7c** | Multi-LUKS fstab | encryption + non-zfs config | UUID for fstab is filesystem UUID of the **mapped device** (`/dev/mapper/Crypt-Root` etc.), not the underlying partition | Inspect `generate_fstab_multi_volume` in `src/install/fstab.rs:350-489` |
| **T7d** | crypttab options | `cat /install/etc/crypttab` after install | Multi-LUKS data: `luks,discard` (no integrity) or `luks` (with integrity); LUKS1 boot: always `luks,discard` | Inspect `crypttab_options` in `src/install/crypttab.rs:14-20` and unit tests at `:213-222` |
| **T7e** | Keyfile permissions | `find /install/etc/cryptsetup-keys.d -type f -printf '%m %p\n'` | All files mode `000`; directory mode `700` | Inspect `setup_keyfiles_for_volumes` in `src/configure/keyfiles.rs:118-161` |

### T8 — Swap configuration

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T8a** | ZRAM service (runit) | `swap_type = "zramonly", init = "runit"`; after install, `ls /install/etc/runit/sv/zram` | `run` and `finish` scripts present; mode 755; `runsvdir/default/zram` symlink present | Inspect `setup_zram_runit` in `src/configure/swap.rs:54-108` |
| **T8b** | ZRAM service (other inits) | Repeat T8a for openrc / s6 / dinit | init-specific service file present at the right path | Inspect parallel `setup_zram_*` functions in `src/configure/swap.rs` |
| **T8c** | Swap file fstab entry | `swap_type = "filezram", filesystem = "btrfs"`; observe fstab | Entry for `/swap/swapfile`; ZRAM service also installed | Inspect `append_swap_file_entry` and `swap_file_fstab_entry` |
| **T8d** | Swap file rejection on xfs | `swap_type = "filezram", filesystem = "xfs"`; run `deploytix validate` | Validation fails with `Swap file requires btrfs or ext4 filesystem` | Inspect `src/config/deployment.rs:1152-1159` |

### T9 — In-chroot configuration

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T9a** | Pacman keyring init | After `configure_system`, `artix-chroot /install pacman-key --list-keys \| head -5` | Keys present; output non-empty | Sequence in `src/install/installer.rs:775-787` |
| **T9b** | Locale configuration | `cat /install/etc/locale.conf /install/etc/vconsole.conf` | Match `system.locale` and `system.keymap` | Inspect `src/configure/locale.rs` |
| **T9c** | dinit keymap service | `init = "dinit"`; after install, `ls /install/etc/dinit.d/keymap` | Service file present | Inspect `create_dinit_keymap_service` in `src/configure/locale.rs` |
| **T9d** | User creation | `grep "^tester:" /install/etc/passwd` | User present; `/etc/sudoers.d/wheel` enables sudo for wheel group | Inspect `src/configure/users.rs` |

### T10 — mkinitcpio + custom hooks

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T10a** | HOOKS construction (no encryption, btrfs) | `grep ^HOOKS= /install/etc/mkinitcpio.conf` | Contains `keyboard keymap consolefont … btrfs filesystems` (no `lvm2`, no `encrypt`, no custom hooks) | Inspect `construct_hooks` in `src/configure/mkinitcpio.rs:72-170` |
| **T10b** | HOOKS for multi-LUKS | encryption + non-LVM-thin; observe HOOKS | Contains `lvm2 crypttab-unlock mountcrypt` and **does NOT** contain `filesystems` | Inspect lines `:97-101` of `src/configure/mkinitcpio.rs` |
| **T10c** | HOOKS for LVM-thin + LUKS | both flags true; observe HOOKS | Contains `lvm2 encrypt … filesystems usr`; with `boot_encryption` also `crypttab-unlock` | Inspect `:103-135` of `src/configure/mkinitcpio.rs` |
| **T10d** | Custom hook installation | encryption true; check `ls /install/usr/lib/initcpio/{hooks,install}/{crypttab-unlock,mountcrypt}` | All four files present, mode 755 | Inspect `install_custom_hooks` in `src/configure/hooks.rs:19-64` |
| **T10e** | FILES array includes keyfiles | encryption true; `grep ^FILES= /install/etc/mkinitcpio.conf` | Contains `/etc/crypttab`, `/etc/cryptsetup-keys.d/cryptroot.key`, …`cryptusr.key`, …`cryptvar.key`, …`crypthome.key` (and `cryptboot.key` if `boot_encryption`) | Inspect `construct_files` in `src/configure/mkinitcpio.rs:181-200` |

### T11 — Bootloader

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T11a** | GRUB EFI install (plain) | After install, `ls /install/boot/efi/EFI/Artix/grubx64.efi` | File exists | Inspect `install_grub` in `src/configure/bootloader.rs:71-127` |
| **T11b** | GRUB cmdline (plain) | `grep GRUB_CMDLINE_LINUX /install/etc/default/grub` | Contains `root=UUID=…` referencing the data filesystem UUID | Inspect `configure_grub_defaults` |
| **T11c** | GRUB cmdline (multi-LUKS) | encrypted config | `cryptdevice=UUID=…:Crypt-Root` and `root=/dev/mapper/Crypt-Root` (or btrfs subvolume equivalent) | Inspect `install_grub_with_layout` in `src/configure/bootloader.rs:130+` |
| **T11d** | GRUB cmdline (LVM-thin) | LVM-thin + LUKS config | `cryptdevice=UUID=…:Crypt-LVM` and `root=/dev/vg0/root` | Inspect `configure_grub_defaults_lvm_thin` |
| **T11e** | GRUB cryptodisk modules | `objdump -d /install/boot/efi/EFI/Artix/grubx64.efi 2>/dev/null \| strings \| grep -E 'cryptodisk\|luks2'` (or check `/install/etc/default/grub` for `GRUB_ENABLE_CRYPTODISK=y`) | `cryptodisk`, `luks`, `luks2`, `gcry_*` modules embedded | Inspect `GRUB_STANDALONE_MODULES` constant in `src/configure/bootloader.rs:17-23` |
| **T11f** | Pacman GRUB-reinstall hook | encrypted config; check `ls /install/etc/pacman.d/hooks/99-grub-reinstall.hook` | File present | Inspect `create_grub_reinstall_hook` in `src/configure/bootloader.rs` |

### T12 — Network + greetd + services

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T12a** | iwd config | `backend = "iwd"`; observe `/install/etc/iwd/main.conf` | Contains `EnableNetworkConfiguration=true` | Inspect `src/configure/network.rs` |
| **T12b** | NetworkManager + iwd | `backend = "networkmanager"`; observe `/install/etc/NetworkManager/conf.d/wifi_backend.conf` | Backend set to iwd | Inspect `src/configure/network.rs` |
| **T12c** | greetd config | DE selected; `cat /install/etc/greetd/config.toml` | Has `[terminal]`, `[default_session]`, default user matches `user.name` | Inspect `src/configure/greetd.rs` |
| **T12d** | Service enabled (runit) | `init = "runit"`; check `ls -l /install/etc/runit/runsvdir/default/` | Symlinks for selected services (seatd, iwd or NetworkManager+iwd, greetd if DE) pointing to `/etc/runit/sv/<svc>` | Inspect `enable_runit_service` in `src/configure/services.rs:178-206` |
| **T12e** | Service enabled (s6) | `init = "s6"`; check `ls /install/etc/s6/adminsv/default/contents.d/` | Touch files for `seatd-srv`, `iwd-srv`, etc. | Inspect `enable_s6_service` in `src/configure/services.rs:239-261` |
| **T12f** | elogind blacklist | DE + any init; `pacman -Q --root=/install elogind-runit 2>&1` (or per-init equivalent) | Returns `error: package 'elogind-runit' was not found` (only base elogind installed) | Inspect `build_service_packages` in `src/configure/services.rs:93-116` and the explicit skip at `:33-35` |

### T13 — Optional package collections

Run each package test with that flag enabled and all others disabled.

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T13a** | GPU drivers | `gpu_drivers = ["amd"]`; verify `pacman -Q --root=/install xf86-video-amdgpu vulkan-radeon` | Both packages installed | Inspect `install_gpu_drivers` in `src/configure/packages.rs` |
| **T13b** | Wine | `install_wine = true`; verify `pacman -Q --root=/install wine` | Installed | Inspect `install_wine_packages` |
| **T13c** | Gaming | `install_gaming = true`; verify `pacman -Q --root=/install steam gamescope-git` | Both present (gamescope-git from custom repo) | Inspect `install_gaming_packages` |
| **T13d** | Session switching | `install_gaming = true, install_session_switching = true, environment = "kde"`; verify scripts in `/install/usr/local/bin/` | `deploytix-session-manager.sh`, `return-to-gamemode.sh`, `session-select.sh`, `steam-gamescope-session.sh` all present | Inspect `setup_session_switching` in `src/configure/session_switching.rs` |
| **T13e** | yay AUR | `install_yay = true`; verify `pacman -Q --root=/install yay` | Installed (built from source) | Inspect `install_yay` |
| **T13f** | btrfs tools | `install_btrfs_tools = true, install_yay = true, filesystem = "btrfs"`; verify | `snapper`, `btrfs-assistant` installed | Inspect `install_btrfs_tools` |
| **T13g** | HHD | `install_hhd = true, install_yay = true`; verify | `hhd` package + init-specific service file at `/etc/{runit/sv,init.d,s6/sv,dinit.d}/hhd` | Inspect `install_hhd` |
| **T13h** | Decky Loader | `install_decky_loader = true, install_yay = true, install_gaming = true`; verify | `plugin_loader` service + binary; service enabled in init | Inspect `install_decky_loader` |
| **T13i** | evdevhook2 | `install_evdevhook2 = true, install_yay = true`; verify | `evdevhook2` package + udev rule + service; user added to `input` group | Inspect `install_evdevhook2` |
| **T13j** | sysctl gaming | `sysctl_gaming_tweaks = true`; verify `cat /install/etc/sysctl.d/99-gaming.conf` | Contains `vm.max_map_count`, `vm.swappiness` | Inspect `install_sysctl_gaming` |
| **T13k** | sysctl network | `sysctl_network_performance = true`; verify `cat /install/etc/sysctl.d/99-network-performance.conf` | Contains BBR + fq + larger socket buffers | Inspect `install_sysctl_network_performance` |

### T14 — Finalize + boot

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T14a** | initramfs regeneration | After install, `ls -lh /install/boot/initramfs-linux-zen.img` (must inspect before unmount step in T14b!) | File present, > 20 MiB; mtime within last few minutes | The chroot's mkinitcpio invocation failed silently — re-run with `RUST_LOG=debug` |
| **T14b** | Clean unmount | After install completes, `mount \| grep /install` | No mounts under `/install` | Inspect `unmount_all` in `src/install/chroot.rs:346-381` |
| **T14c** | LUKS containers closed | After install, `ls /dev/mapper/Crypt-*` | No matches | Inspect close-order in `Installer::finalize` (`src/install/installer.rs:962-998`) |
| **T14d** | First boot | Power-cycle, boot from target | GRUB menu appears; selecting Artix entry boots; greetd login prompt appears (if DE); user can log in | Boot to GRUB rescue → check `ls (hd0,gpt2)/grub/grub.cfg`; if missing, T11 failed silently |
| **T14e** | Boot with encryption | encrypted config; first boot | GRUB prompts for LUKS1 boot password (if `boot_encryption`); initramfs prompts for LUKS2 data password OR auto-unlocks via keyfile | If LUKS prompt loops, check `/etc/crypttab` mapper name matches initramfs `cryptdevice=` arg |
| **T14f** | Post-boot dmesg | `journalctl -k -b 0 \| grep -E 'WARN\|ERR'` | No mkinitcpio, btrfs, cryptsetup, or grub errors | Investigate per-error class in troubleshooting guide |
| **T14g** | NetworkManager auto-connect | DE + NetworkManager backend; after first boot | `nm-applet` in tray; can connect to wifi | T12b / T12d setup may be wrong |

### T15 — Rehearsal mode (full pipeline + auto-wipe)

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T15a** | Rehearsal completes successfully | `target/release/deploytix rehearse -c deploytix.toml -l rehearsal.log` | Final printed table shows ≥ 95% PASS; `Disk wiped: ✓`; exit 0 | Inspect failures in `rehearsal.log`; the first FAIL row is the short-circuit point |
| **T15b** | DiskWipeGuard runs on panic | Inject a panic before phase 3 (e.g. `panic!("test")` in `partition_disk`); run rehearsal | Disk wiped despite panic; `disk_wiped: true` in report | Inspect `Drop for DiskWipeGuard` in `src/rehearsal/guard.rs:200-207` |
| **T15c** | OperationRecord captures durations | After T15a, inspect `rehearsal.log` | Each operation has a duration; basestrap > 60 s; mkinitcpio > 5 s; cryptsetup luksFormat (with integrity) > 30 s on USB | Inspect `CommandRunner::record` in `src/utils/command.rs:106-117` |
| **T15d** | Rehearsal short-circuit message | Manually break a config rule (e.g. delete `device` field after validate); run rehearsal | Report includes `Short-circuited at: …` line | Inspect short-circuit capture in `src/rehearsal/mod.rs:60-66` |

### T16 — Cleanup + interrupt handling

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T16a** | Manual cleanup | Run `deploytix install`, abort partway with Ctrl-C, then run `deploytix cleanup --device $TARGET --wipe` | All `/install` mounts gone; all `Crypt-*` mappings closed; GPT wiped | Inspect `Cleaner::cleanup` in `src/cleanup/mod.rs:26-51` |
| **T16b** | Single SIGINT triggers emergency cleanup | During phase 4 (configure), send `kill -INT <pid>`; observe | Message `Interrupt received, cleaning up...`; emergency cleanup runs; mounts and mappings released; process exits with status 130 | Inspect `handle_signal` and `Installer::run` in `src/utils/signal.rs` and `src/install/installer.rs:115-150` |
| **T16c** | Second SIGINT forces exit | After first SIGINT, immediately send another | Message `Forced exit - cleanup may be incomplete`; default handler restored; process exits with signal | Inspect second-signal branch at `src/utils/signal.rs:41-49` |
| **T16d** | Orphaned cryptsetup killed | Start a `cryptsetup luksFormat --integrity` on a partition; SIGKILL deploytix mid-format; run cleanup | `Killing orphaned cryptsetup process (PID …)` in log; `/dev/mapper/temporary-cryptsetup-*` removed | Inspect `kill_orphaned_cryptsetup` in `src/cleanup/mod.rs:143-192` |

### T17 — GUI

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T17a** | Build with `gui` feature | `cargo build --release --features gui` | Builds; `target/release/deploytix-gui` present | Inspect `Cargo.toml` `[features]` section |
| **T17b** | Single-instance lock | Run two `deploytix-gui` processes back-to-back | Second exits with `Deploytix GUI is already running (lock file /tmp/deploytix-gui.lock exists)` | Inspect `LOCK_PATH` block in `src/gui_main.rs:13-32` |
| **T17c** | Lock cleanup on exit | Close GUI normally; check `/tmp/deploytix-gui.lock` | File removed | Inspect `LockGuard::drop` in `src/gui_main.rs:35-41` |
| **T17d** | Configure validation gate | Start GUI; leave fields empty; observe Next button | "Next →" disabled until disk selected + filesystem valid + system valid + user valid | Inspect `panels::configure::show` return value at `src/gui/panels/configure.rs:51-77` |
| **T17e** | Background install thread | Start install via GUI; observe progress bar | Bar increments; log lines stream in; final "Installation completed successfully!" | Inspect `start_installation` in `src/gui/app.rs:191-254` |
| **T17f** | Save config from Summary | On Summary screen, click Save; check `deploytix.toml` in cwd | TOML written; matches GUI choices | Inspect `save_config` in `src/gui/app.rs:149-169` |
| **T17g** | Theme audio plays | Listen during install | `theme.wav` audible (looped) | If silent: check `XDG_RUNTIME_DIR`, ALSA error log; inspect `src/resources/audio.rs:78-113` |

### T18 — Pkgdeps subsystem (independent)

| ID | Validates | Procedure | Pass criteria | Fail action |
|----|-----------|-----------|---------------|-------------|
| **T18a** | Resolve | `deploytix deps resolve linux-zen` | Prints closure; non-empty | Inspect `cmd_resolve` in `src/pkgdeps/cli.rs` |
| **T18b** | Tree | `deploytix deps tree linux-zen` | Tree output; root node `linux-zen` | Inspect `cmd_tree` |
| **T18c** | Graph DOT | `deploytix deps graph linux-zen` | Valid Graphviz DOT (parsable by `dot -Tpng`) | Inspect `src/pkgdeps/graph.rs` |
| **T18d** | Offline mode | `deploytix deps resolve foo --offline tests/fixtures/sample.json` | Reads from JSON without invoking pacman | Inspect `MockSource` in `src/pkgdeps/source.rs` |
| **T18e** | Integration tests | `cargo test --all-features --test pkgdeps_integration` | All tests pass | Inspect `tests/pkgdeps_integration.rs` |

## Fix-Specific Validation Procedures

### Fix-1: Changes to `compute_layout_from_config` (`src/disk/layouts.rs`)

- **Before/after**: dump layout summary for matrix configs (encryption=F/T × use_lvm_thin=F/T × filesystem=btrfs/ext4/zfs); diff old vs. new.
- **Regression**: T1a, T1b, T1c, T1d, T2a, T5a–T5e.

### Fix-2: Changes to `construct_hooks` (`src/configure/mkinitcpio.rs`)

- **Before/after**: produce HOOKS string for each row of the layout-by-flag matrix (overview document); diff.
- **Regression**: T10a, T10b, T10c, T10d, T10e, T14d, T14e, T14f.
- **Boot test required**: any change here can break boot — run T15 (rehearsal) and T14 (real install + boot).

### Fix-3: Changes to `prepare_deploytix_repo` (`src/install/basestrap.rs`)

- **Before/after**: state of `/tmp/deploytix-pacman.conf` and `/tmp/deploytix-local-repo/` after install on (a) live ISO, (b) clean Artix host with pre-built packages, (c) clean Artix host without pre-built packages.
- **Regression**: T6a, T6b, T6c, T6e, T13c (gaming uses gamescope-git from custom repo).

### Fix-4: Changes to `enable_service` family (`src/configure/services.rs`)

- **Before/after**: ls of `/install/etc/<init>/...` for each init, filtered to only services in `build_service_list` output.
- **Regression**: T12d, T12e, T12f for the affected init; T13g/T13h/T13i for HHD/Decky/evdevhook2 enable paths.

### Fix-5: Changes to `emergency_cleanup` (`src/install/installer.rs`)

- **Before/after**: trace cleanup steps via `RUST_LOG=info` after a deliberate Ctrl-C in phase 4.
- **Regression**: T15b, T16a, T16b, T16c, T16d.

### Fix-6: Changes to `DeploymentConfig::validate()` (`src/config/deployment.rs`)

- **Before/after**: `deploytix validate` against the existing `deploytix.toml` baseline + matrix configs.
- **Regression**: T0a, T0b, T0c.
- **Note**: pure rules cannot be unit-tested in isolation today (`src/config/deployment.rs:1414-1423`). Fix-2 in the troubleshooting guide proposes splitting them out.

### Fix-7: Changes to GRUB cmdline construction (`src/configure/bootloader.rs`)

- **Before/after**: dump `/install/etc/default/grub` for matrix configs; diff.
- **Regression**: T11a–T11f, then T14d–T14e (real boot test) for any non-trivial change. *Do not skip the boot test.*

## Regression Test Matrix

| Files changed | Run tests |
|---------------|-----------|
| `src/config/deployment.rs` | T0a, T0b, T0c |
| `src/disk/layouts.rs` | T1a–T1d, T2a, T5a–T5e |
| `src/disk/partitioning.rs` | T2a, T2b, T2c, T2d |
| `src/disk/formatting.rs`, `src/disk/lvm.rs` | T4a–T4c, T5a–T5e |
| `src/install/installer.rs` | T15a, T15b, T16a, T16b + the affected phase tests |
| `src/install/basestrap.rs` | T6a–T6e, T13c |
| `src/install/chroot.rs` | T5a–T5e, T14b |
| `src/install/fstab.rs` | T7a–T7c, T14d |
| `src/install/crypttab.rs` | T7d, T14e |
| `src/configure/encryption.rs`, `src/configure/keyfiles.rs` | T3a–T3e, T7d, T7e, T14e |
| `src/configure/mkinitcpio.rs`, `src/configure/hooks.rs` | T10a–T10e, T14a, T14d, T14e, T14f |
| `src/configure/bootloader.rs` | T11a–T11f, T14d, T14e |
| `src/configure/services.rs`, `src/configure/greetd.rs` | T12a–T12f |
| `src/configure/network.rs` | T12a, T12b, T14g |
| `src/configure/packages.rs` | T13a–T13k (corresponding subset) |
| `src/configure/swap.rs` | T8a–T8d |
| `src/configure/locale.rs` | T9b, T9c |
| `src/configure/users.rs` | T9d |
| `src/configure/secureboot.rs` | dedicated SecureBoot run on UEFI hardware |
| `src/cleanup/mod.rs`, `src/utils/signal.rs` | T16a–T16d |
| `src/rehearsal/*` | T15a–T15d |
| `src/gui/*`, `src/gui_main.rs` | T17a–T17g |
| `src/resources/audio.rs`, `src/resources/alsa_noop.c`, `build.rs` | T17g (and: confirm `cargo build --release` still produces a binary that links libalsa_noop.a) |
| `src/pkgdeps/*` | T18a–T18e |
| `Cargo.toml`, `.cargo/config.toml`, `Makefile` | full T0–T15 (release build) + `make portable` smoke test |

## Test Logging Template

Append one row per test session. Use a fixed-field format so sessions can be diffed.

```
session: 2026-05-04T15:30Z
operator: <name>
host: <hostname>
host_init: <runit|openrc|s6|dinit>
target_device: /dev/sdX
target_size_gib: 64
config_baseline_sha256: <sha256 of deploytix.toml>
deploytix_version: v1.3.0
deploytix_commit: <git rev-parse --short HEAD>

gates:
  G1: PASS
  G2: PASS
  G3: PASS
  G4: PASS
  G5: PASS
  G6: PASS
  G7: PASS
  G8: PASS
  G9: PASS

results:
  T0a: PASS
  T0b: PASS
  T0c: PASS
  T1a: PASS
  T1b: PASS
  T1c: PASS
  T1d: PASS
  T2a: PASS
  T2b: SKIP    # 512-byte sector device only
  T2c: PASS
  T2d: SKIP    # preserve_home not in this matrix
  ...
  T15a: PASS
  T16a: PASS

short_circuit_at: (none | TXX: <reason>)
overall: PASS | FAIL

notes: |
  Free-form. Mention any out-of-band observations:
  - Audio crackled during basestrap (expected on USB 2)
  - dmesg showed brltty udev warnings (cosmetic, ignore)
```

## Acceptance Criteria

A change to deploytix is validated only when **all** of:

1. `cargo build --release --features gui` succeeds.
2. `cargo clippy --all-features -- -D warnings` succeeds.
3. `cargo fmt -- --check` succeeds.
4. `cargo test --all-features` passes (unit + `pkgdeps_integration`).
5. The relevant subset of T0–T18 from the regression matrix passes against the changed files.
6. **For any change touching the install pipeline**: T15 (rehearsal) passes against the canonical `deploytix.toml`.
7. **For any change to mkinitcpio HOOKS, GRUB cmdline, fstab/crypttab generation, or LUKS/LVM setup**: T14d + T14e (real install + boot) pass on hardware/VM (not just rehearsal — rehearsal does not boot the resulting system).
8. No new `WARN` lines in `journalctl -k -b 0` of the booted target system that were not present before the change.
9. The session log row is filed (e.g. in `docs/test-logs/` or wherever the team keeps them).

A failure in any acceptance criterion blocks the merge until resolved or formally waived in the PR description.
