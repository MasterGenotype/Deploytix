# Deploytix Conceptual Errors Review

A systematic review of architectural misunderstandings, logical contradictions,
and design-level errors in the codebase. This does not cover simple bugs or
missing features, but rather cases where the code's mental model is wrong.

---

## 1. BIOS Boot Partition Misidentified as a Filesystem Partition

**Files:** `src/disk/layouts.rs:402-415`, `src/disk/formatting.rs:116-117`

The CryptoSubvolume layout creates partition 2 as a "BIOS Boot" partition
(the name is "BIOS", it has `LegacyBIOSBootable` attribute), but it is
configured with:

- `type_guid: LINUX_FILESYSTEM` (instead of `BIOS_BOOT`)
- `is_bios_boot: false`
- `is_boot_fs: true`
- `mount_point: Some("/boot")`

This is conceptually incoherent. A BIOS Boot partition (GUID
`21686148-6449-6E6F-744E-656564454649`) is a small (~1 MiB) unformatted
partition that GRUB embeds its core image into. It should never be mounted or
have a filesystem. What the code actually creates is a separate `/boot`
filesystem partition, which is a different concept. The constant
`BIOS_BOOT_MIB = 650` and the comment "BIOS Boot partition size (650 MiB
for GRUB)" further reveal the confusion: a real BIOS Boot partition is 1 MiB.

The code should either:
- Call it a "boot partition" and drop the BIOS references, OR
- Create an actual 1 MiB BIOS Boot partition (type GUID `BIOS_BOOT`,
  no filesystem, no mount point) AND a separate `/boot` partition

Currently the `BIOS_BOOT` type GUID constant is defined but never used.

---

## 2. fstab Hardcodes btrfs for All Non-Encrypted Layouts

**File:** `src/install/fstab.rs:50-56`

The `generate_fstab()` function hardcodes filesystem type and mount options
to `"btrfs"` and `"defaults,noatime,compress=zstd"` for every non-EFI
partition, regardless of what filesystem the user actually chose:

```rust
} else if mount_point == "/" {
    ("btrfs", "defaults,noatime,compress=zstd", 1)
} else {
    ("btrfs", "defaults,noatime,compress=zstd", 2)
};
```

If a user selects ext4, xfs, or f2fs, the fstab will still say `btrfs`
with btrfs-specific options. This means any non-btrfs installation will
produce a non-bootable system. The filesystem type should be derived from
`config.disk.filesystem` (which is not even passed to this function).

---

## 3. Encryption Enabled Outside CryptoSubvolume Has No Effect

**Files:** `src/install/installer.rs:55-62`, `src/config/deployment.rs:373-379`

The wizard allows enabling encryption for Standard and Minimal layouts,
but the installer only performs encryption setup for `CryptoSubvolume`:

```rust
if self.config.disk.layout == PartitionLayout::CryptoSubvolume {
    self.setup_encryption()?;
    // ...
} else {
    self.format_partitions()?;  // No encryption at all
    self.mount_partitions()?;
}
```

Similarly, crypttab generation, custom hooks, and the encrypted bootloader
path are all gated on `CryptoSubvolume`. The validation in `deployment.rs`
permits `encryption: true` on any layout, and the `encrypt` hook is added
to mkinitcpio for non-CryptoSubvolume encrypted layouts (line 59 of
`mkinitcpio.rs`), but there is no code to actually create/open LUKS
containers for these layouts. The user would configure encryption, get
encryption-related mkinitcpio hooks, but the partitions would be plain
unencrypted.

This is a conceptual incompleteness: the configuration model promises
encryption support across layouts, but the implementation only delivers it
for one.

---

## 4. CryptoSubvolume fstab Missing Boot Partition Entry

**File:** `src/install/fstab.rs:96-156`

The `generate_fstab_crypto_subvolume()` function generates entries for
btrfs subvolumes and EFI, but omits the `/boot` partition entirely. The
installer mounts a separate btrfs `/boot` partition (partition 2 in
`installer.rs:341-357`), but fstab never records it. After reboot, `/boot`
would not be mounted automatically, which means kernel updates and
mkinitcpio regeneration would write to the wrong location.

---

## 5. Runit Service Symlink Targets Are Absolute Host Paths

**File:** `src/configure/services.rs:83-103`

The `enable_runit_service()` function creates symlinks where both the
source and target contain the `install_root` prefix:

```rust
let service_dir = format!("{}/etc/runit/sv/{}", install_root, service);
let link_path = format!("{}/{}", enabled_dir, service);
std::os::unix::fs::symlink(&service_dir, &link_path)?;
```

This creates a symlink like:
```
/install/etc/runit/runsvdir/default/iwd -> /install/etc/runit/sv/iwd
```

After the installation is deployed and `/install` is no longer the root,
this symlink points to a non-existent path. The symlink target should be
the path relative to the installed system's root:
```
/install/etc/runit/runsvdir/default/iwd -> /etc/runit/sv/iwd
```

The same bug exists in `enable_dinit_service()` at line 139-155.

---

## 6. `systemd-boot` Offered on an Init-System That Lacks systemd

**Files:** `src/config/deployment.rs:205-220`, `src/configure/bootloader.rs:207-271`

The code comments acknowledge "systemd-boot requires systemd, which is not
the default on Artix" but still presents it as a valid option in the
configuration wizard and sample config. Artix Linux uses alternative init
systems (runit, OpenRC, s6, dinit) precisely to avoid systemd. If no init
system option provides systemd, then `systemd-boot` will always fail because
`bootctl` is a systemd utility. The `basestrap.rs` package list never
installs `systemd-boot` or `systemd` packages.

The option should either be removed, gated on a (currently nonexistent)
systemd init option, or replaced with a comment/warning about standalone
systemd-boot packages.

---

## 7. `mountcrypt` Hook Doesn't Mount `/boot` (Only EFI)

**File:** `src/configure/hooks.rs:229-331`

The custom `mountcrypt` initramfs hook mounts the root btrfs subvolumes
and auto-detects the EFI partition, but never mounts `/boot`. The `/boot`
partition (partition 2 in CryptoSubvolume layout) holds the kernel and
initramfs. If `/boot` is not mounted during early boot, the system will
operate without a proper `/boot` mount until fstab takes over later. This
creates a gap where kernel updates or initramfs regeneration during early
init could fail.

More importantly, the `@boot` subvolume defined in `default_subvolumes()`
(layouts.rs:462-466) conflicts with the separate `/boot` partition. The
CryptoSubvolume layout has BOTH:
- A btrfs `@boot` subvolume inside the LUKS container
- A separate unencrypted btrfs partition 2 mounted at `/boot`

These would compete for the `/boot` mountpoint. The subvolume mount in
fstab would be masked by the physical partition mount, or vice versa,
depending on mount order. One of these should be removed.

---

## 8. `swapoff -a` in Cleanup Disables All System Swap

**Files:** `src/install/chroot.rs:68`, `src/cleanup/mod.rs:55`

Both the installer's `unmount_all()` and the cleaner's `unmount_all()`
run `swapoff -a`, which disables ALL swap on the host system, not just
the swap partition belonging to the installation target. If running from
a live environment that uses swap, this would degrade host performance.
The code should only disable swap on the specific partition it enabled.

---

## 9. Sector Size Hardcoded to 512 in Partitioning

**File:** `src/disk/partitioning.rs:15`

The `generate_sfdisk_script()` function hardcodes `sector_size = 512`,
despite the comment "Default, could be read from sysfs". This contradicts
`detection.rs:139` which correctly reads the logical block size from sysfs.

On 4Kn NVMe drives (4096-byte sectors), all partition size calculations
would be wrong by a factor of 8, producing an unusable partition table.
The sector size should be read from sysfs consistently.

---

## 10. `format_all_partitions` Skips Boot Partition for CryptoSubvolume

**File:** `src/disk/formatting.rs:106-124`

The `format_all_partitions()` function checks `part.is_bios_boot` to
decide whether to format a partition as boot. But in the CryptoSubvolume
layout, partition 2 has `is_bios_boot: false` and `is_boot_fs: true`.
The function doesn't check `is_boot_fs`, so the boot partition falls
through to the generic `format_partition()` call. This happens to work
(it still gets formatted as btrfs via the generic path), but only by
accident rather than design. The logic doesn't match the data model.

The installer's `mount_crypto_subvolumes()` separately handles formatting
boot, which means the boot partition would get formatted twice if
`format_all_partitions` were ever called before it.

---

## 11. `crypttab-unlock` Hook Rewrites Mapper Names, Breaking Assumptions

**File:** `src/configure/hooks.rs:146-160`

The hook script takes the name from `/etc/crypttab` and reformats it:
```bash
formatted_mapping=$(echo "$mapping" | awk '{print toupper(substr($0,1,1)) tolower(substr($0,2))}')
cmd="cryptsetup open $device Crypt-$formatted_mapping"
```

The crypttab generator writes the mapper name as `Root` (stripped of
`Crypt-` prefix, per `crypttab.rs:25-29`). The hook then prepends `Crypt-`
and capitalizes to get `Crypt-Root`.

But if the user configures `luks_mapper_name` as anything other than
`Crypt-Root` (e.g., `Crypt-Data`), the crypttab writes `Data`, the hook
reads it, formats it as `Data` -> `Crypt-Data`. This works. However, if
the mapper name doesn't follow the `Crypt-X` convention (e.g., just
`myvolume`), then `trim_start_matches("Crypt-")` is a no-op, the
crypttab writes `myvolume`, and the hook creates `Crypt-Myvolume`. This
won't match the `mountcrypt` hook which expects `/dev/mapper/{mapper_name}`
using the original config value. The system would fail to boot.

The conceptual error is splitting the mapper name across two independent
transformations (Rust code strips prefix, shell script re-adds prefix)
instead of using the name consistently end-to-end.

---

## 12. Command Injection via Shell String Interpolation in `users.rs`

**File:** `src/configure/users.rs:30-38`

User-supplied values (username, password, groups) are interpolated
directly into shell command strings:

```rust
let useradd_cmd = format!("useradd -m -G {} -s /bin/bash {}", groups_str, username);
let chpasswd_cmd = format!("echo '{}:{}' | chpasswd", username, password);
```

These are passed to `bash -c` via `run_in_chroot`. A password containing
a single quote (e.g., `it's`) would break out of the echo quoting. A
maliciously crafted username or password could execute arbitrary commands
as root in the chroot.

While `encryption.rs` correctly uses stdin piping to avoid this, the
user management code uses the vulnerable pattern. The validation in
`deployment.rs` only checks for empty strings and spaces in usernames;
it does not sanitize against shell metacharacters.

---

## 13. `DnsProvider::Systemd` Is Offered But Cannot Work

**Files:** `src/config/deployment.rs:247`, `src/configure/network.rs:27`, `src/install/basestrap.rs:85-87`

`DnsProvider::Systemd` is available as a config option, but:
- `configure_network()` does nothing for it (empty match arm)
- `build_package_list()` comments "systemd-resolved is part of systemd,
  not available on Artix" and installs nothing
- No service is enabled for it

This option silently produces a system with no configured DNS resolver.

---

## 14. `HOOKS` Line Uses Wrong Syntax in mkinitcpio.conf

**File:** `src/configure/mkinitcpio.rs:119`

The generated config writes:
```
HOOKS="base udev autodetect ..."
```

But mkinitcpio.conf uses parenthesized arrays, not quoted strings:
```
HOOKS=(base udev autodetect ...)
```

The `MODULES`, `BINARIES`, and `FILES` lines correctly use `()` syntax
(lines 116-118), but `HOOKS` uses double quotes. Depending on the
mkinitcpio version, this may fail to parse or be interpreted incorrectly.

---

## 15. CryptoSubvolume Boot Partition Gets Wrong Format Treatment

**File:** `src/install/installer.rs:341-357`

The installer calls `format_boot()` to format the boot partition as
btrfs. But earlier, `format_all_partitions()` is NOT called for
CryptoSubvolume layout (the installer takes the `else` branch only for
non-crypto). Instead, `mount_crypto_subvolumes()` formats both boot and
EFI inline. However, the `@boot` subvolume from `default_subvolumes()`
is ALSO mounted at `/boot` as part of `mount_btrfs_subvolumes()`. The
flow is:

1. Mount btrfs subvolumes including `@boot` at `/install/boot`
2. Format partition 2 as btrfs (overwrites nothing yet)
3. Mount partition 2 at `/install/boot` (shadows the subvolume mount)

The subvolume mount from step 1 is effectively hidden. This means writes
to `/boot` go to the separate partition, but the btrfs subvolume `@boot`
inside the LUKS container exists unused. The fstab references `@boot`
subvolume but the physical partition would be mounted over it.

---

## Summary

| # | Severity | Category | Location |
|---|----------|----------|----------|
| 1 | High | Wrong abstraction | layouts.rs, formatting.rs |
| 2 | Critical | Wrong output | fstab.rs |
| 3 | High | Incomplete model | installer.rs |
| 4 | High | Missing entry | fstab.rs |
| 5 | Critical | Wrong path | services.rs |
| 6 | Medium | Invalid option | deployment.rs, bootloader.rs |
| 7 | High | Conflicting design | hooks.rs, layouts.rs |
| 8 | Low | Overly broad | chroot.rs, cleanup/mod.rs |
| 9 | High | Wrong assumption | partitioning.rs |
| 10 | Low | Logic mismatch | formatting.rs |
| 11 | High | Fragile coupling | hooks.rs, crypttab.rs |
| 12 | Critical | Injection | users.rs |
| 13 | Medium | Dead option | deployment.rs, network.rs |
| 14 | High | Wrong syntax | mkinitcpio.rs |
| 15 | High | Conflicting mounts | installer.rs, layouts.rs |
