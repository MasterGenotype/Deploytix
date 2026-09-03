# Building a Custom Deploytix ISO

Build a bootable Artix Linux ISO with deploytix pre-installed using `buildiso` from `artools-iso`.

> `artools` is now three packages — `artools-base` (`basestrap`, `artix-chroot`),
> `artools-pkg` (package building) and `artools-iso` (`buildiso`). The shared
> paths below (`/usr/share/artools`, `~/artools-workspace`,
> `~/.config/artools`) are unchanged by the split.

## Prerequisites

```sh
sudo pacman -S artools-base artools-pkg artools-iso iso-profiles base-devel go-yq
```

The first run of `buildiso -q` generates `~/artools-workspace/`. Copy the profiles there if not already present:

```sh
buildiso -p base -i openrc -q
cp -r /usr/share/artools/iso-profiles ~/artools-workspace/
```

The `loop` kernel module must be loaded:

```sh
sudo modprobe loop
```

## Quick Start

From the repository root:

```sh
# Base ISO with CLI deploytix (openrc)
./iso/build-deploytix-iso.sh

# Runit base ISO
./iso/build-deploytix-iso.sh -i runit

# Plasma ISO with GUI + CLI deploytix (dinit)
./iso/build-deploytix-iso.sh -g -i dinit

# LXQt ISO with GUI deploytix (s6)
./iso/build-deploytix-iso.sh -g -b lxqt -i s6
```

The ISO is written to `~/artools-workspace/iso/deploytix/` (or `<dir>/workspace/iso/deploytix/` when the build is relocated with `-w`, see [Building from a live USB session](#building-from-a-live-usb-session)).

## Options

| Flag | Description | Default |
|------|-------------|---------|
| `-i <init>` | Init system: `openrc`, `runit`, `dinit`, `s6` | `openrc` |
| `-g` | Include GUI (`deploytix-gui-git` + desktop environment) | off |
| `-b <de>` | Desktop profile for GUI mode (`plasma`, `lxqt`, `xfce`, etc.) | `plasma` |
| `-s` | Skip package rebuild (reuse existing `.pkg.tar.zst` in `pkg/`) | off |
| `-c` | Clean buildiso work directory before building | off |
| `-x` | Build chroot only (stop before ISO generation) | off |
| `-w <dir>` | Build work directory (artools `chroots_dir`); the workspace and the finished ISO move under it too | `/var/lib/artools` |
| `-r` | Reset — remove installed profile, repo, and pacman.conf override | off |
| `-n` | Dry run — print actions without executing | off |
| `-h` | Show help | — |

## Building from a live USB session

`buildiso` assembles the live filesystem with an overlay mount whose *upperdir*
is `<chroots_dir>/buildiso/deploytix/artix/livefs`. The kernel refuses an
upperdir that is itself on an overlay, so on a live USB or ISO session — where
`/` is squashfs plus a COW overlay — the default `/var/lib/artools` cannot work
and the build dies with:

```
mount: /var/lib/artools/buildiso/deploytix/artix/livefs
fsconfig() overlay failed: filesystem on .../livefs not supported as upperdir
```

Nothing under `/` helps here: `/tmp`, `$HOME` and `/var/lib` are all the same
overlay (or tmpfs, when the medium has no persistence partition). Build onto a
real filesystem instead — ext4, xfs, btrfs or f2fs, on its own block device:

```sh
sudo mkdir -p /mnt/build
sudo mount /dev/sdXY /mnt/build            # ext4/xfs/btrfs partition, ~20 GiB+
./iso/build-deploytix-iso.sh -w /mnt/build -g -i runit
```

`-w` writes `chroots_dir` and `workspace_dir` into `~/.config/artools/artools.conf`
and `/etc/artools/artools.conf` (both backed up, both restored by `-r`), so the
chroots, the squashfs *and* the finished ISO all land on the real disk — the ISO
is several gigabytes and would otherwise be written to `$HOME` in RAM. The ISO
appears in `<dir>/workspace/iso/deploytix/`.

The script checks this before building anything: it classifies the filesystem
behind the work directory, then test-mounts a throwaway overlay with it as the
upperdir. An unsuitable path fails in a second with a list of candidate mounts,
rather than an hour later at the livefs mount.

The ext4 `cow_persistence` partition written by `write-deploytix-usb.sh` is
itself a valid target (it is mounted directly, typically at
`/run/artix/cowspace`, not through the overlay) — but the build then consumes the
live session's writable space, so a separate disk is the safer choice.

The same failure appears when building inside a Docker/Podman container on an
overlay graph driver; `-w` pointed at a volume on a real filesystem fixes it too.

## What the Script Does

1. **Builds deploytix packages** — runs `makepkg` in `pkg/` and clones/builds `tkg-gui-git`
2. **Creates a local pacman repository** — copies packages to `/var/lib/artools/repos/deploytix/` and runs `repo-add`
3. **Configures pacman** — installs a custom `iso-x86_64.conf` in `~/.config/artools/pacman.conf.d/` with a `[deploytix]` repo pointing to the local repository
4. **Installs the ISO profile** — copies the deploytix profile to `~/artools-workspace/iso-profiles/deploytix/`
5. **Embeds packages in live-overlay** — copies `.pkg.tar.zst` files and a pacman database into the ISO at `/var/lib/deploytix-repo`; at runtime the Rust installer detects this repo and generates a temporary pacman.conf for basestrap
6. **Runs `buildiso`** — produces the ISO at `~/artools-workspace/iso/deploytix/`
7. **Overrides the live GRUB templates** — the ISO auto-selects the appropriate default boot entry after a 1 second timeout instead of waiting indefinitely at the GRUB menu

To remove all installed artifacts (profile, repo, pacman.conf override), run `./iso/build-deploytix-iso.sh -r`.

## Live environment resources

**Swap.** The live session runs zram — compressed swap in RAM — sized at 50% of
`MemTotal` with a 512 MiB floor, enabled at boot as the `zram` service. The
worker is `/usr/local/bin/deploytix-zram-swap` (`start`/`stop`); per-init service
definitions for runit, OpenRC, s6 and dinit all ship in the profile's
`root-overlay`, and the build fails if the one matching `-i` is missing.

zram was chosen over a swap partition on the stick because it works on every
boot mode and does not write to USB flash. Note what it does and does not buy
you: compression (zstd, typically 2–3×) raises *effective* memory capacity. It
does not create disk space, and it will not make a multi-gigabyte build fit.

**The writable layer.** The live root is squashfs plus an `overlay=livefs` COW
layer, and where that COW lives depends on how the medium was made:

| How it was written | COW backing |
|---|---|
| `write-deploytix-usb.sh` | ext4 partition labeled `cow_persistence`, taking all free space on the stick |
| plain `dd`, a VM, optical, or the "From CD/DVD/ISO" entry | **RAM** (tmpfs) |

Only the first gets a `cow_label=` on the kernel cmdline. On the others, anything
written during the session — including `/tmp` and `/var/tmp` — is held in memory.

**Disk usage during an install.** Package downloads do *not* consume the live
medium. `basestrap` and the in-chroot `pacman`/`yay` calls write to the target's
`/var/cache/pacman/pkg`, and the custom deploytix packages come from the offline
repo baked into the ISO at `/var/lib/deploytix-repo` — nothing is compiled on the
live medium. See [`docs/DISK_SPACE_GUIDE.md`](../docs/DISK_SPACE_GUIDE.md) for
sizing the *target* disk.

## Customisation

### Modifying the profile

Edit `iso/profile/deploytix/profile.yaml` to add or remove packages from the ISO. The format matches the standard artools iso-profiles YAML schema.

### Adding overlay files

Place files in `iso/profile/deploytix/live-overlay/` to overlay them onto the live session filesystem. For example, to include a default deploytix config:

```
iso/profile/deploytix/live-overlay/etc/skel/.config/deploytix/config.toml
```

### Using a custom pacman mirror

The script starts from the system `iso-x86_64.conf`. If you need custom mirrors or additional repos beyond `[deploytix]`, edit the generated file in `~/.config/artools/pacman.conf.d/iso-x86_64.conf` between the `install_pacman_conf` and `run_buildiso` steps (use `-x` to stop after chroot build for manual tweaks).

## Burning the ISO

```sh
# USB stick (replace /dev/sdX)
sudo dd if=~/artools-workspace/iso/deploytix/artix-deploytix-*.iso of=/dev/sdX bs=4M status=progress oflag=sync
```

For a persistent USB that also patches older ISOs to auto-boot the default Deploytix entry, use:

```sh
sudo ./iso/write-deploytix-usb.sh -d /dev/sdX
```
