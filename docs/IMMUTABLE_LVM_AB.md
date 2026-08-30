# Transactional Immutable Root — LVM A/B (dm-verity)

Deploytix can deploy a **transactional immutable** system on the **LVM thin**
layout using an **A/B dual-slot** model with **dm-verity** read-only roots. It is
the LVM-native counterpart to the btrfs backend in
[`IMMUTABLE_SYSTEM.md`](IMMUTABLE_SYSTEM.md); pick whichever matches your disk
layout. When enabled:

- **Two root logical volumes (`root_a`, `root_b`) alternate.** The active slot is
  mounted **read-only** and **dm-verity** integrity-checked; any tampering with an
  on-disk block yields an I/O error instead of corrupt data.
- **`/usr` lives inside the root image** (a single verity-protected tree), so
  `/lib`, `/lib64`, `/bin`, `/sbin` — symlinks into `/usr` — are covered too.
- **Updates are transactional**: `deploytix update` builds the *inactive* slot and
  flips the boot pointer, taking effect on the next reboot. The running slot is
  never touched, so an interrupted or failed update is a no-op.
- **`deploytix rollback`** flips back to the other slot (its image + verity hash
  are intact) — fast and reversible.
- **`/etc` is a writable overlay** (lower = the read-only image `/etc`, upper on a
  persistent `etc_overlay` LV). **`/var`, `/home`** are persistent and shared
  across slots.

Enable it at install time with the wizard prompt *"Enable transactional immutable
root?"* on an LVM-thin config, or `immutable_root = true` with `use_lvm_thin =
true` in the config's `[disk]`/`[packages]`. dm-verity is implied. This backend is
mutually exclusive with the btrfs one (grub-btrfs is not used).

---

## Layout

Logical volumes on the thin pool (inside the LUKS container when encrypted):

| LV | Mount | State | In a slot |
|----|-------|-------|-----------|
| `root_a` | `/` (slot A) | read-only, dm-verity | ✅ |
| `root_b` | `/` (slot B) | read-only, dm-verity | ✅ |
| `hash_a` | — | dm-verity Merkle tree for A | ✅ |
| `hash_b` | — | dm-verity Merkle tree for B | ✅ |
| `var` | `/var` | read-write | ❌ (persistent, shared) |
| `home` | `/home` | read-write | ❌ (persistent, shared) |
| `etc_overlay` | `/etc` overlay upper | read-write | ❌ (persistent, shared) |

`/boot` and EFI are ordinary partitions, shared across slots.

### Slot pointer

The active slot and each slot's verity **root hash** are recorded in a small state
file on the shared `/boot`:

```
# /boot/deploytix-slots.conf
active=A
vg=<volume-group>
roothash_a=<hex>
roothash_b=<hex>
```

The default GRUB entry carries `deploytix.slot=<X> deploytix.roothash=<hashX> ro`
on its kernel cmdline. **Activation is a sed-rewrite of those two tokens in
`/boot/grub/grub.cfg`** — never `grub-mkconfig`, whose `grub-probe` cannot
canonicalize the dm-verity root device.

---

## Boot flow

1. GRUB's default entry sets `deploytix.slot=<X>` and `deploytix.roothash=<hashX>`.
2. The `verity-ab` initramfs hook (a custom mount handler):
   - opens the LUKS container (if encrypted) and activates the VG (`encrypt` +
     `lvm2` hooks);
   - reads `deploytix.slot`/`deploytix.roothash` from `/proc/cmdline`;
   - `veritysetup open`s the slot's `root_*` LV against its `hash_*` tree and the
     root hash, as `/dev/mapper/deploytix_root`;
   - mounts that device **read-only** as `/`;
   - layers a writable overlay over `/etc` (upper/work on the `etc_overlay` LV).
3. `/var`, `/home`, `/boot` and EFI are mounted post-`switch_root` by systemd from
   `/etc/fstab` (stable UUIDs), avoiding double-mount races with the handler.

---

## Updating: `deploytix update`

```
deploytix update                 # full pacman -Syu into the inactive slot
deploytix update vim git         # also install these
deploytix update ./pkg/*.pkg.tar.zst   # install local package files (pacman -U)
deploytix update --reboot        # reboot automatically once staged
deploytix -n update              # dry-run: print the plan, change nothing
```

What it does (active = A → target = B):

1. Mounts `root_b` read-write and `rsync`s the running root tree into it
   (`--delete`, excluding `/var`, `/home`, `/boot` and pseudo-filesystems).
2. `artix-chroot`s the slot and runs `pacman -Syu` (+ requested packages; local
   `.pkg.tar.zst` files go through `pacman -U`) and `mkinitcpio -P`.
3. Freezes the slot and `veritysetup format`s a fresh hash tree → new root hash.
4. Records the hash + `active=B` in the slot-state file and repoints the boot
   pointer. **Reboot to activate.**
5. On any failure, the half-built slot is abandoned and the running slot + boot
   pointer are left untouched.

> **A second update in the same session extends the staged slot.** Once B is
> staged, `active=B` while the session still runs A — so "the inactive slot"
> would be A, the running root, and rebuilding it would rsync over the live
> system and discard everything B holds. A forgotten package therefore goes into
> B, which stays the boot target; step 1's rsync is skipped, since B already
> carries the running tree plus what the earlier update installed. The running
> slot is read from `deploytix.slot=` on `/proc/cmdline` — the same token the
> `verity-ab` hook resolves — because the state file's `active` names the *next*
> boot, not the current one. A failed update leaves the staged slot as the
> earlier update left it, still selected for the next boot.

---

## Rolling back: `deploytix rollback`

```
deploytix rollback --list        # show both slots, hashes, and the active marker
deploytix rollback               # flip to the other slot
deploytix rollback A             # activate a specific slot
deploytix rollback --reboot      # activate immediately
```

Rollback only moves the boot pointer (both slot images + hashes remain), so it is
itself reversible — flip "forward" again with another rollback.

---

## Direct-pacman prevention

Enforcement is the **read-only, verity-checked `/usr`** itself: a direct
`pacman -Syu` cannot modify it, so it fails. Installs also drop the shared
`/etc/profile.d/deploytix-immutable.sh` nudge (same as the btrfs backend) that
intercepts *interactive* `pacman` upgrade/install/remove and points you at
`deploytix update`. `command pacman …` bypasses it.

---

## Caveats & limitations

- **Shared `/boot`, per-slot kernel archives.** The slot images exclude `/boot`,
  so the kernel is shared. Each slot keeps a copy of the images it was built
  with under `/boot/deploytix/<slot>/`, and `activate_slot` restores that copy
  over the canonical names before the pointer starts selecting it — so a
  rollback boots the kernel its modules match, and a failed update restores the
  running slot's kernel instead of leaving the new one behind. Both slots are
  archived at install, since they start as identical clones. See
  `src/immutable/bootset.rs`.
- **Shared package DB.** `/var/lib/pacman` is on the shared `/var`, so a rollback
  restores the slot's `/usr` files but not the package database — the DB reflects
  the newest update. (Same trade-off as the btrfs backend.)
- **`/etc` overlay is shared, not per-slot.** Runtime `/etc` edits live in the
  persistent `etc_overlay` upper and apply to whichever slot is active; they are
  not versioned with a slot.
- **Root-hash trust.** The active slot's root hash is passed on the (GRUB) kernel
  cmdline. For a full chain of trust, combine with SecureBoot / a signed cmdline;
  by itself dm-verity guarantees integrity of the *image* against the pinned hash,
  not the provenance of the hash.
- **No automatic boot-count fallback.** A bad activated slot is escaped manually:
  edit `deploytix.slot`/`deploytix.roothash` at the GRUB prompt, or boot the good
  slot and run `deploytix rollback`.
- **Storage.** Two full root images (thin-provisioned, so physical use tracks
  actual data).

---

## Implementation map

| Concern | Location |
|---------|----------|
| Config flag + backend selection (`immutable_lvm_ab`) | `src/config/deployment.rs` |
| A/B volume layout | `src/disk/lvm.rs` (`immutable_ab_thin_volumes`, `ab`) |
| dm-verity helpers | `src/configure/verity.rs` |
| `verity-ab` initramfs hook + MODULES/HOOKS/BINARIES | `src/configure/hooks.rs`, `src/configure/mkinitcpio.rs` |
| Read-only fstab (`/`+`/etc` via handler) | `src/install/fstab.rs` (`generate_fstab_lvm_ab`) |
| Two-slot GRUB cmdline | `src/configure/bootloader.rs` (`configure_grub_defaults_lvm_ab`) |
| Install-time slot build + verity sealing | `src/install/installer.rs` (`*_lvm_ab_*`, `finalize_immutable_ab`) |
| `update`/`rollback` engine + boot pointer | `src/immutable/lvm_ab.rs` |
| CLI dispatch (btrfs vs LVM A/B) | `src/main.rs` |
