# grub-btrfs Feasibility Assessment

Source: https://github.com/Antynea/grub-btrfs

## Summary

Implementing grub-btrfs is **feasible with qualifications**. The verdict varies
sharply by configuration:

| Configuration | Feasibility | Effort |
|---|---|---|
| Btrfs, no encryption | High — works almost out of the box | Low |
| Btrfs + multi-LUKS (standard GRUB) | High — one hook change required | Medium |
| Btrfs + LVM thin | Not applicable — LVM thin doesn't use btrfs subvolumes for data | N/A |
| Btrfs + SecureBoot standalone GRUB | Low — structural incompatibility | High |

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

This is a **structural incompatibility**.

When `secureboot = true`, `secureboot_method = Sbctl`, and
`encryption = true`, Deploytix runs `grub-mkstandalone`. This command embeds
`boot/grub/grub.cfg` inside the EFI binary (`BOOTX64.EFI`) as a memdisk. The
binary is then signed with sbctl.

grub-btrfs works by writing snapshot entries to `/boot/grub/grub.cfg` at
runtime (when `grub-mkconfig` runs or `grub-btrfsd` detects a new snapshot).
Because the standalone binary has the config baked in, it will never see
dynamically generated snapshot entries.

Possible mitigations — all complex:

1. **Rebuild the standalone binary on every snapshot**: Requires running
   `grub-mkstandalone` + `sbctl sign-all` with root privileges every time
   `grub-btrfsd` fires. This is slow (~5-10s) and would need a custom wrapper
   replacing the default `grub-btrfsd` behaviour.

2. **Switch to two-file GRUB for SecureBoot**: Use standard `grub-install`
   (not standalone) and sign the kernel + grub separately with sbctl. This
   eliminates the embedded-config problem but changes the SecureBoot
   architecture.

3. **Disable grub-btrfs when standalone GRUB is active**: Simple guard in
   Deploytix — mutually exclusive with standalone GRUB.

**Recommendation**: Option 3 for now. Emit a warning when the user enables
`install_btrfs_tools = true` (or a future `grub_btrfs = true`) together with
the standalone GRUB combination.

---

## Required Changes

### New config field

```toml
[packages]
install_grub_btrfs = false  # Enable grub-btrfs snapshot boot menu entries
```

Guards:
- Only valid when `disk.filesystem = "btrfs"`.
- Warn and skip when `system.secureboot = true` and `system.secureboot_method = "sbctl"` and `disk.encryption = true`.

### Package installation (`src/configure/packages.rs`)

Add `grub-btrfs` to a new `install_grub_btrfs()` function. The package is in
the official Artix/Arch repos — no AUR required.

For the `grub-btrfsd` daemon: the official package ships systemd units. For
Artix init systems, look for `grub-btrfs-runit` (AUR) or generate a minimal
runit service file.

### Config file generation (`src/configure/`)

New function `configure_grub_btrfs()` writing
`/etc/default/grub-btrfs/config`:

```ini
# Generated by Deploytix
GRUB_BTRFS_MKCONFIG=/usr/bin/grub-mkconfig
GRUB_BTRFS_MKCONFIG_LIB=/usr/share/grub
GRUB_BTRFS_ENABLE_CRYPTODISK="<true if boot_encryption else false>"
GRUB_BTRFS_SUBVOLUMES_PATHS=("@" "@home" "@usr" "@var" "@log")
GRUB_BTRFS_IGNORE_SNAPSHOTS=()
```

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

The existing `95-grub-reinstall.hook` runs after kernel updates. When
`install_grub_btrfs = true` (and not standalone), add a call to
`grub-btrfs update` (or re-run `grub-mkconfig`) so snapshot entries are
refreshed alongside the kernel entry.

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

1. Add `install_grub_btrfs` config field and validation guards.
2. Add `grub-btrfs` to package installation (pacman, not AUR).
3. Generate `/etc/default/grub-btrfs/config`.
4. Fix `mountcrypt` hook to parse `rootflags` from cmdline.
5. Add snapper root config creation in chroot.
6. Enable `grub-btrfsd` with the correct init service.
7. Update the `95-grub-reinstall` pacman hook to also refresh snapshot entries.
