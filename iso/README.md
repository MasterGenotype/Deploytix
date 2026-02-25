# Building a Custom Deploytix ISO

Build a bootable Artix Linux ISO with deploytix pre-installed using `buildiso` from artools.

## Prerequisites

```sh
sudo pacman -S artools iso-profiles base-devel go-yq
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

The ISO is written to `~/artools-workspace/iso/deploytix/`.

## Options

| Flag | Description | Default |
|------|-------------|---------|
| `-i <init>` | Init system: `openrc`, `runit`, `dinit`, `s6` | `openrc` |
| `-g` | Include GUI (`deploytix-gui-git` + desktop environment) | off |
| `-b <de>` | Desktop profile for GUI mode (`plasma`, `lxqt`, `xfce`, etc.) | `plasma` |
| `-s` | Skip package rebuild (reuse existing `.pkg.tar.zst` in `pkg/`) | off |
| `-c` | Clean buildiso work directory before building | off |
| `-x` | Build chroot only (stop before ISO generation) | off |
| `-n` | Dry run — print actions without executing | off |
| `-h` | Show help | — |

## What the Script Does

1. **Builds deploytix packages** — runs `makepkg` in `pkg/` using the existing PKGBUILD
2. **Creates a local pacman repository** — copies packages to `/var/lib/artools/repos/deploytix/` and runs `repo-add`
3. **Configures pacman** — installs a custom `iso-x86_64.conf` in `~/.config/artools/pacman.conf.d/` with a `[deploytix]` repo pointing to the local repository
4. **Installs the ISO profile** — copies the deploytix profile to `~/artools-workspace/iso-profiles/deploytix/`
5. **Runs `buildiso`** — produces the ISO at `~/artools-workspace/iso/deploytix/`
6. **Cleans up** — restores the original pacman.conf on exit (including on failure)

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
