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
  rolled back). `/tmp`, `/root`, `/srv`, `/opt` land on an ephemeral overlay.

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
| `@overlay` | initramfs scratch | ephemeral | — |
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
   - mounts the pointer subvolume **read-only** as `/`;
   - layers the ephemeral, disk-backed `@overlay` over it (so `/tmp`, `/root`,
     `/srv`, `/opt`, and any stray writes to `/` work but do **not** persist);
   - reads `/.deploytix-pair` and mounts the paired `@usr` **read-only** at
     `/usr` and `@etc` **read-write** at `/etc`;
   - mounts `@var`, `@log`, `@home` read-write.

Because the pointer + marker drive everything, switching systems is just a
pointer move + `grub-mkconfig`.

> **Regenerating grub off an overlay root.** On a booted immutable system `/` is
> an overlayfs, and `grub-probe` aborts with *"failed to get canonical path of
> `overlay'"* if run against it — producing an empty grub.cfg. So `update` and
> `rollback` never run `grub-mkconfig` against the live `/`. Instead they mount
> the target `{root,usr,etc}` set at a scratch chroot (a **real** btrfs root
> where `grub-probe` works), point that root's `/etc/default/grub` at itself, and
> run `grub-mkconfig` from inside the chroot to write the shared
> `/boot/grub/grub.cfg`. See `activate_target` in `src/immutable/boot.rs`.

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

1. Snapshots the **running** `{root, usr, etc}` trio into a new **writable** set
   and writes its pairing marker.

   The running trio is read from the kernel cmdline's `rootflags=subvol=` — what
   the initramfs actually mounted — not from the boot pointer in grub.cfg (which
   names what boots *next*) and not from the mount table (`/` is an overlayfs,
   so it cannot name the subvolume underneath). On a never-updated system that
   trio is the base `{@, @usr, @etc}`; afterwards it is the previously activated
   set. Snapshotting the running set rather than the base is what makes updates
   **stack**: update 2 contains update 1's changes.
2. Mounts the set (root + paired usr/etc, with `/var`, `/home`, `/boot`
   rbind-mounted) and runs `pacman -Syu` + `mkinitcpio -P` inside it via
   `artix-chroot`, or plain `chroot` where `artools` is not installed.
3. On success, points the default boot entry at the new set and regenerates
   grub.cfg. **Reboot to activate.**
4. On failure, deletes the half-built set and leaves the running system
   untouched.
5. Prunes sets beyond `--keep`, never removing the running set or the new one.
   "Running" here is the set resolved in step 1, captured *before* the pointer
   moves — reading the pointer after activation would name the set just staged
   and leave the booted one eligible for deletion.

The running system is never modified, so an interrupted or failed update is a
no-op.

> **API filesystems in the update chroot.** `artools` (which provides
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

## Snapshot entries in the GRUB menu

grub-btrfs lists every snapshot on the root filesystem that contains a `/boot`
directory — deploytix sets (`@deploytix-sets/<id>/root`) and snapper's
`@snapshots/<n>/snapshot` alike — under an *"Artix Linux snapshots"* submenu.
Selecting one boots it read-only with the ephemeral overlay; a set's
`.deploytix-pair` marker pulls in its matching `/usr` and `/etc`.

Two things keep that list current:

- `deploytix update` / `rollback` regenerate grub.cfg (in the chroot, see
  above), and grub-btrfs's generator runs as part of that.
- For snapshots created outside deploytix (snapper timeline, `snapper create`),
  the `grub-btrfsd` service is meant to regenerate on the fly. The **stock
  daemon cannot do that here**: it runs the generator against the live `/`,
  which is an overlayfs — the generator exits with *"Root filesystem isn't
  btrfs"* and `grub-probe` fails outright. Immutable installs therefore run
  `/usr/local/bin/deploytix-grub-btrfsd` instead (written by the grub-btrfs
  phase, same service name and `/.snapshots` watch), which calls
  `deploytix regen-grub` on every change. That command reads the current boot
  pointer from grub.cfg and re-runs `activate_target` for it: mount the
  pointed-at set at the scratch chroot, `grub-mkconfig` there, pointer
  unchanged.

```
deploytix regen-grub             # regenerate grub.cfg for the current pointer
deploytix -n regen-grub          # dry-run: show the strategy and commands
```

`regen-grub` is layout-aware, so it is also the right command on a mutable
deploytix install (reinstall-grub pipeline on encrypted layouts, plain
`grub-mkconfig` otherwise). On the LVM A/B backend it refuses: that backend
keeps its slot pointer with in-place edits of grub.cfg that a `grub-mkconfig`
would discard.

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
| Boot pointer (grub), `deploytix regen-grub` strategy | `src/immutable/boot.rs` |
| `deploytix update` | `src/immutable/update.rs` |
| `deploytix rollback` | `src/immutable/rollback.rs` |
| grub-btrfsd replacement (`deploytix-grub-btrfsd`) | `src/configure/grub_btrfs.rs` |
| Interactive direct-pacman nudge (profile.d) | `src/immutable/lockdown.rs` |
| Read-only fstab + `@etc` entry | `src/install/fstab.rs` |
| Read-only mounts + marker resolution in initramfs | `src/configure/hooks.rs` |
| CLI subcommands | `src/main.rs` |
