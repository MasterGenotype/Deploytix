# Swap Implementation Audit

Scope: every `swap_type` Deploytix offers, across every disk layout it builds.
Two defects were found and fixed; three findings are recorded without a code
change.

---

## 0. What Deploytix offers

`SwapType` (`src/config/deployment.rs`):

| Value | Meaning |
|---|---|
| `partition` | A real swap partition in the computed layout |
| `file_zram` | ZRAM **plus** an on-disk swap file |
| `zram_only` | ZRAM only, no persistent swap |

Sizing: the swap *partition* is `2×RAM clamped 4–20 GiB`
(`disk::layouts::calculate_swap_mib`); the swap *file* is
`disk.swap_file_size_mib`, or `min(2×RAM, 16 GiB)` when unset; ZRAM is a fixed
4 GiB (see finding F3).

---

## 1. Are swap partitions formatted and brought up properly?

**Yes.** Verified end to end:

- **Formatted** — `disk::formatting` runs `mkswap` on any partition with
  `is_swap`, on every layout path (plain, multi-LUKS, LVM thin, LVM A/B).
- **In fstab** — every generator emits `UUID=… none swap defaults 0 0`:
  `generate_fstab`, `generate_fstab_multi_volume`, `generate_fstab_lvm_thin`,
  `generate_fstab_lvm_ab`. On encrypted layouts the entry names the *mapper*
  UUID, so it resolves after `crypttab-unlock` has opened the container.
- **Activated** — by `swapon -a` from the init system's normal boot sequence;
  Deploytix does not (and should not) `swapon` inside the installer.
- **Hibernation** — `resume_params()` resolves the swap partition's UUID and
  emits `resume=UUID=…` with no offset, which is correct for a partition.

No defects.

---

## 2. Are swap files actually created and used?

**Created: yes. Used: only on plain layouts — this was broken, now fixed.**

### D1 — swap file never entered fstab on encrypted, LVM-thin or A/B layouts *(fixed)*

`append_swap_file_entry()` had exactly one call site, inside the *else* branch
of the phase-3.5 layout selection:

```rust
} else {
    self.generate_fstab()?;
    if self.config.disk.swap_type == SwapType::FileZram {
        append_swap_file_entry(&self.config, INSTALL_ROOT)?;   // plain only
    }
}
```

Meanwhile phase 3.7 runs `configure_swap()` for *any* non-partition swap type,
on *every* layout — so an encrypted, LVM-thin or immutable A/B install with
`swap_type = "file_zram"`:

1. created `/swap/swapfile` (or `/var/swap/swapfile`),
2. allocated it at full size — up to 16 GiB,
3. ran `mkswap` on it,
4. and then never referenced it anywhere.

The result was a multi-gigabyte file that was never swapped on. ZRAM still came
up, so the system had swap and the symptom was easy to miss — the only visible
sign is `swapon --show` listing `/dev/zram0` alone, and disk usage that does not
add up.

With `hibernation = true` it was worse than wasted space: `resume_params()`
computes `resume=`/`resume_offset=` from that file and writes them into the
kernel cmdline, so the system was told to resume from a swap area that is never
activated. Hibernation could not work on any encrypted or LVM layout.

**Fix** (`src/install/installer.rs`): the call was hoisted out of the plain
branch to run after the layout selection, so it covers every layout, guarded by
the same `swap_type == FileZram` condition. The swap file is created for every
layout, so its fstab entry belongs to every layout.

### D2 — `fallocate` produces a swap file that XFS and F2FS reject *(fixed)*

`create_regular_swap_file()` used `fallocate` for everything that was not btrfs:

```rust
cmd.run("fallocate", &["-l", &format!("{}M", size_mib), path])
```

`swapon` refuses a file containing holes or unwritten extents — it maps blocks
with `bmap` rather than going through the filesystem. On the ext family a
fallocated file is fully mapped and works. On **XFS**, `fallocate` leaves
*unwritten* extents, and `swapon` fails with `skipping - it appears to have
holes`. **F2FS** is stricter still: a swap file must be contiguous and pinned.

So `filesystem = "xfs"` with `swap_type = "file_zram"` produced a swap file that
could never be activated — and, again, silently, because ZRAM covered for it.

**Fix** (`src/configure/system/swap.rs`): `check_is_btrfs()` was generalised to
`fs_type_of()`, and allocation now branches on the filesystem —
`fallocate` on ext only, `dd if=/dev/zero` everywhere else, including unknown
types. `dd` is slower because it writes the file out, which is precisely what
makes the extents real. btrfs keeps its own path (`btrfs filesystem mkswapfile`,
falling back to `chattr +C` + `fallocate`), which is correct there: NOCOW is the
requirement on btrfs, and mkswapfile handles it.

Note `stat -f -c %T` reports every ext filesystem as `ext2/ext3`, so that is the
string matched; `ext4` is accepted too for safety.

### Swap file placement on immutable roots

Already correct before this audit, and worth recording because it is subtle.
`swap_file_path()` returns `/var/swap/swapfile` when `immutable_root` is set,
rather than `/swap/swapfile`, because:

- `/` and `/usr` are mounted read-only, so `swapon` on a file inside the root
  fails outright;
- on the btrfs backend `@` is snapshotted, and a swap file's **physical
  extents** are exactly what `resume_offset=` names — a snapshot makes that
  offset meaningless;
- `/var` is writable and shared across sets and slots on both backends, so the
  file survives an update and keeps its offset.

---

## 3. Is anything wrong with how ZRAM is used?

Mechanically it is sound on all four init systems. `setup_zram` writes a service
that loads the module, sets `comp_algorithm` **before** `disksize` (required —
the algorithm cannot be changed once the device is sized), runs `mkswap`, and
activates with `swapon -p 100`. The priority is right: ZRAM must outrank disk
swap so the kernel fills compressed RAM first. Teardown (`swapoff` + `reset`) is
present in the runit `finish`, the OpenRC `stop`, and the s6/dinit equivalents.

Three findings, none fixed here — all are judgement calls or need hardware to
confirm, and none is silently wrong the way D1 and D2 were.

### F1 — ZRAM size is a fixed 4 GiB regardless of RAM

```rust
const ZRAM_SIZE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
```

Every other size in Deploytix scales with the host (swap partition `2×RAM`
clamped, swap file `min(2×RAM, 16 GiB)`); ZRAM alone does not. Consequences:

- **On a 2–4 GiB machine** a 4 GiB ZRAM device is over-provisioned. ZRAM pages
  still occupy RAM; a device sized at or above total RAM can be filled with
  poorly-compressible data until the machine OOMs, which is the failure ZRAM is
  meant to prevent.
- **On a 32–64 GiB machine** 4 GiB is small enough to be nearly irrelevant.

Recommendation: size it from RAM — `min(RAM, 8 GiB)` is a reasonable default
with `zstd` (the configured default algorithm), and expose it as
`disk.zram_size_mib` with `0` meaning auto, matching how `swap_file_size_mib`
already works. Left unchanged because it alters behaviour on every existing
config and the right multiplier is a policy choice.

### F2 — the ZRAM service does not fail loudly when the module is absent

The generated scripts run `modprobe zram num_devices=1` and continue regardless.
If `zram` is missing — a custom kernel built without `CONFIG_ZRAM`, which is not
hypothetical on this project's kernels — `/sys/block/zram0/` does not exist, the
`echo`s fail, `mkswap` fails, `swapon` fails, and the system boots with no swap
at all and nothing obvious in the log. Under runit the service then restarts in
a loop.

This is the same class of problem as the missing `dm-thin-pool` target that the
host-kernel preflight (`utils::deps::ensure_kernel_dm_targets`) now catches. A
matching check would be: verify the *target's* kernel provides `zram` after
basestrap, and have the service log a single clear error rather than failing
silently.

### F3 — `exec pause` is assumed to exist

The runit service ends with `exec pause` to hold the oneshot open. `pause` is
not part of runit upstream. It is used identically in the ISO's own shipped
overlay (`iso/profile/deploytix/root-overlay/etc/runit/sv/zram/run`), so it is
an established assumption in this project and presumably resolves on Artix —
**left unchanged deliberately**. If it does not resolve, the run script exits
immediately and `runsv` restarts it forever, re-running `mkswap` on a live swap
device each time. Worth confirming once on a real target; `exec sleep infinity`
is the portable alternative.

---

## 4. Result

| Item | Status |
|---|---|
| Swap partition: `mkswap` | ✅ correct on all layouts |
| Swap partition: fstab | ✅ correct on all layouts |
| Swap partition: hibernation `resume=` | ✅ correct |
| Swap file: created | ✅ correct on all layouts |
| Swap file: fstab | 🔧 **fixed** (D1 — was plain layouts only) |
| Swap file: allocation on XFS/F2FS | 🔧 **fixed** (D2 — was unswappable) |
| Swap file: allocation on ext/btrfs | ✅ correct |
| Swap file: immutable placement on `/var` | ✅ correct |
| Swap file: hibernation offset | ✅ correct — and only reachable now that D1 is fixed |
| ZRAM: service correctness, all four inits | ✅ correct |
| ZRAM: priority 100 over disk swap | ✅ correct |
| ZRAM: sizing | ⚠️ F1 — fixed 4 GiB, does not scale |
| ZRAM: missing-module behaviour | ⚠️ F2 — fails silently |
| ZRAM: `exec pause` | ⚠️ F3 — assumed, matches shipped ISO |
| `zram_only` + hibernation | ✅ rejected by validation |

Tests added: `only_ext_filesystems_may_use_fallocate_for_swap`,
`btrfs_is_not_handled_by_the_regular_allocator`. Full suite: 537 passing,
clippy clean under `-D warnings`.
