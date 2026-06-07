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
| Btrfs + SecureBoot standalone GRUB (sbctl + encryption) | High — reuses existing rebuild/sign pipeline as the regen target | Medium-High |

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
- If `boot_encryption = true`: set `GRUB_BTRFS_ENABLE_CRYPTODISK=y` (GRUB
  must unlock the LUKS1 `/boot` container before reading any kernel or config).

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
`GRUB_BTRFS_MKCONFIG`, the command it invokes to regenerate the boot config
whenever it detects a snapshot change (this is the same variable distros use
to point at `grub-mkconfig` vs. `grub2-mkconfig` vs. `update-grub`). For
standalone-GRUB systems, Deploytix should generate:

```ini
GRUB_BTRFS_MKCONFIG=/usr/local/bin/reinstall-grub
```

instead of the default `/usr/bin/grub-mkconfig`. With this in place:

- Manual `grub-btrfs` CLI runs trigger the full rebuild + re-sign.
- The `grub-btrfsd` daemon (inotify-watching `/.snapshots`) triggers the same
  pipeline automatically whenever a snapshot is created or deleted.
- Kernel/GRUB-package pacman updates continue to trigger it via the existing
  `95-grub-reinstall.hook` — same script, same code path, one source of truth.

For non-standalone configurations (no SecureBoot, or shim-based SecureBoot,
or unencrypted), `GRUB_BTRFS_MKCONFIG` stays at the plain `grub-mkconfig` —
no rebuild/re-sign is needed because the config isn't embedded.

#### Remaining work for this path specifically

1. **Daemon privileges**: `grub-btrfsd` must run as root (it already needs
   to write `/boot/grub/grub.cfg`; rebuilding + signing the standalone binary
   is the same trust boundary the pacman hook already operates in). Ensure
   the generated runit service runs the daemon as root.
2. **Debounce/batching**: automated snapshot schedules (e.g. snapper hourly
   timeline snapshots) can fire frequently. Rebuilding + re-signing costs
   several seconds each time. grub-btrfs supports tuning (`GRUB_BTRFS_LIMIT`
   and the daemon's inotify debounce) to coalesce bursts of snapshot events
   into a single rebuild — these should be set to sane defaults (e.g. only
   regenerate once per N seconds of inactivity) rather than left at upstream
   defaults tuned for the cheap bare-`grub-mkconfig` case.
3. **Latency window**: there is a brief gap between "snapshot taken" and
   "rebuilt + signed binary written" during which a reboot would show the
   previous menu (missing the newest snapshot, but never an inconsistent or
   broken one — `reinstall-grub` always regenerates from current state).
   This is an inherent property of *any* embedded-config approach and is not
   a regression versus today's kernel-update handling, which has the same
   characteristic.
4. **`reinstall-grub` generalisation**: the script is currently generated
   per-install based on `use_standalone`/SecureBoot settings
   (`create_grub_reinstall_script`). No structural change is needed — it
   already does the right thing; it just needs to additionally be referenced
   from `/etc/default/grub-btrfs/config`.

**Net effort**: Medium-high. No new rebuild mechanism to design — wire an
existing, already-tested script into a second trigger, tune daemon timing,
and ensure the daemon runs with the right privileges under the target init
system.

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
the official Artix/Arch repos — no AUR required.

For the `grub-btrfsd` daemon: the official package ships systemd units. For
Artix init systems, look for `grub-btrfs-runit` (AUR) or generate a minimal
runit service file that runs the daemon as root (required — see "Daemon
privileges" above).

### Config file generation (`src/configure/`)

New function `configure_grub_btrfs()` writing
`/etc/default/grub-btrfs/config`. The `GRUB_BTRFS_MKCONFIG` target depends on
whether standalone GRUB is active:

```ini
# Generated by Deploytix
GRUB_BTRFS_MKCONFIG_LIB=/usr/share/grub
GRUB_BTRFS_ENABLE_CRYPTODISK="<true if boot_encryption else false>"
GRUB_BTRFS_SUBVOLUMES_PATHS=("@" "@home" "@usr" "@var" "@log")
GRUB_BTRFS_IGNORE_SNAPSHOTS=()

# Standard GRUB:
GRUB_BTRFS_MKCONFIG=/usr/bin/grub-mkconfig

# Standalone GRUB (secureboot=true, secureboot_method=sbctl, encryption=true):
# GRUB_BTRFS_MKCONFIG=/usr/local/bin/reinstall-grub
```

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

### Daemon → rebuild wiring (new)

This is the one genuinely new piece of glue: ensuring
`/etc/default/grub-btrfs/config` points `GRUB_BTRFS_MKCONFIG` at
`/usr/local/bin/reinstall-grub` for standalone systems (see config file
generation above), and that the `grub-btrfsd` service runs as root with
sane debounce timing for the target init system.

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
6. Enable `grub-btrfsd` with the correct init service, running as root, with
   debounce timing tuned for the standalone-rebuild cost (several seconds
   per regeneration) when standalone GRUB is active.
7. No changes needed to `95-grub-reinstall.hook` — it already performs the
   full regenerate → rebuild → sign cycle that snapshot entries ride along on.

This order keeps every step independently testable: 1-2 are inert until
combined with 3-7, and 3 (the mountcrypt fix) is the only change that affects
boot-critical code paths — it should be validated first, in isolation,
against a non-standalone encrypted+btrfs install before layering the
standalone-GRUB wiring (5-6) on top.
