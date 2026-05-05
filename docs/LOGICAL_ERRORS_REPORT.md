# Logical Errors Report

Systematic code review of the Deploytix codebase, organized by severity.

---

## Critical Severity

### 1. Hardcoded "btrfs" filesystem in multi-volume fstab generation

**File:** `src/install/fstab.rs:367-370`

`generate_fstab_multi_volume` hardcodes `btrfs` as the filesystem type and `compress=zstd` as mount options for all encrypted volumes, regardless of the actual configured filesystem. If a user configures multi-volume encryption with ext4, xfs, or f2fs, the generated fstab will have wrong entries and the system will fail to boot.

```rust
// Always says "btrfs" even if the filesystem is ext4/xfs/f2fs
"UUID={}  {}  btrfs  defaults,noatime,compress=zstd  0  {}\n\n",
```

### 2. Hardcoded "btrfs" filesystem in LVM thin fstab generation

**File:** `src/install/fstab.rs:480-484`

Same issue as above but in `generate_fstab_lvm_thin`. All thin volume entries are hardcoded to `btrfs` with btrfs-specific options, breaking non-btrfs configurations.

```rust
// Always says "btrfs" even if the filesystem is ext4/xfs/f2fs
"UUID={}  {}  btrfs  defaults,noatime,compress=zstd  0  {}\n\n",
```

### 3. Missing `rootflags=subvol=@` for encrypted btrfs with subvolumes

**File:** `src/configure/bootloader.rs:546-568`

In `configure_grub_defaults`, when a LUKS mapper is present (encrypted system), the code sets `root=/dev/mapper/{mapper}` but never adds `rootflags=subvol=@` even when `uses_subvolumes` is true. The `rootflags` line at 564 only executes in the non-encrypted `else` branch. An encrypted btrfs system with subvolumes would mount the wrong subvolume (or top-level volume) and fail to boot.

```rust
if let Some(mapper) = mapper_name {
    cmdline_parts.push(format!("root=/dev/mapper/{}", mapper));
    cmdline_parts.push("rw".to_string());
    // BUG: rootflags=subvol=@ is never added here
} else if ... {
} else {
    // rootflags only added in this non-encrypted branch
    if uses_subvolumes {
        cmdline_parts.push("rootflags=subvol=@".to_string());
    }
}
```

### 4. Crypttab mapper name mismatch for LVM container

**File:** `src/install/installer.rs:1570-1574`

`generate_crypttab_lvm_thin` uses `container.volume_name` ("Lvm") as the crypttab target name, but the LUKS container was opened with mapper name "Crypt-LVM" (line 1297). At boot, initramfs will try to open the LUKS device with name "Lvm" but look for `/dev/mapper/Crypt-LVM`, causing a name mismatch that can prevent boot.

```rust
// Crypttab writes "Lvm" as target name
"{}  UUID={}  {}  luks\n", container.volume_name, luks_uuid, lvm_keyfile
// But the device was opened as "Crypt-LVM" at line 1297
```

---

## High Severity

### 5. ZRAM runit service created but never enabled

**File:** `src/configure/swap.rs:55-108`

`setup_zram_runit` creates service files under `/etc/runit/sv/zram/` but never creates the required symlink in `/etc/runit/runsvdir/default/zram` to enable the service. ZRAM will not start at boot on runit systems.

### 6. ZRAM OpenRC service created but never enabled

**File:** `src/configure/swap.rs:111-159`

`setup_zram_openrc` creates the init script but never runs `rc-update add zram default`. ZRAM will not start at boot on OpenRC systems.

### 7. ZRAM dinit service created but never enabled

**File:** `src/configure/swap.rs:240-280`

`setup_zram_dinit` creates the service file but does not create a symlink in `/etc/dinit.d/boot.d/`. ZRAM will not start at boot on dinit systems.

### 8. `is_device_mounted` never matches partitioned disks

**File:** `src/disk/detection.rs:117-122`

The function checks if a whole-disk path (e.g., `/dev/sda`) appears in `/proc/mounts`, but `/proc/mounts` lists partition devices (e.g., `/dev/sda1`). A disk with mounted partitions will never match, making this check effectively a no-op for partitioned disks. A disk in active use could be selected as an installation target.

```rust
fn is_device_mounted(device: &str) -> bool {
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    // Only matches exact device path, never matches partitions of the device
    mounts.lines().any(|line| line.split_whitespace().next() == Some(device))
}
```

### 9. `lv_mapper_path` fails for VG/LV names containing hyphens

**File:** `src/disk/lvm.rs:253-255`

Device-mapper doubles hyphens in VG/LV names (e.g., VG `my-vg` + LV `my-lv` becomes `/dev/mapper/my--vg-my--lv`). This function does not escape hyphens, producing an incorrect path that won't match the actual device node.

```rust
pub fn lv_mapper_path(vg_name: &str, lv_name: &str) -> String {
    format!("/dev/mapper/{}-{}", vg_name, lv_name) // No hyphen escaping
}
```

---

## Medium Severity

### 10. `fsck_pass` always returns 0, contradicting its documentation

**File:** `src/install/fstab.rs:35-47`

The doc comment says ext4 should return pass 1 for root and pass 2 for other ext4 mounts, but the function unconditionally returns 0. The same issue affects `boot_fs_fstab_entry` at line 23, where ext4 returns pass 0 despite the comment saying it should be pass 2. ext4 filesystems will never be checked at boot.

### 11. EFI mount options inconsistent across fstab generators

**File:** `src/install/fstab.rs`

- Lines 121, 146: `"defaults,noatime"` (no umask) in `generate_fstab`
- Lines 258, 407, 549: `"umask=0077,defaults"` in subvolume/multi-volume/LVM paths

The missing `umask=0077` in the regular `generate_fstab` path is a security issue — the EFI partition will be world-readable.

### 12. EFI fsck pass inconsistent across fstab generators

**File:** `src/install/fstab.rs`

- Line 147: pass `2` in `generate_fstab` (non-ZFS)
- Lines 258, 407, 549: pass `0` in all other generators

### 13. `wait_for_device` timeout is half the intended duration

**File:** `src/configure/hooks.rs:110-124`

The shell function sets `timeout=30` and decrements by 1 per iteration, but sleeps 0.5s per iteration. Actual timeout is 15 seconds, not 30. Same issue in `wait_for_mapper` (lines 127-140) where `timeout=20` gives 10 seconds.

### 14. Missing newline when writing password to `cryptsetup luksAddKey`

**File:** `src/configure/keyfiles.rs:84-88`

`add_keyfile_to_luks` uses `stdin.write_all(password.as_bytes())` without a trailing newline, unlike `encryption.rs` which uses `writeln!()`. This could cause `cryptsetup` to hang or read the password incorrectly depending on buffering behavior.

### 15. Missing discard option in LVM thin crypttab entry

**File:** `src/install/installer.rs:1573`

The LVM container's crypttab entry hardcodes `luks` without `discard`. Other code paths use `crypttab_options()` which returns `luks,discard` when appropriate. SSD performance is degraded without TRIM support.

### 16. Swap not activated during LVM thin volume mounting

**File:** `src/install/installer.rs` (`mount_lvm_volumes` method, ~lines 1444-1518)

`mount_lvm_volumes` mounts thin volumes, boot, and EFI but never activates swap partitions (no `swapon` call). Other mount paths (`mount_partitions_inner`, `mount_partitions_zfs`) include explicit swap activation.

### 17. `wipe_partition_table` doesn't zero the last MB of disk

**File:** `src/disk/partitioning.rs:162-183`

Comment says "zero the first and last MB to ensure clean state" but only the first MB is zeroed. Stale backup GPT headers at the end of the disk can confuse partitioning tools.

---

## Low Severity

### 18. `determine_device_type` misclassifies all removable devices as "usb"

**File:** `src/disk/detection.rs:83-86`

The function checks `removable == 1` and returns `"usb"` for any removable device that isn't MMC. Non-USB removable devices (internal SD readers via SATA, etc.) would be misclassified.

### 19. `entries_mount_order` includes non-mountable LVM entries

**File:** `src/disk/volumes.rs:181-185`

LVM PV entries have an empty `mount_point` string (0 slashes), causing them to sort to the front as the shallowest mount point. They should be filtered out since they aren't mountable.

### 20. `nm-applet.desktop` autostart always installed regardless of network backend

**File:** `src/configure/packages.rs:489-500`

`install_autostart_entries` always installs `nm-applet.desktop` even when the user chose Iwd instead of NetworkManager, causing errors or failed autostart at login.

### 21. `swapoff -a` in cleanup disables all host swap

**File:** `src/cleanup/mod.rs:58`

The cleanup runs `swapoff -a` which disables ALL swap on the host system, not just swap set up by the installer. On a live system, this could cause OOM issues.

### 22. Silent failure cascade in cleanup

**File:** `src/cleanup/mod.rs:80, 119`

`unmount_all` and `close_encrypted_volumes` silently discard all errors (`let _ = ...`). If unmounts fail (busy filesystem), crypto close also fails, but the tool reports "Cleanup complete" and may proceed to wipe a still-mounted filesystem.

### 23. Several operations bypass `CommandRunner` for raw `std::process::Command`

**Files:**
- `src/disk/partitioning.rs:116-123` — `sfdisk`
- `src/disk/partitioning.rs:171` — `dd`
- `src/disk/formatting.rs:517-519` — `btrfs subvolume list`
- `src/cleanup/mod.rs:237-257` — `sfdisk` and `fdisk`

These bypass logging, privilege handling, and dry-run checks provided by `CommandRunner`. Currently guarded by early dry-run returns, but fragile if code is restructured.

### 24. Wizard auto-inserted root partition can create duplicate remainder

**File:** `src/config/deployment.rs:655-668`

When no root partition is defined, the wizard inserts one with `size_mib: 0` (remainder). If the user already defined a different partition with `size_mib == 0`, this creates two remainder partitions that pass the wizard but fail later in `validate()` with a confusing error.

### 25. No validation of `zram_percent` range

**File:** `src/config/deployment.rs`

`lvm_thin_pool_percent` is validated to be 1-100, but `zram_percent` has no validation. A value of 0 makes `ZramOnly` swap useless; values above 100 attempt to allocate more ZRAM than available RAM.
