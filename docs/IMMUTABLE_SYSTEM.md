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

A **baseline set** is created at the end of the install, read-only, capturing the
pristine system. Without it a fresh install has no snapshots at all, so
grub-btrfs's generator finds nothing and the boot menu ships with an empty
snapshot submenu. The install also regenerates `grub.cfg` once grub-btrfs is
present — the bootloader phase runs `grub-mkconfig` before the package is
installed, so the grub.cfg it writes predates `/etc/grub.d/41_snapshots-btrfs`.

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

> **Nothing regenerates grub against the live root.** Two things enforce that.
> `activate_target` uses the chroot below. And grub-btrfsd — which fires whenever
> the watched snapshot directory changes — is pointed at
> `/usr/local/bin/deploytix-grub-regen`, a guard that refuses on an overlay root.
> Without it the daemon would overwrite the good grub.cfg the chroot just wrote:
> `41_snapshots-btrfs` reads *"not a btrfs filesystem"* off an overlay, prints
> *"Root filesystem isn't btrfs"* and **exits 0**, dropping the snapshot menu
> while reporting success.
>
> **Snapshot entries inside a signed EFI binary.** grub-btrfs writes its entries
> to a *separate* `grub-btrfs.cfg` and has grub.cfg reach them with
> `configfile ${prefix}/grub-btrfs.cfg`. In a standalone image `${prefix}` is
> `(memdisk)/boot/grub`, **not** the real `/boot/grub` — so unless that file is
> embedded alongside grub.cfg, the submenu's own `[ ! -e ... ]` test fails on
> every boot and no snapshot menu appears, however many sets exist and however
> correctly `grub-btrfs.cfg` was written to disk. Both `grub-mkstandalone`
> invocations embed it when present. Embedding also brings the entries under the
> SecureBoot signature instead of trusting a file on the ESP.
>
> **Signed EFI binaries.** Where sbctl SecureBoot meets an encrypted disk the
> installer builds standalone GRUB, embedding grub.cfg in the signed binary. A
> pointer move that only rewrote `/boot/grub/grub.cfg` would never be read at
> boot, so `activate_target` runs `/usr/local/bin/reinstall-grub` there instead —
> it regenerates, rebuilds and re-signs.
>
> **Devices are resolved, not assumed.** `detect_devices()` reads the backing
> devices out of `/proc/self/mounts` (probing `/etc`, since `/` is an overlay and
> names no block device). The `/dev/mapper/Crypt-*` constants are only a
> fallback: they do not exist on an unencrypted install, and `resolve_mapper_name`
> can hand a second deploytix system `Crypt-Root-1`.

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

1. Picks the set to write into. If an update is already staged for the next boot
   (you forgot a package and ran `update` again), that set is filled further and
   stays the boot target. Otherwise `{@, @usr, @etc}` of **whatever boots next**
   are snapshotted into a new **writable** set with its pairing marker.
2. Mounts the set (root + paired usr/etc, with `/var`, `/home`, `/boot`
   rbind-mounted) and runs `pacman -Syu` + `mkinitcpio -P` inside it via
   `artix-chroot`, or plain `chroot` where `artools` is not installed.
3. On success, points the default boot entry at the new set and regenerates
   grub.cfg. **Reboot to activate.**
4. On failure, deletes the half-built set and leaves the running system
   untouched.
5. Prunes sets beyond `--keep`, never removing the running set or the new one.

The running system is never modified, so an interrupted or failed update is a
no-op. A failed update on a *staged* set leaves that set exactly as the earlier
update left it, still selected for the next boot.

> **Updates chain, they do not restart.** Each update sources the next boot
> pointer, never the pristine `@` — which is read-only for the life of the
> system, so re-sourcing it would discard every package earlier updates
> installed. Concretely:
>
> | Running | Staged for next boot | `deploytix update foo` writes into |
> |---------|----------------------|------------------------------------|
> | `@` | `@` (nothing staged) | a new set snapshotted from `@` |
> | set `X` | set `X` (nothing staged) | a new set snapshotted from `X` |
> | set `X` | set `Y` (update staged) | **set `Y` itself** — `foo` joins it |
> | set `X` | set `R` (rollback staged, read-only) | a new set snapshotted from `R` |
>
> The running set is never written to in place: that would mutate the live root
> and leave nothing to roll back to. The running root comes from
> `rootflags=subvol=` on `/proc/cmdline` (the same value the `mountcrypt` hook
> resolves); the staged pointer comes from grub.cfg. Pruning also uses the
> running root, so the set the session is booted from is never pruned away.
>
> The LVM A/B backend follows the same rule with slots: a second update extends
> the staged slot instead of rebuilding "the inactive slot", which after staging
> is the running root. It skips the rsync in that case — the slot already holds
> the running tree plus what earlier updates added.

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

## Kernels and `/boot`

`/boot` is outside the snapshot, so the transaction has to reconcile it
explicitly. Two things used to go wrong, and both are closed:

- pacman's kernel install hook and `mkinitcpio -P` write **through** the
  rbind-mounted `/boot` during the update. If a later step failed, the set was
  discarded but `/boot` kept the new `vmlinuz`/`initramfs` — and the set that
  still booted had no matching `/usr/lib/modules`.
- A rollback moved the pointer but not the kernel, so an older set booted
  against whatever was installed last.

Each set now keeps its own images:

```
/boot/deploytix/base/     kernel of the pristine @ install
/boot/deploytix/<id>/     kernel each set was built with
/boot/vmlinuz-linux-zen   canonical names — a copy of the selected set's
/boot/initramfs-*.img     images, rewritten on every pointer move
```

| Moment | What happens to `/boot` |
|--------|-------------------------|
| Install | `base` and the baseline set are archived |
| Update succeeds | the set's images are archived, then restored under the canonical names |
| Update fails | the **running** set's archive is restored, undoing whatever the transaction wrote |
| Rollback | the target set's archive is restored before grub.cfg is regenerated |
| Set pruned | its archive goes with it; orphans and interrupted archives are swept |

Only the canonical names are ever booted. GRUB's `10_linux` and grub-btrfs both
glob the *top level* of `/boot`, so the nested archive directories are invisible
to them: no menu entry has to be hand-written, and `/boot/efi` and `/boot/grub`
are never in scope. Archives are built under a temporary name and swapped in;
restores land each file with a rename, so an interrupted run leaves the previous
archive intact rather than a torn one. On a btrfs `/boot` (the default when the
data filesystem is btrfs) `cp --reflink=auto` makes an archive nearly free; on
ext4 budget roughly a kernel plus two initramfs images per retained set.

Two limits remain:

- **The grub-btrfs menu still boots the canonical kernel.** Those entries are
  generated by upstream's `boot_separate()` branch, which uses the live `/boot`
  for every snapshot. `deploytix rollback` is the kernel-correct path; the
  grub-btrfs menu remains the manual recovery path it was.
- **Microcode (`*-ucode.img`) stays shared.** It is not kernel-versioned, so
  there is nothing to roll back.

---

## Caveats & limitations

- **Shared `/boot`, per-set kernel archives.** The kernel and initramfs live on
  a separate, non-snapshotted `/boot` partition, so the files themselves are
  shared while `/usr/lib/modules/<ver>` lives inside each set. Each set
  therefore keeps a copy of the images it was built with under
  `/boot/deploytix/<id>/`, and **every boot-pointer move restores that copy over
  the canonical `/boot/vmlinuz-*` / `/boot/initramfs-*` names**. The invariant is
  that the canonical images always match the set the pointer selects, so a
  rollback boots the kernel its modules match, and a failed update cannot leave
  a newer kernel over an older set. See `src/immutable/bootset.rs`.
- **`/etc` is writable at runtime** (a subvolume, not an overlay). Runtime edits
  mutate `@etc` directly and are captured in the next set; a rollback restores
  the paired `@etc`. This is per-set, not per-boot isolation.
- **Multi-filesystem atomicity.** With `@usr` on a separate `Crypt-Usr`
  container, a set's three snapshots cannot share one btrfs transaction; they are
  created in sequence and bound by the shared id + marker. `deploytix update`
  deletes a partially built set on any failure.
- **Recovery.** A bad activated set is always escapable: pick an older entry from
  the grub-btrfs menu, or boot any set and run `deploytix rollback`.
- **The baseline set is prunable.** It is an ordinary set, so `deploytix update`
  will eventually prune it past `--keep`. The pristine install stays reachable
  regardless via `deploytix rollback @`, which repoints at `@` itself.

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
| Per-set kernel archives on `/boot` | `src/immutable/bootset.rs` |
| Interactive direct-pacman nudge (profile.d) | `src/immutable/lockdown.rs` |
| Read-only fstab + `@etc` entry | `src/install/fstab.rs` |
| Read-only mounts + marker resolution in initramfs | `src/configure/hooks.rs` |
| CLI subcommands | `src/main.rs` |
