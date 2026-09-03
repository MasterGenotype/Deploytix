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
indifferent to the device-mapper hierarchy above it.

The rewrite is anchored on `2>/dev/null)` inside the command substitution, so
it matches both the 4.13 release (`var=$(...) # comment`) and current upstream
(`var="$(...)"`); a line that already guards its probe with `||` — newer
grub-btrfs guards `root_uuid`/`boot_uuid` itself — is left alone rather than
double-patched. Afterwards the script **verifies** that no unguarded
`grub_probe` assignment remains among the four, and exits non-zero if one does.
The idempotence marker `# DEPLOYTIX-INTEGRITY-PATCH-V1` is written *after* that
check passes, not before, so a partial application cannot mark itself done and
be skipped on the next run. `DEPLOYTIX_GRUB_BTRFS_ROOT` prefixes every path,
which is what lets the patch be exercised against a fixture tree in tests
(`tests/fixtures/grub-btrfs/`, one fixture per upstream variant).

The pacman hook re-applies the patch whenever the grub-btrfs package (re)writes
`41_snapshots-btrfs`; it is numbered `91-` so it runs before
`95-grub-reinstall.hook`, guaranteeing the patched generator is in place when
`grub-mkconfig` fires. With grub-btrfs absent, both files are inert: the hook
never triggers and the script exits 0.

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

---

## Fix 4 — grub.cfg is regenerated at install, so the submenu stub exists

**Code:** `regenerate_grub_cfg()` in `src/configure/grub_btrfs.rs`, called at
the end of installer Phase 5.45.

**Problem.** `41_snapshots-btrfs` does not write snapshot entries into
grub.cfg directly. It emits a small **stub** — a submenu that `configfile`s a
separately generated `grub-btrfs.cfg` — and grub-btrfsd later refreshes only
that generated list. A fresh install ran `grub-mkconfig` in the bootloader
phase, *before* grub-btrfs was installed, so the shipped grub.cfg had no stub
at all. What happened next depended on the daemon version: 4.13 always runs a
full `grub-mkconfig` and so recovers, while newer daemons only re-run the
generator in place once the stub's marker exists — and never produce a menu.

**Fix.** Phase 5.45 regenerates grub.cfg as its last step, with grub-btrfs
already installed and configured. The first snapshot the daemon ever sees then
lands in a config that is already wired for it, on every daemon version.
Standalone-GRUB systems run `reinstall-grub` (mkconfig → mkstandalone → sbctl
sign) rather than a bare `grub-mkconfig`, since the config that boots is inside
the signed EFI binary; if that script is somehow absent the step falls back to
`grub-mkconfig` and warns.

---

## Fix 5 — standalone GRUB: the snapshot list lives on the ESP

**Code:** `write_grub_btrfs_config()` and `esp_locator_script()` in
`src/configure/grub_btrfs.rs`.

**Problem.** On SecureBoot + encryption layouts Deploytix builds a *standalone*
GRUB image: grub.cfg is baked into the signed `BOOTX64.EFI` as a memdisk, and
`${prefix}` therefore resolves to that memdisk. grub-btrfs defaults to writing
`grub-btrfs.cfg` under `${prefix}` and to having the stub read it from there —
a path that can never contain a file written at runtime. The submenu appeared
in the menu and did nothing when selected.

**Fix.** The generated `/etc/default/grub-btrfs/config` redirects the list to
the EFI System Partition, which GRUB can always read with no `cryptomount`:

```bash
GRUB_BTRFS_GBTRFS_DIRNAME="/boot/efi/EFI/BOOT"
GRUB_BTRFS_GBTRFS_SEARCH_DIRNAME="(\$deploytix_esp)/EFI/BOOT"
```

The escaped `\$` survives bash's `source` of the config as a literal `$`, which
`41_snapshots-btrfs` copies verbatim into grub.cfg for **GRUB** to expand. The
variable is set by a generated grub.d script, `/etc/grub.d/40_deploytix-esp`
(numbered to run before `41_snapshots-btrfs`), which probes the ESP's
filesystem UUID at generation time — so a reformatted ESP is picked up by the
next regeneration rather than being baked in at install:

```
search --no-floppy --fs-uuid --set=deploytix_esp <uuid>
```

It is never fatal: with no UUID it emits `set deploytix_esp=memdisk`, the
stub's existence test fails cleanly, and the submenu is simply absent.

**Rollback.** Remove the two `GRUB_BTRFS_GBTRFS_*` lines from
`/etc/default/grub-btrfs/config` and delete `/etc/grub.d/40_deploytix-esp`.
Standard-GRUB installs get neither: `${prefix}` is the on-disk `/boot/grub`
there, so upstream's default is already right.

---

## Remaining caveats

- **Immutable roots need no overlay** — and must not have one. The
  `mountcrypt` overlay described below is scoped to snapper snapshots: the
  layout root `@` and every `@deploytix-sets/*` root boot as plain btrfs
  mounts. An overlayfs `/` would make `41_snapshots-btrfs` exit early with
  *"Root filesystem isn't btrfs"* and abort `grub-probe` outright, so on a
  transactional immutable install the snapshot menu depends on that scoping.
  See [IMMUTABLE_SYSTEM.md](IMMUTABLE_SYSTEM.md).
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
  `GRUB_BTRFS_MKCONFIG` at `reinstall-grub` on standalone-GRUB systems),
  `grub-btrfsd` service definitions for runit/OpenRC/s6/dinit, and a closing
  grub.cfg regeneration (Fix 4). See the
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
