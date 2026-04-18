# Deploytix — Arch/Artix snapshot build

Snapshot of the `main` branch at commit [`e34a93c`](https://github.com/MasterGenotype/Deploytix/commit/e34a93ca01a0e38e3d51611dd109010416566268) (9 commits past `v1.2.6`). Built with `makepkg` on Arch Linux.

## Package version

`1.2.6.r9.ge34a93c-1`  (x86_64)

## Assets

| File | Description |
|---|---|
| `deploytix-git-1.2.6.r9.ge34a93c-1-x86_64.pkg.tar.zst` | CLI installer (`/usr/bin/deploytix`) |
| `deploytix-gui-git-1.2.6.r9.ge34a93c-1-x86_64.pkg.tar.zst` | egui graphical wizard (`/usr/bin/deploytix-gui`) + `.desktop` + polkit policy |
| `deploytix-git-debug-1.2.6.r9.ge34a93c-1-x86_64.pkg.tar.zst` | Detached debug symbols |
| `PKGBUILD` | Updated PKGBUILD (always builds from tip of `main`) |
| `.SRCINFO` | Generated SRCINFO for AUR |
| `SHA256SUMS` | Checksums for all assets |

## Install

```sh
# CLI only
sudo pacman -U deploytix-git-1.2.6.r9.ge34a93c-1-x86_64.pkg.tar.zst

# GUI (pulls in the CLI package as a dependency)
sudo pacman -U \
  deploytix-git-1.2.6.r9.ge34a93c-1-x86_64.pkg.tar.zst \
  deploytix-gui-git-1.2.6.r9.ge34a93c-1-x86_64.pkg.tar.zst
```

## Build it yourself

```sh
curl -LO https://github.com/MasterGenotype/Deploytix/releases/download/v1.2.6-r9-ge34a93c/PKGBUILD
makepkg -sCf
```

The PKGBUILD now points `source=` at `git+https://github.com/MasterGenotype/Deploytix.git#branch=main`, so rebuilding always pulls the latest `main`.

## Runtime dependencies

- CLI (`deploytix-git`): `gcc-libs`, `alsa-lib`
- GUI (`deploytix-gui-git`): above + `libxkbcommon`, `libxcb`, `wayland`, `mesa`

Recommended optional dependencies for full installer functionality: `dosfstools`, `e2fsprogs`, `btrfs-progs`, `xfsprogs`, `f2fs-tools`, `cryptsetup`, `lvm2`, `grub`, `artools`.

## Changes in this build

- PKGBUILD: explicitly track `main` (`#branch=main`).
- PKGBUILD: add `alsa-lib` to `makedepends` and runtime `depends` (needed by `rodio`/`alsa-sys`).
