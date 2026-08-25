# grub-btrfs Compatibility Fixes for Encrypted Layouts

Status: **implemented** (see per-fix code references below).

These fixes originate from a 2026-05-18 field incident on a Deploytix-deployed
Artix system (runit, multi-LUKS with dm-integrity, btrfs subvolumes,
unencrypted `/boot`) where the user had installed the `grub-btrfs` package for
snapshot boot menu entries. Two independent failures caused every
kernel-update GRUB regeneration to abort silently — the machine was one reboot
away from an unbootable state — and a third gap made snapshot menu entries
boot the live system instead of the selected snapshot. The full root-cause
analysis lives in [GRUB_BTRFS_FEASIBILITY.md](GRUB_BTRFS_FEASIBILITY.md); this
document records what Deploytix now ships to close each gap, and how to undo
it.

Deploytix does **not** install grub-btrfs. Fixes 2 and 3 are delivered as a
dormant patch script plus a pacman hook that only fire if/when the user
installs grub-btrfs; Fix 1 is unconditionally part of the generated
`mountcrypt` hook on subvolume layouts.

---

## Fix 1 — `mountcrypt` honours `rootflags=subvol=` from the kernel cmdline

**Code:** `generate_mountcrypt_hook()` in `src/configure/hooks.rs`
(`resolve_root_subvol()` in the generated hook).

**Problem.** The generated `mountcrypt` initramfs hook replaces mkinitcpio's
default cmdline-driven root mount with its own `mount_handler`, which mounted
root with a hardcoded `subvol=@`. grub-btrfs snapshot menu entries work by
appending `rootflags=...,subvol="@snapshots/<n>/snapshot"` to the kernel
cmdline; the kernel's own root-mount path would honour that, but it never runs
on multi-LUKS layouts — mountcrypt replaces it. Selecting a snapshot entry
therefore booted the live `@` subvolume: correct kernel, correct initramfs, no
error, wrong filesystem state.

**Fix.** On layouts with btrfs subvolumes, the generated hook now defines
`resolve_root_subvol()`, which parses `/proc/cmdline` for
`rootflags=subvol=<path>`:

- last occurrence wins, matching kernel behaviour for repeated parameters
  (grub-btrfs appends its `rootflags` after the one from
  `GRUB_CMDLINE_LINUX_DEFAULT`);
- surrounding double-quotes are stripped (grub-btrfs emits them);
- falls back to the layout's root subvolume (`@`) when absent.

The root mount uses the resolved value; `/usr`, `/var`, `/home` stay pinned to
their layout subvolumes because they live on separate `Crypt-*` LUKS
containers that a root snapshot does not cover. A failed root mount with a
non-default subvolume prints a hint about read-only snapper snapshots
(`snapper -c root modify <n> --read-write` / `btrfs property set <path> ro
false`).

Normal boots are unaffected: the default entry carries `rootflags=subvol=@`,
which resolves to the same value previously hardcoded. Layouts without
subvolumes get neither the function nor the parsing.

**Rollback.** Regenerate the initramfs from a hook without the function, or
pin the mount line back to `subvol=@` in
`/usr/lib/initcpio/hooks/mountcrypt` and run `mkinitcpio -P`.

---

## Fix 2 — `grub-probe` → `blkid` fallback in `41_snapshots-btrfs`

**Code:** `create_grub_btrfs_compat()` in `src/configure/bootloader.rs`.
Installed on encrypted btrfs layouts as:

- `/usr/local/bin/patch-grub-btrfs-integrity`
- `/etc/pacman.d/hooks/91-patch-grub-btrfs.hook`

**Problem.** grub-btrfs's generator `/etc/grub.d/41_snapshots-btrfs` runs
under `set -e` and assigns four variables via
`$(grub-probe --device … --target=…)`. `grub-probe` cannot walk device-mapper
stacks containing a dm-integrity layer (`<name>_dif`, present on all Deploytix
`integrity = true` layouts) and can fail on plain LUKS2 as well. The failing
command substitution kills the generator, which kills `grub-mkconfig`, which
means the `95-grub-reinstall.hook` kernel-update pipeline silently stops
updating `grub.cfg` — the boot menu keeps referencing a kernel that no longer
exists.

**Fix.** The patch script rewrites the four assignments (`root_uuid`,
`boot_uuid`, `boot_hs`, `boot_fs`) into fallback chains:

```bash
root_uuid=$(${grub_probe} … 2>/dev/null || blkid -s UUID -o value "${root_device}" 2>/dev/null || true)
```

`blkid` reads the filesystem superblock through the mapper device and is
indifferent to the device-mapper hierarchy above it. The patch is idempotent
(marker `# DEPLOYTIX-INTEGRITY-PATCH-V1` after the shebang) and self-verifies
that all four lines landed. The pacman hook re-applies it whenever the
grub-btrfs package (re)writes `41_snapshots-btrfs`; it is numbered `91-` so it
runs before `95-grub-reinstall.hook`, guaranteeing the patched generator is in
place when `grub-mkconfig` fires. With grub-btrfs absent, both files are
inert: the hook never triggers and the script exits 0.

**Rollback.** Delete both files; `pacman -S --overwrite '*' grub-btrfs`
restores the pristine generator.

---

## Fix 3 — `GRUB_BTRFS_ENABLE_CRYPTODISK` matched to the layout

**Code:** same script as Fix 2 (`ensure_cryptodisk_flag()` in the generated
`patch-grub-btrfs-integrity`).

**Problem.** With `GRUB_BTRFS_ENABLE_CRYPTODISK="true"` in
`/etc/default/grub-btrfs/config`, the generator extracts a LUKS UUID by
grepping the cmdline for `cryptdevice=UUID=…:cryptdev` — the format consumed
by the stock `encrypt` mkinitcpio hook, which Deploytix's custom hooks replace
and whose cmdline token Deploytix never emits. The grep exits 1 and `set -e`
aborts the generator (same blast radius as Fix 2). That branch only exists for
setups where GRUB must `cryptomount` a disk before it can read a kernel, i.e.
when `/boot` itself is encrypted.

**Fix.** The patch script sets `GRUB_BTRFS_ENABLE_CRYPTODISK` from the
layout's `boot_encryption` value baked in at install time: `"false"` for
unencrypted `/boot` (GRUB uses the plain `insmod btrfs` + `search --fs-uuid`
path), `"true"` when `/boot` is a LUKS1 container. Applied once, marked with
`# DEPLOYTIX-CRYPTODISK-V1` on the managed line; later manual edits by the
user are never overwritten because the marker check runs first.

**Rollback.** Edit the value in `/etc/default/grub-btrfs/config` (keep or
remove the marker — with the marker present the script leaves the line alone).

---

## Remaining caveats

- **Read-only snapshots** — *fixed when `install_grub_btrfs = true`.* snapper
  snapshots are RO by default. On unencrypted layouts the stock
  `grub-btrfs-overlayfs` latehook is added to `HOOKS` (the package is
  installed, so mkinitcpio finds it). On multi-LUKS layouts the stock
  latehook is **not** usable, contrary to this document's earlier analysis:
  by latehook time mountcrypt has already mounted `/usr`, `/var` and `/home`
  inside `/new_root`, and the latehook's `mount --move` would carry them into
  the overlay's lowerdir where overlayfs hides submounts — `/usr` would
  appear empty and boot would fail. Instead the generated mountcrypt hook
  probes the mounted root for writability and, when a non-default subvolume
  is read-only, layers a tmpfs-backed overlayfs over it *before* the other
  volume mounts. Without `install_grub_btrfs`, behaviour is unchanged
  (mountcrypt mounts rw and prints the manual-remedy hint on failure).
- **Partial rollback semantics.** Only `/` is snapshotted; `/usr`, `/var`,
  `/home` remain live volumes on separate LUKS containers (separate
  subvolumes on the single-partition layout). Booting a snapshot shows the
  snapshot's root with the live everything-else. This is by design.
- **Full grub-btrfs integration** — *implemented* behind
  `packages.install_grub_btrfs` (installer Phase 5.45,
  `src/configure/grub_btrfs.rs`): package installation from the official
  repos, snapper root config with a top-level `@snapshots` subvolume,
  `/etc/default/grub-btrfs/config` generation (pointing
  `GRUB_BTRFS_MKCONFIG` at `reinstall-grub` on standalone-GRUB systems), and
  `grub-btrfsd` service definitions for runit/OpenRC/s6/dinit. See the
  implementation order in
  [GRUB_BTRFS_FEASIBILITY.md](GRUB_BTRFS_FEASIBILITY.md).

## Applying to already-deployed systems

Systems installed before these fixes can adopt them manually:

1. Regenerate or hand-edit `/usr/lib/initcpio/hooks/mountcrypt` to add
   `resolve_root_subvol()` and the `subvol=${root_subvol}` root mount, then
   run `mkinitcpio -P`.
2. Copy `/usr/local/bin/patch-grub-btrfs-integrity` and
   `/etc/pacman.d/hooks/91-patch-grub-btrfs.hook` from a fresh install (or
   generate them with `deploytix -n` dry-run inspection) and run the script
   once.
