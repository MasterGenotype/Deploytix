# Disk-backed `/tmp` on immutable systems

## Problem

Transactional immutable installs mount `/` as a **plain read-only btrfs
subvolume** (no overlay). Writable paths that normally live under `/` therefore
need explicit homes:

| Path | Historical deploytix choice |
|------|-----------------------------|
| `/root`, `/opt`, `/srv` | bind mounts from `@var` |
| `/tmp` | **tmpfs** via fstab |

The fstab line was:

```fstab
tmpfs  /tmp  tmpfs  rw,nosuid,nodev,mode=1777  0  0
```

With **no `size=`**, the kernel caps a tmpfs at **half of physical RAM**. On a
~21 GiB machine that is ~11 GiB. Large build trees (e.g. linux-tkg under
`/tmp/tkg-gui-<pid>`) hit `ENOSPC` long before disk free space is exhausted.

Non-immutable installs never received this line: `/tmp` is an ordinary directory
on the root filesystem (already disk-backed).

## Alternatives considered

| Option | Verdict | Why |
|--------|---------|-----|
| A. Keep tmpfs, raise `size=` | Rejected as primary | Still RAM/swap-backed; large builds starve memory |
| B. Keep tmpfs; document `TMPDIR=/var/tmp` | Rejected as only fix | Helps tools that honor `TMPDIR`; anything hardcoding `/tmp` still fails |
| C. Bind `/var/tmp` → `/tmp` | Rejected as default | On multi-volume layouts `/var` is often small and already holds the swapfile |
| D. `/tmp` under `@home` | Rejected | System-global sticky directory on the home volume is the wrong ownership/unlock model |
| E. Dedicated TMP LV/partition | Deferred | Clean isolation, needs layout/UI work |
| F. Hybrid small tmpfs + disk `TMPDIR` | Deferred | Split semantics confuse users |
| **G. btrfs `@tmp` on the root FS, mounted at `/tmp`** | **Adopted** | Disk capacity, early availability, same pattern as other immutable writable mounts |

## Adopted design

### Subvolume

- **Name:** `@tmp`
- **Filesystem:** root btrfs (`Crypt-Root` / install ROOT device)
- **Mount:** `/tmp`
- **Options:** `subvol=@tmp,rw,noatime,compress=zstd`
- **Mode:** `1777` (sticky, world-writable)
- **Not** part of the paired snapshot set `{@, @usr, @etc}`
- **Not** initramfs-owned (fstab mounts it after `switch_root`)

Capacity tracks **root volume free space**, not half of RAM. On a multi-volume
layout this intentionally avoids stuffing build scratch onto a small `Crypt-Var`
next to the swap file.

### Ephemeral policy

Disk `/tmp` would otherwise persist across reboots. Deploytix installs:

```text
# /etc/tmpfiles.d/deploytix-tmp.conf
D! /tmp 1777 root root 0
```

`etmpfiles` / `tmpfiles-setup` runs this at boot (`--create --remove --boot`) and
wipes contents while restoring mode `1777`, approximating classic volatile `/tmp`
without a RAM backend.

Stock `/usr/lib/tmpfiles.d/tmp.conf` (`q /tmp … 10d`) remains; the `/etc` drop-in
is the deploytix policy for immutable systems.

### fstab (immutable btrfs)

```fstab
# Writable paths for the read-only root…
UUID=<root-fs-uuid>  /tmp  btrfs  subvol=@tmp,rw,noatime,compress=zstd  0  0
/var/roothome  /root  none  bind  0  0
/var/opt  /opt  none  bind  0  0
/var/srv  /srv  none  bind  0  0
```

### Install-time contract

On `immutable_root` (btrfs backend):

1. Create `@tmp` on the root btrfs (idempotent).
2. Mount it at `<install_root>/tmp` with mode `1777`.
3. Emit the fstab line above (not tmpfs).
4. Install `/etc/tmpfiles.d/deploytix-tmp.conf`.

There is **no in-tree migration** of already-deployed systems. Operators who
need to convert a live host do it once by hand (create `@tmp`, edit fstab, add
the tmpfiles drop-in, reboot). That is an ops procedure, not a deploytix
subcommand.

### Code map

| Concern | Location |
|---------|----------|
| `@tmp` constant + create/mount + tmpfiles drop-in | `src/immutable/tmp.rs` |
| fstab writable-path block | `src/install/fstab.rs` (`immutable_writable_paths`) |
| Install hooks (next to `@etc`) | `src/install/installer.rs` |
| Layout docs | `docs/IMMUTABLE_SYSTEM.md` |

### Tests

- Generated immutable writable paths contain `subvol=@tmp` and do **not** use
  default half-RAM `tmpfs /tmp`.
- `sanitize_fstab` still leaves the `/tmp` line active.
- Create/mount helpers are dry-run safe.

## LVM A/B gap

`generate_fstab_lvm_ab` does not currently emit an explicit `/tmp` entry. A
dm-verity read-only root needs the same *kind* of fix (disk-backed scratch on a
shared data LV). That is a separate follow-up; this document’s install path is
the **btrfs transactional** backend.

## Operational notes

- Plan **root volume size** with large builds in mind if users compile kernels
  on-device (`linux-tkg` trees commonly need multi‑GB scratch).
- `/var/tmp` remains on `@var` for durable temp files (FHS); `/tmp` is boot-wiped.
- Tools that honor `TMPDIR` still work; with disk-backed `/tmp`, the default is
  already suitable for large workdirs.
- After a manual live conversion, verify:

```bash
findmnt -no TARGET,SOURCE,FSTYPE,OPTIONS /tmp
df -h /tmp
ls -ld /tmp
# expect btrfs subvol=@tmp, capacity ≈ root FS free, mode drwxrwxrwt
```

## Incident that drove this

Handheld immutable Artix host: `/tmp` tmpfs 11 GiB (50% of 21 GiB RAM) filled by
`/tmp/tkg-gui-<pid>` (~9–11 GiB kernel sources/objects) during a TKG GUI build,
while Crypt-Root still had ~67 GiB free and Crypt-Home ~740 GiB free.
