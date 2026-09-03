# Transactional Immutable Root

Deploytix can deploy (and convert to) a **transactional immutable** system in the
style of openSUSE MicroOS / Aeon, adapted to Artix + pacman. When enabled:

- **`/` and `/usr` are mounted read-only on every boot.** `/lib`, `/lib64`,
  `/bin`, `/sbin` are symlinks into `/usr`, so they are covered for free.
- **`/etc` is a dedicated writable `@etc` subvolume**, snapshotted together with
  `@` and `@usr` so configuration rolls back with the system.
- **Snapshots are atomic paired sets `{@, @usr, @etc}`** that roll back together —
  no more "old root + current /usr" skew.
- **Updates are transactional**: `deploytix update` builds a new writable snapshot
  set, upgrades inside it, and activates it on the next reboot. Direct
  `pacman -Syu` on the live read-only system is refused.
- Writable state — `/var`, `/home` — is persistent and shared across sets (not
  rolled back). `/tmp` is a tmpfs; `/root`, `/opt`, `/srv` are bind mounts out of
  `@var`, so they are writable and persistent like the rest of `/var`.

Enable it at install time with the wizard prompt *"Enable transactional immutable
root?"* or `immutable_root = true` in the config's `[packages]` (requires
`install_grub_btrfs = true`).

---

## Layout

Subvolume roles on the root btrfs (`Crypt-Root`), with `@usr` on its own
`Crypt-Usr` container in multi-volume encrypted layouts (or on the root fs in
single-partition layouts):

| Subvolume | Mount | State | In snapshot set |
|-----------|-------|-------|-----------------|
| `@` | `/` | read-only | ✅ |
| `@usr` | `/usr` | read-only | ✅ |
| `@etc` | `/etc` | read-write | ✅ |
| `@var`, `@log` | `/var`, `/var/log` | read-write | ❌ (persistent) |
| `@home` | `/home` | read-write | ❌ (persistent) |
| `@snapshots` | `/.snapshots` | snapper | — |
| `@overlay` | snapshot-boot scratch | ephemeral | — |
| `@deploytix-sets/<id>/{root,usr,etc}` | — | snapshot sets | — |

### Snapshot sets

A **set** captures the three OS subvolumes under a shared id (seconds since the
Unix epoch) in a top-level `@deploytix-sets` directory on each filesystem:

```
Crypt-Root:  @deploytix-sets/<id>/root   (snapshot of @)
             @deploytix-sets/<id>/etc    (snapshot of @etc)
Crypt-Usr:   @deploytix-sets/<id>/usr    (snapshot of @usr)
```

Each set's `root` snapshot carries a **`.deploytix-pair`** marker naming its
`usr`/`etc` subvolumes:

```
usr=@deploytix-sets/<id>/usr
etc=@deploytix-sets/<id>/etc
```

The live system's `@` carries `usr=@usr` / `etc=@etc`.

---

## Boot flow

1. GRUB's default entry sets `rootflags=subvol=<pointer>` (the **boot pointer**),
   where `<pointer>` is `@` for the live system or `@deploytix-sets/<id>/root`
   for an activated set. This is stored in `GRUB_CMDLINE_LINUX_DEFAULT` in
   `/etc/default/grub`.
2. The initramfs `mountcrypt` hook:
   - mounts the pointer subvolume **read-only** as `/` — a plain btrfs mount, no
     overlay (see below);
   - reads `/.deploytix-pair` and mounts the paired `@usr` **read-only** at
     `/usr` and `@etc` **read-write** at `/etc`;
   - mounts `@var`, `@log`, `@home` read-write.

Because the pointer + marker drive everything, switching systems is just a
pointer move + a grub regeneration.

> **No overlay on the immutable root.** The `mountcrypt` hook layers an
> ephemeral `@overlay` over `/` only when the booted subvolume is a *snapper*
> snapshot — those are read-only by design and cannot be booted otherwise. The
> layout root `@` and every `@deploytix-sets/*` root are excluded, so on a
> normal immutable boot `/` is a real btrfs mount. That matters far beyond
> tidiness: an overlayfs `/` makes `grub-probe` abort with *"failed to get
> canonical path of `overlay'"*, makes grub-btrfs's `41_snapshots-btrfs` bail
> with *"Root filesystem isn't btrfs"* (so no snapshot entries are ever
> generated), and makes `findmnt -no FSROOT /` report `/` instead of the running
> subvolume. Keeping `/` a real btrfs mount is what lets the stock `grub-btrfsd`
> and `grub-mkconfig` work unaided.
>
> The writable paths the overlay used to provide are given directly instead: a
> tmpfs `/tmp`, and `/root`, `/opt`, `/srv` as bind mounts of `/var/roothome`,
> `/var/opt`, `/var/srv` (bind mounts rather than symlinks — the `filesystem`
> package owns those three as directories and a symlink would conflict on
> update). See `immutable_writable_paths()` in `src/install/fstab.rs`.

> **Regenerating grub for a target set.** `update` and `rollback` still mount
> the target `{root,usr,etc}` set at a scratch chroot and run the regeneration
> from inside it, rather than against the live `/`. Not because the live root
> cannot be probed any more — it can — but because grub's `10_linux` prepends
> `rootflags=subvol=<subvol of />` for a btrfs root, so a live run would emit
> both the running and the target subvolume on the kernel line and rely on
> last-wins ordering. The chroot is unambiguous. On standalone-GRUB systems the
> regeneration runs `reinstall-grub` (mkconfig → mkstandalone → sbctl sign)
> rather than a bare `grub-mkconfig`, because the config that actually boots is
> embedded in the signed EFI binary. See `activate_target` in
> `src/immutable/boot.rs`.

---

## Updating: `deploytix update`

```
deploytix update                 # full pacman -Syu into a new set
deploytix update vim git         # sync/upgrade and also install these
deploytix update --keep 5        # retain 5 previous sets when pruning (default 3)
deploytix update --reboot        # reboot automatically once staged
deploytix -n update              # dry-run: print the plan, change nothing
```

What it does:

1. Snapshots the current `{@, @usr, @etc}` into a new **writable** set and writes
   its pairing marker.
2. Mounts the set (root + paired usr/etc, with `/var`, `/home`, `/boot`
   rbind-mounted) and runs `pacman -Syu` + `mkinitcpio -P` inside it via
   `artix-chroot`, or plain `chroot` where `artools-base` is not installed.
3. On success, points the default boot entry at the new set and regenerates
   grub.cfg. **Reboot to activate.**
4. On failure, deletes the half-built set and leaves the running system
   untouched.
5. Prunes sets beyond `--keep`, never removing the running set or the new one.

The running system is never modified, so an interrupted or failed update is a
no-op.

> **API filesystems in the update chroot.** `artools-base` (which provides
> `artix-chroot`) is a host/ISO dependency and is *not* installed into deployed
> systems, so `deploytix update` and `rollback` normally chroot with plain
> `chroot`. That mounts nothing, so deploytix mounts `/proc`, `/sys`, `/dev`
> (+ `pts`, `shm`), `/run` and `/tmp` into the target itself and releases them
> afterwards (`utils::command::chroot_api_setup_cmd`). Without `/proc` the
> `/etc/mtab` symlink (`../proc/self/mounts`) dangles and pacman aborts the
> transaction with *"could not determine filesystem mount points"*; `grub-probe`
> and `mkinitcpio` fail for the same reason.

---

## Rolling back: `deploytix rollback`

```
deploytix rollback --list        # show targets (current marked *)
deploytix rollback               # step back one set
deploytix rollback <id>          # roll back to a specific set id
deploytix rollback @             # roll back to the pristine base install
deploytix rollback <id> --reboot # activate immediately
```

Rollback only moves the boot pointer + regenerates grub.cfg — nothing is deleted,
so it is itself reversible (roll "forward" to a newer set). The interactive
grub-btrfs menu remains as a manual recovery path.

---

## Direct-pacman prevention

Enforcement is the **read-only `/usr` mount** itself: a direct `pacman -Syu` on
the live system cannot modify `/usr`, so it fails. Use `deploytix update` instead.

For a friendlier, earlier failure, installs drop a `/etc/profile.d`
snippet (`deploytix-immutable.sh`) that intercepts *interactive* `pacman`
upgrade/install/remove on an immutable system (pairing marker present **and**
`/usr` read-only) and points you at `deploytix update`. `command pacman …`
bypasses it.

> **Why not a pacman `PreTransaction` hook?** `basestrap`/`pacstrap` run
> `pacman -r <newroot>`, which reads hooks from the *host's* hookdir but runs each
> hook's `Exec` chrooted into the new root. A `Target = *` hook execing any binary
> therefore aborts every install-to-another-root — breaking ISO builds and
> deploytix's own deploys from an immutable machine. So the lockdown is a
> shell-level nudge, not a pacman hook.

---

## Caveats & limitations

- **Shared `/boot`.** The kernel and initramfs live on a separate,
  non-snapshotted `/boot` partition, so they are shared across all sets. A
  rollback restores userspace (`@`/`@usr`/`@etc`) but boots with the most
  recently installed kernel. The `mountcrypt` hook is version-independent, so
  this is safe; only kernel *contents* are not rolled back.
- **`/` is read-only, and writes to it fail.** There is no overlay catching
  stray writes to paths outside `/tmp`, `/etc`, `/var`, `/home`, `/root`,
  `/opt`, `/srv`. `/mnt` and `/media` in particular are read-only, so a runtime
  `mkdir /mnt/usb` will not work — mount under `/run/media` (what udisks and
  desktop automounters already use) instead.
- **`/etc` is writable at runtime** (a subvolume, not an overlay). Runtime edits
  mutate `@etc` directly and are captured in the next set; a rollback restores
  the paired `@etc`. This is per-set, not per-boot isolation.
- **Multi-filesystem atomicity.** With `@usr` on a separate `Crypt-Usr`
  container, a set's three snapshots cannot share one btrfs transaction; they are
  created in sequence and bound by the shared id + marker. `deploytix update`
  deletes a partially built set on any failure.
- **Recovery.** A bad activated set is always escapable: pick an older entry from
  the grub-btrfs menu, or boot any set and run `deploytix rollback`.

---

## Implementation map

| Concern | Location |
|---------|----------|
| Config flag `immutable_root` | `src/config/deployment.rs` |
| Subvolume roles, marker, device detection, live marker | `src/immutable/mod.rs` |
| `@etc` creation + mount | `src/immutable/etc.rs` |
| Paired snapshot sets | `src/immutable/snapshot.rs` |
| Boot pointer (grub) | `src/immutable/boot.rs` |
| `deploytix update` | `src/immutable/update.rs` |
| `deploytix rollback` | `src/immutable/rollback.rs` |
| Interactive direct-pacman nudge (profile.d) | `src/immutable/lockdown.rs` |
| Read-only fstab + `@etc` entry | `src/install/fstab.rs` |
| Read-only mounts + marker resolution in initramfs | `src/configure/hooks.rs` |
| Writable-path bind sources (`/var/roothome`, `/var/opt`, `/var/srv`) | `src/immutable/mod.rs` |
| grub-btrfs config, ESP snapshot list, install-time regeneration | `src/configure/grub_btrfs.rs` |
| CLI subcommands | `src/main.rs` |
