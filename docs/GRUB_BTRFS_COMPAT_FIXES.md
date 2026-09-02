# grub-btrfs Compatibility Fixes for Encrypted Layouts

Status: **implemented** (see per-fix code references below).

These fixes originate from a 2026-05-18 field incident on a Deploytix-deployed
Artix system (runit, multi-LUKS with dm-integrity, btrfs subvolumes,
unencrypted `/boot`) where the user had installed the `grub-btrfs` package for
snapshot boot menu entries. A failing `grub-probe` (Fix 2) caused every
kernel-update GRUB regeneration to abort silently — the machine was one reboot
away from an unbootable state — while a second gap made snapshot menu entries
boot the live system instead of the selected snapshot (Fix 1). Fix 3 covers a
related mis-targeting on encrypted-`/boot` layouts, which degrades snapshot
boot rather than breaking regeneration. The full root-cause analysis lives in
[GRUB_BTRFS_FEASIBILITY.md](GRUB_BTRFS_FEASIBILITY.md); this document records
what Deploytix now ships to close each gap, and how to undo it.

Fixes 2 and 3 are delivered as a patch script plus a pacman hook, applied on
encrypted btrfs layouts and dormant until grub-btrfs is installed — by
`packages.install_grub_btrfs` or by the user later. Fix 1 is unconditionally
part of the generated `mountcrypt` hook on subvolume layouts.

Fixes 4–6 close a second class of failure, found after the above shipped:
snapshots that exist, are listed by `41_snapshots-btrfs`, and still never
appear in the boot menu. They live in the grub-btrfs phase itself
(`src/configure/grub_btrfs.rs`) rather than in the patch script.

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
(marker `# DEPLOYTIX-INTEGRITY-PATCH-V1` after the shebang). The pacman hook
re-applies it whenever the grub-btrfs package (re)writes
`41_snapshots-btrfs`; it is numbered `91-` so it runs before
`95-grub-reinstall.hook`, guaranteeing the patched generator is in place when
`grub-mkconfig` fires. With grub-btrfs absent, both files are inert: the hook
never triggers and the script exits 0.

The rewrite anchors on the `2>/dev/null)` that closes each command
substitution and skips any assignment that already contains `||`, so it
applies to both the packaged 4.13 release (bare `var=$(...) # comment`) and
current upstream master (quoted `var="$(...)"`, where `root_uuid`/`boot_uuid`
already carry a blkid fallback but `boot_hs`/`boot_fs` do not). The
self-check fails only if an unguarded `grub_probe` assignment remains — not
because fewer than four lines were rewritten, which is the expected outcome
on newer upstream. `tests/fixtures/grub-btrfs/` holds verbatim copies of both
generators and the unit tests in `bootloader.rs` run the generated script
against them (`DEPLOYTIX_GRUB_BTRFS_ROOT=<dir>` prefixes every path the
script touches, which also lets it be applied to a mounted target).

**Rollback.** Delete both files; `pacman -S --overwrite '*' grub-btrfs`
restores the pristine generator.

---

## Fix 3 — `GRUB_BTRFS_ENABLE_CRYPTODISK` matched to the layout

**Code:** same script as Fix 2 (`ensure_cryptodisk_flag()` in the generated
`patch-grub-btrfs-integrity`).

**Problem.** With `GRUB_BTRFS_ENABLE_CRYPTODISK="true"` in
`/etc/default/grub-btrfs/config`, the generator picks the LUKS container GRUB
must unlock by grepping the cmdline for `cryptdevice=UUID=…:cryptdev` — the
format consumed by the stock `encrypt` mkinitcpio hook, which Deploytix's
custom hooks replace and whose cmdline token Deploytix never emits.

That grep is `|| true`-guarded upstream, so — unlike Fix 2 — it does **not**
abort the generator:

```bash
crypt_source="$(printf '%s %s\n' "$GRUB_CMDLINE_LINUX_DEFAULT" "$GRUB_CMDLINE_LINUX" \
                  | grep -o -P 'cryptdevice=\K[^:]+' || true)"
```

With no token the generator falls through to `cryptomount -a` instead of
`cryptomount -u <uuid>`, so GRUB tries to unlock **every** LUKS container it
can see — on a multi-LUKS layout that is a passphrase prompt for Boot, Root,
Usr, Var and Home before a snapshot entry boots. It is a boot-UX regression,
not a regeneration failure. The branch only exists for setups where GRUB must
`cryptomount` a disk before it can read a kernel, i.e. when `/boot` itself is
encrypted.

**Fix.** Two parts, both baked in at install time:

1. `ensure_cryptodisk_flag()` sets `GRUB_BTRFS_ENABLE_CRYPTODISK` from the
   layout's `boot_encryption`: `"false"` for unencrypted `/boot` (GRUB uses
   the plain `insmod btrfs` + `search --fs-uuid` path), `"true"` when `/boot`
   is a LUKS container. Marked `# DEPLOYTIX-CRYPTODISK-V1` on the managed
   line.
2. `ensure_crypt_source_fallback()` (encrypted `/boot` only) appends a default
   to the extraction so the correct container is targeted:

   ```bash
   crypt_source="${crypt_source:-UUID=<boot-luks-uuid>}" # DEPLOYTIX-CRYPTSOURCE-V1
   ```

   Upstream's next block turns a `UUID=…` source into `cryptomount -u`. The
   default is *appended* rather than replacing the extraction, so if upstream
   reworks that line the patch degrades to today's `cryptomount -a` behaviour
   instead of corrupting the script.

Both are applied once and skipped when their marker is present, so later
manual edits by the user are never overwritten.

**Rollback.** Edit the value in `/etc/default/grub-btrfs/config` (keep or
remove the marker — with the marker present the script leaves the line alone).
To drop the pinned container, delete the `DEPLOYTIX-CRYPTSOURCE-V1` line from
`/etc/grub.d/41_snapshots-btrfs`; leaving the marker-bearing line out but the
marker absent means the next package upgrade re-applies it.

> **grub-btrfs 4.13 (the packaged release) has no cryptodisk support at all** —
> neither `GRUB_BTRFS_ENABLE_CRYPTODISK` nor the `cryptdevice=` extraction
> exist in its generator, so this fix is dormant there (the script reports
> *"No cryptdevice extraction … skipping"*). With a standard `grub-install`
> and encrypted `/boot` that is harmless: GRUB has already `cryptomount`ed
> `/boot` to read grub.cfg, so snapshot entries' `search --fs-uuid` finds it.
> Only standalone GRUB with encrypted `/boot` is affected (each snapshot entry
> would need its own `cryptomount`, which 4.13 cannot emit); that is an
> upstream limitation until the packaged release catches up with master.

---

## Fix 4 — grub.cfg regenerated after grub-btrfs is installed

**Code:** `regenerate_grub_cfg()` in `src/configure/grub_btrfs.rs`
(installer Phase 5.45).

**Problem.** grub-btrfs is two pieces. `41_snapshots-btrfs` writes the
per-snapshot entries to a separate `grub-btrfs.cfg` and emits only a small
*stub* into grub.cfg — a submenu that `configfile`s that list if it exists.
The bootloader phase runs `grub-mkconfig` before the package is installed, so
the grub.cfg a fresh install boots from has no stub and no `snapshots-btrfs`
marker. Whatever grub-btrfsd later writes to `grub-btrfs.cfg` is unreachable
from the menu until something re-runs `grub-mkconfig`. The 4.13 daemon
happens to do that on every event (its marker check greps a literal
`{grub_directory}/grub.cfg` — an upstream typo — so it always takes the full
`grub-mkconfig` path); the current daemon only re-runs the generator in place
once the marker is present, and would never add it.

**Fix.** The grub-btrfs phase ends by regenerating grub.cfg inside the install
chroot: `grub-mkconfig -o /boot/grub/grub.cfg`, or the full `reinstall-grub`
pipeline on standalone-GRUB systems (the config is embedded in the signed EFI
binary, so a bare mkconfig would not reach it). The generator finds no
snapshots yet, but the stub and marker are in place from the first boot, on
every daemon version.

---

## Fix 5 — standalone GRUB: snapshot list on the ESP

**Code:** `write_grub_btrfs_config()` / `esp_locator_script()` in
`src/configure/grub_btrfs.rs`. Applies when `uses_standalone_grub()`
(SecureBoot via sbctl + disk encryption).

**Problem.** The stub grub-btrfs emits reads the list from
`${prefix}/grub-btrfs.cfg`. On a standalone image `${prefix}` is
`(memdisk)/boot/grub` — the config embedded in the signed binary at build
time. No runtime write can land there, so the submenu's existence test always
fails and the entries never show, even though `/boot/grub/grub-btrfs.cfg` is
being written correctly.

**Fix.** The snapshot list goes to the EFI System Partition instead
(`GRUB_BTRFS_GBTRFS_DIRNAME="/boot/efi/EFI/BOOT"`, next to `BOOTX64.EFI`),
which GRUB can always read without a `cryptomount` — so this also works with
an encrypted `/boot`. The stub is pointed at
`GRUB_BTRFS_GBTRFS_SEARCH_DIRNAME="($deploytix_esp)/EFI/BOOT"`, and a small
generator, `/etc/grub.d/40_deploytix-esp` (sorts before `41_snapshots-btrfs`),
emits `search --no-floppy --fs-uuid --set=deploytix_esp <ESP UUID>` into
grub.cfg. The UUID is probed at generation time rather than baked in at
install, so a reformatted ESP is picked up by the next regeneration; if it
cannot be determined the variable is pointed at the memdisk, so the stub
fails its existence test cleanly instead of erroring at the menu.

Both daemon paths now work: an in-place generator run refreshes the ESP file
the embedded stub reads, and a full run goes through `reinstall-grub` as
before. Nothing about the embedded config's trust model changes — the
standalone image already loads the kernel and initramfs from disk unverified
(`--disable-shim-lock`), and the GRUB shell is not password-protected.

**Rollback.** Remove the two `GRUB_BTRFS_GBTRFS_*` lines and
`/etc/grub.d/40_deploytix-esp`, then `reinstall-grub`.

---

## Fix 6 — immutable root: regenerate through a chroot

**Code:** `immutable_watcher_script()` / `daemon_command()` in
`src/configure/grub_btrfs.rs`; `regenerate_grub()` in
`src/immutable/boot.rs`; `deploytix regen-grub` in `src/main.rs`. Applies
when `immutable_root = true` on the btrfs backend.

**Problem.** On a transactional immutable install the live `/` is an
overlayfs over the read-only `@` (or snapshot-set) subvolume.
`41_snapshots-btrfs` begins with `btrfs filesystem df /` and exits — silently,
status 0 — with *"Root filesystem isn't btrfs"*; `grub-mkconfig` itself hits
`grub-probe: failed to get canonical path of 'overlay'`. The stock grub-btrfsd
therefore never produces an entry, and on its full-mkconfig path rewrites
grub.cfg from a root it cannot describe. `deploytix update`/`rollback` already
avoid this by running `grub-mkconfig` in a scratch chroot of the target set;
snapshots created by snapper in between were left out.

**Fix.** On immutable btrfs installs the `grub-btrfsd` service (same name,
same `/.snapshots` watch, all four init systems) execs
`/usr/local/bin/deploytix-grub-btrfsd` instead of `/usr/bin/grub-btrfsd`. The
watcher waits on `inotifywait` like the daemon, settles for five seconds, and
runs `deploytix regen-grub`, which reads the current boot pointer from
grub.cfg and re-runs the pointer activation for it — mount that set at
`/run/deploytix-grub`, `grub-mkconfig` inside, pointer unchanged. The same
command is layout-aware on mutable installs (`reinstall-grub` on encrypted
layouts, `grub-mkconfig` otherwise) and refuses on the LVM A/B backend, whose
slot pointer is kept by in-place edits a mkconfig would discard.

**Rollback.** Point the service back at `/usr/bin/grub-btrfsd` — and accept
that it will not produce entries on an overlay root.

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
3. Run `deploytix regen-grub` once (Fix 4) so grub.cfg carries the snapshot
   submenu stub. Snapshots that already exist appear immediately.
4. Standalone GRUB (Fix 5): add the two `GRUB_BTRFS_GBTRFS_*` lines to
   `/etc/default/grub-btrfs/config` and install `/etc/grub.d/40_deploytix-esp`
   (executable) from a fresh install, then `reinstall-grub`.
5. Immutable root (Fix 6): install `/usr/local/bin/deploytix-grub-btrfsd`
   from a fresh install and point the `grub-btrfsd` service at it. Until
   then, `deploytix regen-grub` after creating snapshots does the same job by
   hand.
