# grub-btrfs Feasibility Assessment

Source: https://github.com/Antynea/grub-btrfs

## Summary

Implementing grub-btrfs — **including full support for standalone GRUB with
SecureBoot and encryption** — is feasible. Effort varies by configuration:

| Configuration | Feasibility | Effort |
|---|---|---|
| Btrfs, no encryption | High — works almost out of the box | Low |
| Btrfs + multi-LUKS (standard GRUB) | High — one hook change required | Medium |
| Btrfs + LVM thin | Not applicable — LVM thin doesn't use btrfs subvolumes for data | N/A |
| Btrfs + SecureBoot standalone GRUB (sbctl + encryption) | High — existing rebuild/sign pipeline is the regen target, but needs a custom trigger (see below) | High |

All four supported combinations can ship together; none require disabling
another Deploytix feature.

---

## How grub-btrfs Works

grub-btrfs installs a hook at `/etc/grub.d/41_snapshots-btrfs` that is
automatically called by `grub-mkconfig`. It scans `/.snapshots/` (snapper
convention) for btrfs snapshots and injects separate GRUB boot menu entries
for each one, overriding `rootflags=subvol=<snapshot-path>` in the kernel
cmdline.

An optional daemon (`grub-btrfsd`) watches the snapshots directory with
inotify and re-runs `grub-mkconfig` whenever snapshots are created or deleted.

For encrypted systems it reads `GRUB_BTRFS_ENABLE_CRYPTODISK` from
`/etc/default/grub-btrfs/config` and appends the necessary crypto modules
to the generated entries.

---

## Current Codebase State

### What already exists

- **Btrfs subvolumes** — fully implemented: `@` (root), `@home`, `@usr`,
  `@var`, `@log`. The `@var` and `@log` subvolumes are on separate subvolumes
  from `@`, which is exactly the right layout for read-only snapshot booting
  (writable `/var` survives a rollback).
- **GRUB installation** — two paths: standard (`grub-install` + `grub-mkconfig`)
  and standalone (SecureBoot + encryption embeds grub.cfg inside the EFI binary).
- **Snapper package installation** — already in `PackagesConfig.install_btrfs_tools`
  (installs `snapper` + `btrfs-assistant` via AUR).
- **Mkinitcpio custom hooks** — `crypttab-unlock` + `mountcrypt` handle
  multi-LUKS unlock and mounting. The `mountcrypt` hook hardcodes subvolume
  paths at install time.

### What is missing

1. `grub-btrfs` package installation.
2. `/etc/default/grub-btrfs/config` generation.
3. `grub-btrfsd` service enablement (init-system-aware).
4. `mountcrypt` hook must read `rootflags` from the kernel cmdline for
   snapshot booting to work.
5. Snapper configuration for the `@` subvolume.

---

## Detailed Analysis by Configuration

### 1. Btrfs Without Encryption

This is the straightforward case.

- `grub-btrfs` is available in the Artix repos (`pacman -S grub-btrfs`).
- The existing `grub-mkconfig` call in `run_grub_install()` already invokes
  `/etc/grub.d/41_snapshots-btrfs` automatically once the package is installed.
- **No GRUB config changes are needed** beyond adding the package.
- The subvolume layout (`@var`, `@log` separate from `@`) already satisfies
  the read-only snapshot requirement.
- `grub-btrfsd` needs a runit/openrc service file; Artix ships
  `grub-btrfs-runit` in the AUR (or a service file can be generated).

**One real obstacle**: the `filesystems` hook (used for unencrypted btrfs)
mounts whatever subvolume is declared in fstab. When grub-btrfs boots a
snapshot, it changes `rootflags=subvol=@` in the cmdline to
`rootflags=subvol=@/.snapshots/N/snapshot`. The standard `btrfs` + `filesystems`
hooks read `rootflags` from the cmdline, so this works correctly with no
changes to Deploytix's hook generation.

**Net effort**: Add `grub-btrfs` to the base packages (alongside snapper),
generate the grub-btrfs config file, enable the daemon service.

---

### 2. Btrfs + Multi-LUKS Encryption (Standard GRUB)

This is the default encryption path in Deploytix (multi-LUKS, `crypttab-unlock`
+ `mountcrypt`).

#### The `mountcrypt` hook problem

The generated `mountcrypt` hook (in `src/configure/hooks.rs`) hardcodes the
btrfs subvolume at install time:

```bash
mount -t btrfs -o subvol=@,defaults,noatime,compress=zstd \
    /dev/mapper/Crypt-Root /new_root
```

When grub-btrfs boots a snapshot it overrides `rootflags` in the kernel
cmdline to `subvol=@/.snapshots/5/snapshot`. The standard `filesystems` hook
reads this; the custom `mountcrypt` hook does **not** — it ignores the cmdline
entirely and always mounts `@`.

**Fix required**: The `mountcrypt` hook generation
(`generate_mountcrypt_hook()` in `src/configure/hooks.rs`) must be extended to
parse `rootflags` from `/proc/cmdline` at boot time and use that value instead
of the hardcoded subvolume name when it is present. Draft logic:

```bash
# Read rootflags from cmdline (grub-btrfs sets this for snapshots)
ROOTFLAGS=$(grep -oP 'rootflags=\K\S+' /proc/cmdline || true)
SUBVOL=${ROOTFLAGS:-subvol=@,defaults,noatime,compress=zstd}

mount -t btrfs -o "$SUBVOL" /dev/mapper/Crypt-Root /new_root
```

This is a localised change in one function and doesn't affect the existing
hook architecture.

#### GRUB cryptodisk interaction

Deploytix only sets `GRUB_ENABLE_CRYPTODISK=y` when `/boot` itself is
encrypted (LUKS1 `boot_encryption`). grub-btrfs respects this: it reads
`GRUB_BTRFS_ENABLE_CRYPTODISK` from its own config file
(`/etc/default/grub-btrfs/config`) independently. Setting
`GRUB_BTRFS_ENABLE_CRYPTODISK=y` there causes grub-btrfs to embed the
`cryptodisk`, `luks`, and `luks2` modules into each snapshot entry.

Because Deploytix uses custom initramfs hooks (not the standard `encrypt`
hook), GRUB itself doesn't need to unlock LUKS for the data partitions — the
initramfs does that. The only case where GRUB must unlock something is when
`/boot` is encrypted. So:

- If `boot_encryption = false`: set `GRUB_BTRFS_ENABLE_CRYPTODISK=false` in
  the grub-btrfs config (GRUB reads `/boot` unencrypted, initramfs handles
  data LUKS).
- If `boot_encryption = true`: set `GRUB_BTRFS_ENABLE_CRYPTODISK=true` (GRUB
  must unlock the LUKS1 `/boot` container before reading any kernel or config).
  Upstream `41_snapshots-btrfs` lowercases this value and only takes the
  cryptodisk branch for the literal string `true` (the man page example uses
  `"true"` too) — `y`/`yes` fall through to the non-cryptodisk branch and
  silently omit the `cryptodisk`/`luks`/`luks2` modules from encrypted-`/boot`
  snapshot entries.

**Net effort**: Medium. One hook change + config file generation.

---

### 3. Btrfs + LVM Thin

LVM thin in Deploytix collapses all data partitions into thin LVs inside a
single LVM PV. The filesystem on those LVs can be btrfs, but in practice the
LVM thin path doesn't configure btrfs subvolumes — `layout.subvolumes` may be
`Some(...)` but the LVM thin code path in the installer operates differently.

grub-btrfs scans `/.snapshots/` on the **mounted root**, so it can work with
LVM-backed btrfs in principle. However, combining LVM thin + btrfs subvolumes
+ grub-btrfs is an unusual stack, and the snapper integration for LVM volumes
differs from the standard btrfs-native path.

**Recommendation**: Treat this combination as out of scope for the initial
grub-btrfs implementation. LVM thin is already a niche feature.

---

### 4. Btrfs + SecureBoot Standalone GRUB (Sbctl + Encryption)

**Revised verdict: feasible — via a unified rebuild-on-change pipeline.**
This requires more moving parts than the other configurations, but
Deploytix already contains nearly all of the machinery needed.

#### The problem

When `secureboot = true`, `secureboot_method = Sbctl`, and
`encryption = true`, Deploytix runs `grub-mkstandalone`, which embeds
`/boot/grub/grub.cfg` inside the signed EFI binary (`BOOTX64.EFI`) as a
memdisk (`run_grub_mkstandalone()`, `src/configure/bootloader.rs:286`).
grub-btrfs adds entries to `/boot/grub/grub.cfg` at runtime (via the
`/etc/grub.d/41_snapshots-btrfs` hook, invoked whenever `grub-mkconfig` runs,
either manually or by the `grub-btrfsd` daemon on snapshot create/delete).
A plain `grub-mkconfig` updates the on-disk `grub.cfg` but the **signed,
embedded** config inside `BOOTX64.EFI` stays stale — new snapshot entries
would never appear in the boot menu.

#### The fix: reuse the existing reinstall-grub pipeline as the regen target

Deploytix **already solves this exact class of problem** for kernel updates.
`create_grub_reinstall_script()` (`src/configure/bootloader.rs:458`) generates
`/usr/local/bin/reinstall-grub`, which — for the standalone case — performs
precisely the three steps required to make a new `grub.cfg` "stick":

```bash
grub-mkconfig -o /boot/grub/grub.cfg          # (1) regenerate config —
                                              #     this also runs 41_snapshots-btrfs,
                                              #     so snapshot entries land here
grub-mkstandalone --format=x86_64-efi \       # (2) rebuild the embedded-config binary
    --output=/boot/efi/EFI/BOOT/BOOTX64.EFI \
    --disable-shim-lock --modules="$MODULES" \
    "boot/grub/grub.cfg=/boot/grub/grub.cfg"
sbctl sign-all                                 # (3) re-sign for SecureBoot
```

Step (1) automatically incorporates grub-btrfs's snapshot entries (that's how
grub-btrfs always works — it's a `grub-mkconfig` plugin, not a separate
config file). Steps (2) and (3) are exactly what's needed to make those
entries reach the signed boot binary. **No new rebuild logic needs to be
written — only a second trigger path needs to call the existing script.**

#### Wiring grub-btrfs to the existing script

grub-btrfs's own config (`/etc/default/grub-btrfs/config`) exposes
`GRUB_BTRFS_MKCONFIG` — the command distros use to point at `grub-mkconfig`
vs. `grub2-mkconfig` vs. `update-grub`. However, `grub-btrfsd`'s
`create_grub_menu()` does **not** call it unconditionally; it first checks
whether `grub.cfg` already contains a `snapshots-btrfs` stanza:

```bash
if grep "snapshots-btrfs" "$GRUB_BTRFS_GRUB_DIRNAME/grub.cfg"; then
    /etc/grub.d/41_snapshots-btrfs            # stanza exists: regen menu only
else
    ${GRUB_BTRFS_MKCONFIG:-grub-mkconfig} -o "$GRUB_BTRFS_GRUB_DIRNAME/grub.cfg"
fi
```

So `GRUB_BTRFS_MKCONFIG` is only consulted on the **bootstrap run**, before
any `snapshots-btrfs` stanza exists. From the second snapshot event onward,
the daemon takes the first branch and runs `/etc/grub.d/41_snapshots-btrfs`
directly — which only rewrites the on-disk `/boot/grub/grub.cfg`. For the
standalone case that's precisely the file that does **not** matter at boot
(the signed binary's embedded copy does), so pointing `GRUB_BTRFS_MKCONFIG`
at `/usr/local/bin/reinstall-grub` would only cover the one-time bootstrap —
every subsequent snapshot create/delete would again drift the embedded config
out of sync, reproducing the original problem.

**Fix: bypass `grub-btrfsd`'s dispatch for standalone systems entirely.**
Rather than relying on the variable the daemon only reads once, Deploytix
should generate a small dedicated watcher (inotify on `/.snapshots`, the same
mechanism `grub-btrfsd` uses internally) that **unconditionally** invokes
`/usr/local/bin/reinstall-grub` on every snapshot create/delete:

- This sidesteps the stanza-detection shortcut completely — `reinstall-grub`
  always runs the full `grub-mkconfig → grub-mkstandalone → sbctl sign-all`
  cycle, so the *embedded* config is rebuilt and re-signed every time, not
  just the on-disk `grub.cfg`.
- `grub-mkconfig` (step 1 of `reinstall-grub`) still invokes
  `41_snapshots-btrfs` as a normal plugin, so snapshot entries are produced
  exactly as upstream intends — only the *trigger* path changes, not the
  entry-generation mechanism.
- The stock `grub-btrfsd` service should stay **disabled** on standalone
  systems: running it alongside the custom watcher would race two processes
  over `grub.cfg` and waste a rebuild+resign cycle on the
  `41_snapshots-btrfs`-only branch (which the standalone case can't use).
- `GRUB_BTRFS_MKCONFIG=/usr/local/bin/reinstall-grub` should still be set in
  `/etc/default/grub-btrfs/config` (it's harmless and covers the bootstrap
  run / any manual `grub-mkconfig` invocations that consult it), but it must
  not be treated as the mechanism that keeps the embedded config in sync —
  that job belongs entirely to the new watcher.

For non-standalone configurations (no SecureBoot, shim-based SecureBoot, or
unencrypted), none of this applies: stock `grub-btrfsd` +
`GRUB_BTRFS_MKCONFIG=/usr/bin/grub-mkconfig` works as designed, because the
on-disk `grub.cfg` *is* what GRUB reads at boot — no embedded copy to track.

#### Remaining work for this path specifically

1. **Custom watcher service**: write a small inotify-based watcher (the same
   technique `grub-btrfsd` uses, but without its stanza-detection shortcut)
   that watches `/.snapshots` and unconditionally calls
   `/usr/local/bin/reinstall-grub` on create/delete events. This is genuinely
   new code — there's no upstream component that performs an *unconditional*
   rebuild on every snapshot event — but it's a thin wrapper (inotify loop +
   one command), not a rebuild mechanism in its own right; the actual
   regenerate/rebuild/sign logic is entirely delegated to `reinstall-grub`.
2. **Watcher privileges**: like `grub-btrfsd`, the watcher must run as root
   (it needs to rebuild and re-sign the boot binary — the same trust boundary
   the pacman hook already operates in). Generate it as a runit (or
   target-init) service running as root, and ensure the stock `grub-btrfsd`
   service is *not* also enabled on standalone systems (see above — running
   both would race over `grub.cfg`).
3. **Debounce/batching**: automated snapshot schedules (e.g. snapper hourly
   timeline snapshots) can fire frequently, and rebuilding + re-signing costs
   several seconds each time. The watcher needs its own debounce (coalesce
   bursts of snapshot events into a single rebuild, e.g. only regenerate once
   per N seconds of inactivity) — it cannot reuse `GRUB_BTRFS_LIMIT` or the
   daemon's inotify debounce, since those only govern `grub-btrfsd`'s own
   (bypassed) dispatch path.
4. **Latency window**: there is a brief gap between "snapshot taken" and
   "rebuilt + signed binary written" during which a reboot would show the
   previous menu (missing the newest snapshot, but never an inconsistent or
   broken one — `reinstall-grub` always regenerates from current state).
   This is an inherent property of *any* embedded-config approach and is not
   a regression versus today's kernel-update handling, which has the same
   characteristic.
5. **`reinstall-grub` generalisation**: the script is currently generated
   per-install based on `use_standalone`/SecureBoot settings
   (`create_grub_reinstall_script`). No structural change is needed — it
   already does the right thing; the watcher just needs to shell out to it.

**Net effort**: High. Unlike the original "wire an existing trigger"
framing, the daemon's stanza-detection shortcut means `grub-btrfsd` cannot be
reused as-is for standalone systems — a small but genuinely new watcher
component must be written, packaged as a service, debounced, and kept from
conflicting with the stock daemon. The regenerate/rebuild/sign logic itself
remains fully delegated to the existing, already-tested `reinstall-grub`.

---

## Required Changes

### New config field

```toml
[packages]
install_grub_btrfs = false  # Enable grub-btrfs snapshot boot menu entries
```

Guards:
- Only valid when `disk.filesystem = "btrfs"`.
- No guard against standalone GRUB / SecureBoot+encryption is required —
  that combination is supported via the rebuild pipeline below.

### Package installation (`src/configure/packages.rs`)

Add `grub-btrfs` to a new `install_grub_btrfs()` function. The package is in
the official Artix/Arch repos — no AUR required (it ships
`/etc/grub.d/41_snapshots-btrfs`, `grub-btrfsd`, and the config template,
which is all that's needed regardless of standalone/standard GRUB).

For non-standalone systems, enable the `grub-btrfsd` daemon: the official
package ships systemd units; for Artix init systems, look for
`grub-btrfs-runit` (AUR) or generate a minimal runit service file that runs
the daemon as root.

For standalone systems, do **not** enable `grub-btrfsd` — generate and enable
the custom watcher service instead (see "Snapshot-watcher service" below).

### Config file generation (`src/configure/`)

New function `configure_grub_btrfs()` writing
`/etc/default/grub-btrfs/config`. The `GRUB_BTRFS_MKCONFIG` target depends on
whether standalone GRUB is active:

```ini
# Generated by Deploytix
GRUB_BTRFS_MKCONFIG_LIB=/usr/share/grub/grub-mkconfig_lib
GRUB_BTRFS_ENABLE_CRYPTODISK="<true if boot_encryption else false>"
GRUB_BTRFS_SUBVOLUMES_PATHS=("@" "@home" "@usr" "@var" "@log")
GRUB_BTRFS_IGNORE_SNAPSHOTS=()

# Standard GRUB:
GRUB_BTRFS_MKCONFIG=/usr/bin/grub-mkconfig

# Standalone GRUB (secureboot=true, secureboot_method=sbctl, encryption=true):
# GRUB_BTRFS_MKCONFIG=/usr/local/bin/reinstall-grub
```

Note `GRUB_BTRFS_MKCONFIG_LIB` must name the helper **file**
(`grub-mkconfig_lib`), not its containing directory — `41_snapshots-btrfs`
sources it directly (`. "$GRUB_BTRFS_MKCONFIG_LIB"`) and errors out before
generating any snapshot entries if the path doesn't resolve to a file. On
Arch/Artix the GRUB package installs it at
`/usr/share/grub/grub-mkconfig_lib`.

The `use_standalone` boolean is already computed in
`run_grub_install_with_secureboot()` / `create_grub_reinstall_hook()`
(`src/configure/bootloader.rs:258,408`) — `configure_grub_btrfs()` should
take the same boolean and select the `GRUB_BTRFS_MKCONFIG` value accordingly.

**Ordering dependency**: `reinstall-grub` must be generated *before*
`/etc/default/grub-btrfs/config` references it (i.e.
`create_grub_reinstall_hook()` must run before `configure_grub_btrfs()` in
the install pipeline).

### mountcrypt hook fix (`src/configure/hooks.rs`)

In the generated `mountcrypt` hook script, replace the hardcoded
`subvol=@,...` mount option for the root partition with a runtime parse of
`rootflags` from `/proc/cmdline`. This is a string-generation change inside
`generate_mountcrypt_hook()` — no structural changes to the Rust code.

### Snapper configuration (`src/configure/` or `src/install/`)

If `install_grub_btrfs = true`, also run `snapper -c root create-config /`
in chroot to initialise the snapper config for the root subvolume. This
creates `/.snapshots/` which grub-btrfs scans.

### Pacman hook update (`src/configure/bootloader.rs`)

No change needed beyond what's already planned: `95-grub-reinstall.hook`
already runs `reinstall-grub`, which (for standalone systems) performs
`grub-mkconfig` → `grub-mkstandalone` → `sbctl sign-all` — this regenerates
snapshot entries (via grub-btrfs's `41_snapshots-btrfs` plugin to
`grub-mkconfig`) as a side effect of the existing kernel-update flow. For
*non-standalone* systems with `install_grub_btrfs = true`, no hook change is
needed either — `grub-mkconfig` alone is sufficient and grub-btrfs's own
daemon handles snapshot-triggered regeneration.

### Snapshot-watcher service (new, standalone systems only)

This is the one genuinely new component: a small inotify watcher on
`/.snapshots` that unconditionally runs `/usr/local/bin/reinstall-grub` on
every snapshot create/delete event, generated and enabled in place of
`grub-btrfsd` whenever `use_standalone` is true (see "Wiring grub-btrfs to
the existing script" above for why `grub-btrfsd` itself can't be reused for
this case). It needs:

- root privileges (same trust boundary as the pacman reinstall hook),
- its own debounce so bursts of scheduled snapshots coalesce into one
  rebuild+resign cycle, and
- a service file for the target init system (runit, etc.), generated
  alongside the rest of the bootloader/service configuration.

For non-standalone systems, no new component is needed: enabling the stock
`grub-btrfsd` service (running as root, with sane debounce) is sufficient,
since `GRUB_BTRFS_MKCONFIG=/usr/bin/grub-mkconfig` and the on-disk
`grub.cfg` are exactly what GRUB reads at boot.

---

## Read-Only Snapshot Boot Compatibility

grub-btrfs warns that read-only snapshots "can be tricky". Deploytix's
subvolume layout handles this correctly:

- `/var` is on `@var` (separate subvolume).
- `/var/log` is on `@log` (separate subvolume).

When booting a **read-only** `@` snapshot, the running system writes logs to
`@log` and `/var/run` (tmpfs). The root `@` snapshot itself is never written.
This matches the snapper rollback model and requires no changes to the
subvolume layout.

The single-partition btrfs layout (`standard_subvolumes()`) already defines
all five subvolumes (`@`, `@home`, `@usr`, `@var`, `@log`), satisfying this
requirement.

---

## Implementation Order

1. Add `install_grub_btrfs` config field (no mutual-exclusion guard needed).
2. Add `grub-btrfs` to package installation (pacman, not AUR).
3. Fix `mountcrypt` hook to parse `rootflags` from cmdline — required for
   *any* encrypted multi-LUKS system to honour snapshot boot entries.
4. Add snapper root config creation in chroot (`snapper -c root create-config /`).
5. Generate `/etc/default/grub-btrfs/config`, selecting `GRUB_BTRFS_MKCONFIG`
   based on the existing `use_standalone` boolean:
   - standalone → `/usr/local/bin/reinstall-grub`
   - standard → `/usr/bin/grub-mkconfig`
   (Must run after `create_grub_reinstall_hook()` so the script exists when referenced.)
6. Branch on `use_standalone`:
   - **standard**: enable the stock `grub-btrfsd` service, running as root,
     with debounce timing tuned for the rebuild cost.
   - **standalone**: generate and enable the new snapshot-watcher service
     (running as root, debounced for the multi-second rebuild+resign cost)
     in place of `grub-btrfsd` — see "Snapshot-watcher service" above.
7. No changes needed to `95-grub-reinstall.hook` — it already performs the
   full regenerate → rebuild → sign cycle that snapshot entries ride along on.

This order keeps every step independently testable: 1-2 are inert until
combined with 3-7, and 3 (the mountcrypt fix) is the only change that affects
boot-critical code paths — it should be validated first, in isolation,
against a non-standalone encrypted+btrfs install before layering the
standalone-GRUB wiring (5-6) on top.
