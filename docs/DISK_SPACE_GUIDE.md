# Tutorial: How Much Disk Space Do You Need?

Deploytix can target very different kinds of storage — a USB flash drive you
carry in your pocket, an internal SSD/NVMe drive in a laptop or handheld, or a
spinning HDD repurposed as a Linux box. The "right" disk size depends on
**which installation media you pick** and **which features you enable** in
`[packages]` and `[disk]`. This tutorial walks through both, and ends with a
sizing table you can use as a starting point.

## 1. How Deploytix sees your media

`deploytix list-disks` reports each candidate device's `device_type`
(`removable`, `nvme`, `ssd`, `hdd`, `sd`, `mmc`, `loop`, `disk`) and a
`removable` flag (read from `/sys/block/<dev>/removable`). That distinction
matters because it tells you what kind of workload the target can sustain:

| Media kind | Typical examples | Characteristics |
|------------|------------------|-----------------|
| **Removable** | USB flash drives, SD/MMC cards | Portable, but slower random I/O and limited write endurance — keep installs lean |
| **SSD / NVMe** | Internal SATA SSD, M.2 NVMe | Fast random I/O, handles large package sets, snapshots, and swap well |
| **HDD** | Internal spinning disks | High capacity, slow random I/O — favor fewer/larger partitions and avoid heavy swapping |

Run `deploytix list-disks --all` before committing to a layout so you know
exactly how much raw capacity (`size_bytes`) you are working with — Deploytix
will refuse to lay out partitions on a disk that's too small
(`DeploytixError::DiskTooSmall`).

## 2. What Deploytix always reserves

Regardless of media, every layout prepends the same fixed-size system
partitions before your data partitions are allocated (`src/disk/layouts.rs`):

| Partition | Size | Notes |
|-----------|------|-------|
| EFI | 512 MiB | `EFI_MIB` |
| Boot | 2 GiB | `BOOT_MIB`; becomes a `@boot` btrfs subvolume when `boot_filesystem = "btrfs"` |
| Swap (if `swap_type = "partition"`) | `2 × RAM`, clamped to 4–20 GiB | `calculate_swap_mib()`; ZRAM/swap-file modes don't consume a partition |

That's a **minimum reservation of roughly 6.5 GiB** (512 MiB + 2 GiB + 4 GiB
swap floor) before a single byte goes to `/`, `/usr`, `/var`, or `/home`.

On top of that, the default data partitions (`default_partitions()`) reserve:

| Mount point | Default size |
|-------------|--------------|
| `/` | 20 GiB |
| `/usr` | 30 GiB |
| `/var` | 10 GiB |
| `/home` | remainder of the disk |

So a stock layout needs **~66 GiB just for the fixed-size partitions**
(EFI + Boot + Swap + `/` + `/usr` + `/var`) before `/home` gets anything
meaningful. Anything smaller and you'll need a `[[disk.partitions]]` override
with smaller `size_mib` values (see [README → Partition Configuration](../README.md#partition-configuration)).

## 3. Feature overhead to budget for

Beyond the base layout, several optional features change how much space you
actually need:

- **Desktop environment** (`[desktop] environment = "kde" | "gnome" | "xfce"`)
  adds several GiB of packages on top of a minimal/no-DE install.
- **Gaming & handheld stack** (`install_gaming`, `install_wine`,
  `install_hhd`, `install_decky_loader`, GPU drivers, etc.) is the single
  largest consumer — Steam libraries, Wine prefixes, and GPU driver stacks
  routinely add tens of GiB, and game installs themselves can dwarf the OS.
  Plan for a much larger `/home` (or a dedicated games partition) if you
  enable these.
- **Encryption** (`disk.encryption`, `integrity`) adds negligible static
  overhead but rules out some space-saving tricks (e.g. `discard`/TRIM is
  disabled when `integrity = true`), so leave more headroom for filesystem
  metadata and avoid running a volume close to full.
- **Btrfs subvolumes & snapshots** (`use_subvolumes`, `install_btrfs_tools`
  for snapper) mean old snapshots continue to occupy space after you "delete"
  files. Budget extra free space — btrfs performance and balance operations
  degrade badly when a filesystem is nearly full.
- **LVM thin provisioning** (`use_lvm_thin`) lets you overcommit virtual
  volume sizes, but the underlying thin pool is still bounded by physical
  disk space — overcommitting without monitoring can fill the pool and take
  the system down.
- **Swap type** (`swap_type = "partition" | "file" | "zram"`) — a swap
  *partition* consumes fixed disk space up front; a swap *file* or *ZRAM*
  trades that for runtime memory/disk usage instead.

## 4. Recommendations by installation media

These are starting points, not hard limits — always leave headroom (btrfs in
particular dislikes running near-full).

### USB flash drive / SD card (removable)

Removable media is the most space- and endurance-constrained target. Favor a
**minimal** install: no DE, no gaming stack, ext4 or xfs over btrfs+snapshots
(fewer writes, less metadata churn), and ZRAM or a small swap file instead of
a swap partition.

| Use case | Recommended capacity |
|----------|---------------------|
| Minimal/base system, no DE | 16–32 GB |
| With a lightweight DE (XFCE) | 32–64 GB |
| Full DE (KDE/GNOME) + extras | 64 GB+ |

> Avoid the gaming/handheld package set on removable media — the package
> footprint and write volume are a poor match for flash endurance.

### Internal SSD / NVMe

The most flexible and forgiving target — fast enough for btrfs subvolumes,
snapshots, LVM thin, encryption, and large package sets.

| Use case | Recommended capacity |
|----------|---------------------|
| Minimal/base system, no DE | 32–64 GB |
| Desktop (KDE/GNOME/XFCE), general use | 100–250 GB |
| Gaming/handheld stack (Steam, Wine, Decky, HHD, GPU drivers) | 250 GB minimum, 500 GB+ recommended |
| Encrypted + btrfs subvolumes + snapshots | add at least 20% headroom on top of the above |

This is the recommended target for the full Deploytix feature set
(encryption, LVM thin, gaming/handheld extras, SecureBoot).

### Internal HDD

HDDs offer the most capacity per dollar but the worst random-I/O performance.
Favor **larger, fewer partitions**, ext4/xfs over btrfs (less metadata
seeking), and avoid swap partitions in favor of ZRAM (keeps swap activity off
the slow disk entirely).

| Use case | Recommended capacity |
|----------|---------------------|
| Minimal/base system, no DE | 64 GB minimum (capacity is cheap; allocate generously) |
| Desktop, general use | 250 GB+ |
| Gaming/handheld stack | 1 TB+ (game libraries dominate; prefer a separate large `/home`) |

> If you have both an SSD and an HDD, the best pairing is typically: install
> Deploytix's system partitions (EFI/Boot/`/`/`/usr`/`/var`) on the SSD for
> snappy boots and package operations, and a large `/home` (via a
> [Custom layout](CUSTOM_PARTITION_LAYOUT.md) or LVM) on the HDD for bulk
> storage.

## 5. Checking your numbers before you commit

1. `deploytix list-disks --all` — confirm the target's `size_bytes` and
   `device_type`/`removable` flags.
2. `deploytix validate <config>` — catches `DiskTooSmall` and other
   cross-field issues before you touch the disk.
3. `deploytix install -n` (`--dry-run`) — preview the computed partition
   layout (sizes, mount points, encryption flags) without writing anything.
4. From the GUI's Review step, run a **Rehearsal** — it executes the full
   pipeline against the real device and then wipes it, so you can confirm the
   layout fits before doing a real install.

## See also

- [README → Configuration](../README.md#configuration) for the `[disk]`
  TOML schema and default partition table.
- [docs/CUSTOM_PARTITION_LAYOUT.md](CUSTOM_PARTITION_LAYOUT.md) for defining
  your own partition sizes and mount points.
- [README → Gaming & Handheld Features](../README.md#gaming--handheld-features)
  for the full list of optional packages that drive up space requirements.
