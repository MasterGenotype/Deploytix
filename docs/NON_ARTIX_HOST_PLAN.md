# Non-Artix Host Support — Implementation Plan

Enable deploytix to deploy a complete Artix Linux system from **any** Linux
host (Ubuntu, Debian, Fedora, Arch, …) by provisioning a minimal Artix
"helper" environment on demand and replacing every Artix-specific host tool
with a portable equivalent. The end state: `basestrap`, `artix-chroot`, and
`artools` are no longer host requirements — they become one of two
interchangeable backends.

## 1. Background & current failure

Today the host must be Artix. On any other distro the install dies in
Phase 1:

```
⚠ Missing host system dependencies:
  - basestrap (package: artools)
Packages to install: artools
Installing missing packages...
✗ IO error: No such file or directory (os error 2)      ← exec("pacman") on Ubuntu
```

The manual workaround (build an Artix chroot with
`vps-setup/scripts/setup-artix-chroot.sh`, run deploytix inside it) works —
this plan folds that workaround into deploytix itself, up to the point where
the chroot exists, then replaces `basestrap` with a plain `pacman
--root` install driven from the helper.

## 2. Architecture overview

Introduce the concept of an **install backend**, selected in Phase 1:

| Backend | When | How base system is installed |
|---|---|---|
| `native` | Host is Artix (or has `basestrap` + pacman) | `basestrap` (unchanged, current behavior) |
| `helper` | Any other Linux host | Artix helper rootfs is provisioned; `pacman --root <target>` runs **inside** the helper |

Everything outside Phase 3 already runs on plain host tools (`sfdisk`,
`mkfs.*`, `cryptsetup`, `mount`) or inside the **target** chroot
(`grub-install`, `useradd`, `mkinitcpio`), so the backend only has to cover:

1. Detecting the host and choosing/offering the backend.
2. Provisioning the helper (port of `setup-artix-chroot.sh`).
3. Installing the base package set without `basestrap`.
4. Running target-chroot commands without `artix-chroot` (mostly done —
   needs API-filesystem mounts added to the plain-`chroot` fallback).
5. Installing missing *host* dependencies with the host's own package
   manager instead of hardcoded `pacman`.

```
Ubuntu host
├── Phase 1  detect: non-Artix → offer helper backend
├── Phase 1b provision helper  /var/lib/deploytix/artix-helper  (once, cached)
│            └── weekly ISO → verify sha256 → unsquashfs rootfs → API mounts
├── Phase 2  partition/format/mount /install          (host tools, unchanged)
├── Phase 3  bind /install → helper/mnt/target
│            chroot helper: pacman --root /mnt/target -Sy --noconfirm <pkgs>
├── Phase 4+ chroot /install directly                 (plain chroot + API mounts)
└── Phase 6  finalize, unmount helper binds
```

## 3. Inventory of Artix-specific touchpoints

| Location | What | Change |
|---|---|---|
| `src/utils/deps.rs:39` | `basestrap → artools` package mapping | required only for `native` backend |
| `src/utils/deps.rs:194-209` | installs missing deps via hardcoded `pacman -S` | route through new distro adapter |
| `src/install/basestrap.rs:925-1017` | `run_basestrap[_with_retries]` invokes `basestrap` | add backend dispatch: `native` → basestrap, `helper` → helper pacman |
| `src/utils/command.rs:66-73` | `run_in_artix_chroot` falls back to bare `chroot` **without API mounts** | fix fallback: ensure `/dev`, `/proc`, `/sys`, `/run` (+ resolv.conf) are mounted before chrooting |
| `src/install/installer.rs:730-748` | target keyring init (`pacman-key --populate artix`) | unchanged — runs in target chroot, works under both backends |
| `basestrap.rs` custom-repo machinery (`ensure_custom_packages`, temp repo, generated pacman.conf) | host-path based | make repo dir visible inside helper (bind or copy) and rewrite paths in the generated pacman.conf for the helper's namespace |
| makepkg on-demand builds (`build_package_from_source`) | needs pacman/makepkg host | run inside helper as unprivileged user for the helper backend |
| README / CLAUDE.md "Artix only" claims | docs | update |

## 4. Host detection (`src/host/detect.rs`, new)

```rust
pub enum HostDistro { Artix, Arch, Debian, Ubuntu, Fedora, OpenSuse, Other(String) }

pub struct HostEnvironment {
    pub distro: HostDistro,          // /etc/os-release ID= / ID_LIKE=
    pub has_basestrap: bool,         // command_exists("basestrap")
    pub has_pacman: bool,
    pub package_manager: PkgMgr,     // Apt | Dnf | Zypper | Pacman | Unknown
}

pub fn detect() -> HostEnvironment;
pub fn preferred_backend(&self) -> InstallBackend;  // Native iff has_basestrap
```

Parsing rules: `ID=artix` → Artix; `ID_LIKE` containing `arch`/`debian`/
`rhel fedora`/`suse` buckets the rest. `Other` is fully supported as long as
the required host binaries exist (the helper path needs only curl/tar-level
tooling, not a known package manager).

## 5. UX / decision flow

**Phase 1 (`prepare()`), replacing the current hard failure:**

- Backend `native` available → proceed exactly as today.
- Otherwise print a distro-aware notice and offer:
  1. **Provision Artix helper automatically** (recommended, default) —
     shows download size (~800 MB ISO), cache location, disk usage.
  2. **Abort** with instructions (install artools, or run from Artix).
- CLI: `dialoguer` confirm prompt (respects `--noconfirm`-style flags).
- GUI: a new panel/dialog in the Review step showing the same choice, with
  progress reporting for the ISO download + extraction (reuse the existing
  progress-callback channel).
- Non-interactive (config-driven) runs: controlled by config (below); the
  default `auto` selects helper without prompting when stdin is not a TTY.

**Config & CLI surface:**

```toml
[host]                       # new section, all optional
backend = "auto"             # auto | native | helper
helper_dir = "/var/lib/deploytix/artix-helper"
iso_cache_dir = "/var/cache/deploytix/artix"
iso_url = ""                 # pin a specific ISO (default: discover weekly)
keep_helper = true           # false = delete helper after successful install
```

CLI: `deploytix install --backend helper`, plus a maintenance subcommand
`deploytix helper <setup|status|destroy>` for provisioning/cleaning outside
an install run.

## 6. Helper provisioning (`src/host/helper.rs`, port of `setup-artix-chroot.sh`)

Native Rust port (not shelling out to the script) so the GUI can drive it
with progress and it ships inside the binary. Steps, mirroring the script:

1. **Host prerequisites** — `curl`-equivalent (use `ureq`/`reqwest` in-process
   instead of the curl binary), `unsquashfs` (squashfs-tools), `rsync` or
   in-process copy. Missing ones installed via the distro adapter (§8) or
   reported with per-distro install commands.
2. **ISO discovery** — fetch `https://download.artixlinux.org/weekly-iso/sha256sums`,
   pick newest `artix-base-runit-*-x86_64.iso` (variant configurable),
   fall back to directory-index scrape; honor `iso_url` pin.
3. **Download + verify** — resumable download into `iso_cache_dir`; verify
   sha256 from the manifest (hard fail on mismatch, same policy as the
   script; PGP fallback for pinned non-weekly URLs can come later).
4. **Extract rootfs** — locate the squashfs (`rootfs.sfs` etc., same
   candidate-priority logic) and `unsquashfs` into `helper_dir`. Validate
   with the script's `is_root_filesystem` checks + `/etc/os-release`
   contains Artix.
5. **Prepare** — disable pacman `CheckSpace`, copy host `resolv.conf`,
   write a `.deploytix-helper` metadata file (ISO URL/sha, created-at) so
   re-runs can detect a valid cached helper and skip everything above.
6. **Mounts** — rbind `/dev`, mount `proc`, rbind `/sys` and `/run` under
   `helper_dir` (make-rslave), registered with the existing signal-safe
   cleanup so interrupts unmount them.
7. **Keyring** — inside helper: `pacman-key --init && pacman-key --populate
   artix` + `pacman -Sy` (metadata only). One-time per helper.

Idempotency: a valid metadata file + passing sanity check ⇒ reuse; `--refresh`
or `helper destroy` forces a rebuild. Provisioning happens **before Phase 2**
so no disk writes occur if the download fails.

## 7. Basestrap replacement (`helper` backend Phase 3)

`basestrap` ≈ Artix pacstrap: it bootstraps `<target>` using the host's
pacman and Artix repos. Equivalent with a helper:

1. Bind-mount the mounted target root (`/install`) to
   `<helper_dir>/mnt/target`.
2. Make the custom `[deploytix]` repo reachable: copy (small) or bind
   `/tmp/deploytix-local-repo` to `<helper_dir>/tmp/deploytix-local-repo`;
   generate the custom pacman.conf **against helper-internal paths**
   (`Server = file:///tmp/deploytix-local-repo`).
3. Run inside the helper (via the fixed chroot runner):

   ```
   pacman -r /mnt/target --config /etc/deploytix-pacman.conf \
          --cachedir /mnt/target/var/cache/pacman/pkg \
          --noconfirm -Sy <package list>
   ```

   `--cachedir` on the target keeps downloaded packages out of the helper
   and preserves them for the installed system, matching basestrap.
4. Post-bootstrap parity with basestrap: copy the helper's
   `/etc/pacman.d/mirrorlist` and `/etc/pacman.conf` into the target if the
   packages didn't provide them, and copy `resolv.conf` for in-chroot
   network use (already done later by configure phases if present).
5. Reuse `run_basestrap_with_retries`' retry/error-classification logic —
   extract the retry wrapper so both backends share it.

`build_package_list()` is backend-independent and unchanged. The existing
target-side keyring init (`installer.rs:730-748`) runs afterward as today.

**Custom package builds:** when `tkg-gui-git` etc. must be built from
source under the helper backend, run `makepkg` inside the helper as an
unprivileged user (create `builder` uid in the helper; bind the
`~/.gitrepos/deploytix` tree read-only into it). The weekly base ISO ships
`base-devel`; verify at provision time and `pacman -S --needed base-devel
git` into the helper if absent.

## 8. Host dependency installer (`src/host/pkgmgr.rs`)

Replace the hardcoded `pacman -S` in `deps.rs` with an adapter keyed by
`HostEnvironment.package_manager`, plus per-distro package-name maps for the
existing binary list (only names differ):

| binary | Artix/Arch | Debian/Ubuntu | Fedora |
|---|---|---|---|
| `mkfs.vfat` | dosfstools | dosfstools | dosfstools |
| `mkfs.btrfs` | btrfs-progs | btrfs-progs | btrfs-progs |
| `cryptsetup` | cryptsetup | cryptsetup | cryptsetup |
| `lvcreate` | lvm2 | lvm2 | lvm2 |
| `sfdisk` | util-linux | fdisk | util-linux |
| `unsquashfs` | squashfs-tools | squashfs-tools | squashfs-tools |
| `basestrap` | artools | — (helper backend instead) | — |

Unknown package manager ⇒ list the missing binaries and per-distro hints,
don't attempt to install. `grub-install`/`mkinitcpio` are **not** host deps
(they run in the target chroot) — audit `deps.rs` so the helper backend only
demands what the host truly executes.

## 9. Chroot runner fix (`src/utils/command.rs`)

`run_in_artix_chroot`'s bare-`chroot` fallback currently skips API
filesystems — fine under `artix-chroot`, broken without it (`grub-install`,
`mkinitcpio`, `pacman-key` all need `/dev`, `/proc`, `/sys`). Change:

- New `ensure_api_mounts(root)` — idempotent (`mountpoint -q` guard) rbind
  of `/dev`, `proc`, `/sys`, `/run` + `resolv.conf` copy; registered for
  signal-safe cleanup; used by both the target chroot and the helper chroot.
- `run_in_chroot` calls it before plain `chroot`. When `artix-chroot`
  exists, behavior is unchanged.
- `unmount_all` (chroot.rs) already unmounts deepest-first; add the API
  mounts to its list so Phase 6 leaves nothing behind (the current Artix
  path gets this from `artix-chroot`'s own cleanup).

## 10. Module layout & change list

```
src/host/                    (new)
  mod.rs
  detect.rs                  HostDistro, HostEnvironment, backend selection
  pkgmgr.rs                  apt/dnf/zypper/pacman adapters + name maps
  iso.rs                     weekly-ISO discovery, download, sha256 verify, cache
  helper.rs                  extract, validate, mounts, keyring, enter/exec, destroy
src/install/basestrap.rs     backend dispatch in run_basestrap_with_retries;
                             helper-aware custom-repo plumbing
src/install/installer.rs     Phase-1 backend decision + Phase-1b provisioning hook;
                             progress reporting for download/extract
src/utils/command.rs         ensure_api_mounts + fixed chroot fallback
src/utils/deps.rs            backend-aware dep list, adapter-based installs
src/config/deployment.rs     [host] section (backend, helper_dir, iso_url, …)
src/main.rs                  `helper` subcommand, `--backend` flag
gui/                         backend notice + provisioning progress panel
docs/, README.md             replace "Artix Linux Only" with backend matrix
```

Dependency note: prefer `ureq` (small, rustls) for the two HTTP fetches to
avoid requiring a curl binary; sha2 crate already available via cargo tree
or add `sha2`.

## 11. Edge cases & risks

- **Disk space:** ISO (~800 MB) + helper (~2.5 GB extracted). Check free
  space in `helper_dir`/`iso_cache_dir` filesystems before starting; expose
  `keep_helper=false` for constrained hosts.
- **Kernel-feature gaps** (old host kernels): btrfs `block-group-tree`
  already handled; same pattern applies if other mkfs tools grow features —
  keep the "installer must mount what it formats" rule in mind for new fs.
- **No efivars on BIOS hosts:** already handled (skip efibootmgr).
- **SELinux hosts (Fedora):** bind-mounts + chroot generally fine, but
  label the helper dir `unconfined` or document `setenforce 0` caveat;
  test explicitly before claiming Fedora support.
- **Signature trust:** helper pacman verifies Artix package sigs after
  `pacman-key --populate artix`; the keyring comes from the sha256-verified
  ISO — same trust chain as the manual script.
- **Concurrent runs:** helper gets a lock file (same pattern as the GUI
  single-instance lock, PID-stale-aware) so two installs don't share
  mounts.
- **Rehearsal mode:** helper provisioning is *not* wiped by DiskWipeGuard
  (it lives off-target); rehearsal exercises the helper pacman path
  end-to-end, which is exactly what we want on the Ubuntu VPS.
- **Nested-chroot case:** running deploytix *inside* an Artix chroot (the
  current workflow) still detects as Artix/native and keeps working; the
  helper backend removes the need for it but must not break it.

## 12. Testing plan

1. **Unit:** os-release parsing fixtures (artix/arch/ubuntu/fedora/unknown);
   package-name maps; backend selection matrix; pacman.conf path rewriting.
2. **Ubuntu 22.04 VPS (the real rig):**
   - `deploytix helper setup` — provisions from scratch; re-run is a no-op.
   - `deploytix rehearse -c deploytix.toml` with `backend=auto` — full
     61-op rehearsal green, base system installed via helper pacman.
   - Real install to `/dev/sdb`, then loop-mount and inspect: fstab,
     users, services, GRUB fallback loader present.
   - Interrupt (SIGINT) during Phase 3 — helper binds and target mounts all
     released.
3. **Artix regression:** run the existing chroot-based flow — `native`
   backend selected automatically, zero behavior change.
4. **Negative:** no network (ISO fetch fails before disk writes), sha256
   mismatch, helper dir on full filesystem.

## 13. Milestones

| # | Deliverable | Size |
|---|---|---|
| 1 | `src/host/detect.rs` + graceful Phase-1 offer (replaces the pacman ENOENT crash) + `[host]` config | S |
| 2 | Distro adapter for host deps (`deps.rs` rework) | S |
| 3 | `ensure_api_mounts` + chroot fallback fix | S |
| 4 | ISO discovery/download/verify + helper extraction (`helper setup` subcommand) | M |
| 5 | Helper-backed Phase 3 (pacman --root, custom-repo plumbing, retries) | M |
| 6 | makepkg-in-helper for custom packages | M |
| 7 | GUI integration (offer dialog + provisioning progress) | S/M |
| 8 | Docs + Ubuntu VPS validation + Artix regression pass | S |

Milestones 1–3 are independently shippable and immediately improve the
non-Artix experience (clear offer/error instead of `os error 2`). 4–5 form
the core feature; 6 unblocks fully self-contained installs; 7–8 polish.
