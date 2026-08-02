# Non-Artix Host Support — Task List

Derived from `NON_ARTIX_HOST_PLAN.md`. Ordered by milestone; M1–M3 are independently shippable, M4–M5 are the core feature, M6–M8 are polish/validation.

---

## Milestone 1 — Host detection, Phase-1 offer, `[host]` config (S)

- [ ] Create `src/host/mod.rs` and `src/host/detect.rs`
  - [ ] Implement `HostDistro` enum (`Artix, Arch, Debian, Ubuntu, Fedora, OpenSuse, Other(String)`)
  - [ ] Implement `HostEnvironment` struct (`distro`, `has_basestrap`, `has_pacman`, `package_manager: PkgMgr`)
  - [ ] Parse `/etc/os-release`: `ID=artix` → Artix; bucket the rest via `ID_LIKE` (`arch`, `debian`, `rhel fedora`, `suse`); `Other` fully supported when required host binaries exist
  - [ ] Implement `detect()` and `preferred_backend()` (Native iff `has_basestrap`)
- [ ] Replace the Phase-1 hard failure (`pacman` ENOENT / `os error 2`) with a graceful decision flow in `prepare()`
  - [ ] Backend `native` available → proceed unchanged
  - [ ] Otherwise: distro-aware notice offering (1) provision Artix helper automatically (default; show ~800 MB ISO download size, cache location, disk usage) or (2) abort with instructions (install artools / run from Artix)
  - [ ] CLI: `dialoguer` confirm prompt, respecting `--noconfirm`-style flags
  - [ ] Non-interactive/config-driven runs: `backend = "auto"` selects helper without prompting when stdin is not a TTY
- [ ] Add `[host]` config section to `src/config/deployment.rs` (all optional): `backend` (`auto|native|helper`), `helper_dir`, `iso_cache_dir`, `iso_url`, `keep_helper`
- [ ] Add CLI surface in `src/main.rs`: `deploytix install --backend <b>` flag and `deploytix helper <setup|status|destroy>` subcommand (stubs OK until M4)

## Milestone 2 — Distro adapter for host deps (`deps.rs` rework) (S)

- [ ] Create `src/host/pkgmgr.rs` with adapters for Apt / Dnf / Zypper / Pacman
- [ ] Replace hardcoded `pacman -S` install path in `src/utils/deps.rs:194-209` with the adapter keyed by `HostEnvironment.package_manager`
- [ ] Add per-distro package-name maps for the host binary list (e.g. `sfdisk` → util-linux on Arch/Fedora but fdisk on Debian/Ubuntu; add `unsquashfs` → squashfs-tools)
- [ ] Make the `basestrap → artools` mapping (`deps.rs:39`) required only for the `native` backend
- [ ] Unknown package manager → list missing binaries with per-distro install hints instead of attempting installation
- [ ] Audit `deps.rs` so the helper backend only demands what the host truly executes (`grub-install`/`mkinitcpio` are target-chroot tools, not host deps)

## Milestone 3 — API mounts + chroot fallback fix (S)

- [ ] Implement `ensure_api_mounts(root)` in `src/utils/command.rs`
  - [ ] Idempotent (`mountpoint -q` guard) rbind of `/dev`, mount `proc`, rbind `/sys`, `/run`; copy `resolv.conf`
  - [ ] Register mounts with the signal-safe cleanup mechanism
  - [ ] Shared by both the target chroot and the helper chroot
- [ ] Fix `run_in_artix_chroot`'s bare-`chroot` fallback (`command.rs:66-73`) to call `ensure_api_mounts` before chrooting; behavior unchanged when `artix-chroot` exists
- [ ] Add the API mounts to `unmount_all` (chroot.rs, deepest-first) so Phase 6 leaves nothing mounted

## Milestone 4 — ISO acquisition + helper provisioning (M)

- [ ] Create `src/host/iso.rs`
  - [ ] ISO discovery: fetch `https://download.artixlinux.org/weekly-iso/sha256sums`, pick newest `artix-base-runit-*-x86_64.iso` (variant configurable); directory-index scrape fallback; honor `iso_url` pin
  - [ ] Resumable download into `iso_cache_dir`; verify sha256 from manifest, hard fail on mismatch (PGP fallback for pinned non-weekly URLs deferred)
  - [ ] Use `ureq` (rustls) for HTTP in-process instead of a curl binary; add `sha2` crate if not already in the tree
- [ ] Create `src/host/helper.rs` — native Rust port of `setup-artix-chroot.sh` (no shell-out, so GUI can drive progress)
  - [ ] Host prerequisite check: `unsquashfs`, rsync-or-in-process copy; install missing via the distro adapter or report per-distro commands
  - [ ] Locate squashfs (`rootfs.sfs` etc., same candidate-priority logic) and `unsquashfs` into `helper_dir`
  - [ ] Validate with `is_root_filesystem`-style checks + `/etc/os-release` contains Artix
  - [ ] Prepare: disable pacman `CheckSpace`, copy host `resolv.conf`, write `.deploytix-helper` metadata (ISO URL/sha, created-at)
  - [ ] Mounts: rbind `/dev`, mount `proc`, rbind `/sys` + `/run` (make-rslave), registered for signal-safe cleanup
  - [ ] Keyring (one-time per helper): `pacman-key --init && pacman-key --populate artix` + `pacman -Sy` (metadata only)
  - [ ] Idempotency: valid metadata + sanity check ⇒ reuse; `--refresh` / `helper destroy` forces rebuild
  - [ ] Provision before Phase 2 so no disk writes occur if the download fails
  - [ ] Free-space check on `helper_dir` / `iso_cache_dir` filesystems (~800 MB ISO + ~2.5 GB extracted); support `keep_helper = false` for constrained hosts
  - [ ] Lock file for concurrent runs (PID-stale-aware, same pattern as the GUI single-instance lock)
- [ ] Wire up `deploytix helper setup|status|destroy` to this module
- [ ] Hook Phase-1b provisioning into `src/install/installer.rs` with progress reporting for download/extract (reuse existing progress-callback channel)

## Milestone 5 — Helper-backed Phase 3 (basestrap replacement) (M)

- [ ] Add backend dispatch in `run_basestrap[_with_retries]` (`src/install/basestrap.rs:925-1017`): `native` → basestrap (unchanged), `helper` → helper pacman
- [ ] Extract the retry/error-classification wrapper so both backends share it
- [ ] Bind-mount the mounted target root (`/install`) to `<helper_dir>/mnt/target`
- [ ] Custom-repo plumbing for the helper namespace: copy or bind `/tmp/deploytix-local-repo` into `<helper_dir>/tmp/deploytix-local-repo`; generate the custom pacman.conf against helper-internal paths (`Server = file:///tmp/deploytix-local-repo`)
- [ ] Run inside the helper via the fixed chroot runner: `pacman -r /mnt/target --config /etc/deploytix-pacman.conf --cachedir /mnt/target/var/cache/pacman/pkg --noconfirm -Sy <packages>` (target-side cachedir keeps packages out of the helper and preserves them for the installed system)
- [ ] Post-bootstrap parity: copy helper `/etc/pacman.d/mirrorlist` and `/etc/pacman.conf` into the target if packages didn't provide them; copy `resolv.conf` for in-chroot network use
- [ ] Confirm `build_package_list()` stays backend-independent and target keyring init (`installer.rs:730-748`) runs unchanged afterward

## Milestone 6 — makepkg-in-helper for custom packages (M)

- [ ] Run `makepkg` inside the helper as an unprivileged user for the helper backend (`build_package_from_source`)
  - [ ] Create a `builder` uid in the helper
  - [ ] Bind the `~/.gitrepos/deploytix` tree read-only into the helper
- [ ] Verify `base-devel` at provision time; `pacman -S --needed base-devel git` into the helper if absent

## Milestone 7 — GUI integration (S/M)

- [ ] New panel/dialog in the Review step offering the backend choice (same options as CLI)
- [ ] Progress reporting for ISO download + extraction via the existing progress-callback channel

## Milestone 8 — Docs + validation (S)

### Documentation
- [ ] Update README / CLAUDE.md: replace "Artix Linux Only" claims with the backend matrix
- [ ] Document the SELinux caveat for Fedora hosts (label helper dir `unconfined` or note `setenforce 0`); don't claim Fedora support until explicitly tested

### Unit tests
- [ ] os-release parsing fixtures (artix / arch / ubuntu / fedora / unknown)
- [ ] Package-name maps
- [ ] Backend selection matrix
- [ ] pacman.conf path rewriting

### Ubuntu 22.04 VPS (the real rig)
- [ ] `deploytix helper setup` provisions from scratch; re-run is a no-op
- [ ] `deploytix rehearse -c deploytix.toml` with `backend=auto` — full 61-op rehearsal green, base installed via helper pacman (helper dir must survive DiskWipeGuard — it lives off-target)
- [ ] Real install to `/dev/sdb`; loop-mount and inspect fstab, users, services, GRUB fallback loader
- [ ] SIGINT during Phase 3 — verify all helper binds and target mounts are released

### Regression / negative
- [ ] Artix regression: existing chroot-based flow auto-selects `native`, zero behavior change (nested-chroot workflow must keep working)
- [ ] No network: ISO fetch fails before any disk writes
- [ ] sha256 mismatch: hard failure
- [ ] Helper dir on a full filesystem
