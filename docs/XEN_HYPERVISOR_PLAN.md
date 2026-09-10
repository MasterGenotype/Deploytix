# Xen Hypervisor Support in Deploytix — Design & Implementation Plan

Status: **plan / not implemented**
Source material: [Xen — ArchWiki](https://wiki.archlinux.org/title/Xen)

---

## 0. Scope and provenance

This document takes the ArchWiki Xen article — a guide written for a *mutable,
systemd, single-boot-path Arch install* — and reinterprets every step of it for
the systems Deploytix actually builds: Artix with runit/OpenRC/s6/dinit, LUKS2
multi-volume encryption, dm-verity or btrfs-snapshot immutable roots, SecureBoot
via sbctl or shim, and GRUB as the only bootloader.

The goal is not "Xen also installs". The goal is **Xen as a first-class
Deploytix feature that composes with every other feature the installer offers**,
including the ones the wiki has never had to think about: a read-only `/` and
`/usr`, an atomic A/B boot pointer, a grub.cfg embedded inside a signed EFI
binary, and an encrypted `/boot`.

### A note on sources

`wiki.archlinux.org` is blocked by this environment's egress policy. The article
text used here was taken in full from a public Markdown mirror of the ArchWiki
(`greg-js/arch-wiki-md-repo`, `wiki/_content/english/Xen.md`), which is a
snapshot rather than the live page. Everything attributed to the wiki below was
read from that snapshot verbatim, including its code blocks. Where the snapshot
may lag the live article — package names (`ovmf` vs `edk2-ovmf`), the exact Xen
version suffixes, and whether the Xen package still ships `/etc/grub.d/09_xen`
rather than GRUB's own `20_linux_xen` — this is called out inline as
**[verify]**. Those points change details of the implementation, never its
shape.

---

## Part I — What the wiki actually prescribes

Condensed, with the parts that matter to us kept verbatim.

### I.1 Requirements

- Dom0 kernel support is built into Arch's `linux`/`linux-lts`. Deploytix ships
  `linux-zen` (or `linux-tkg`), which likewise carries the Xen guest and dom0
  drivers.
- HVM domU needs Intel VT-x / AMD-V: `grep -E "(vmx|svm)" /proc/cpuinfo`.
- PCI passthrough needs IOMMU/VT-d.

### I.2 Installation

> To install the Xen hypervisor, install the `xen` package \[from the AUR]. It
> provides the Xen hypervisor, current xl interface and all configuration and
> support files, including systemd services. The multilib repository needs to be
> enabled and the `multilib-devel` package group installed to compile Xen.

Plus `xen-docs`, and `seabios` and/or `ovmf` **[verify: `edk2-ovmf`]** for BIOS
and UEFI guests respectively.

### I.3 Bootloader — the four modes the wiki documents

> The boot loader must be modified to load a special Xen kernel (`xen.gz` or in
> the case of UEFI `xen.efi`) which is then used to boot the normal kernel.

**UEFI (`xen.efi` direct).** `xen-X.Y.Z.efi` goes on the ESP alongside the
kernel and ramdisk, with an ASCII config file next to it:

```
[global]
default=xen

[xen]
options=console=vga iommu=force:true,qinval:true,debug:true loglvl=all noreboot=true reboot=no vga=ask ucode=scan
kernel=vmlinuz-linux root=/dev/sdaX rw add_efi_memmap #earlyprintk=xen
ramdisk=initramfs-linux.img
```

Note the structure: `options=` is the **hypervisor** command line, `kernel=` is
the **dom0 kernel** and *its* command line, `ramdisk=` is the dom0 initramfs.
That three-way split is the single most important thing to carry across.

**systemd-boot.** A type-2 `efi /xen-X.Y.Z.efi` entry. The wiki notes the
loader cannot pass `-cfg=`, so only one Xen config is reachable. Irrelevant to
us — Deploytix installs GRUB only.

**EFISTUB / UEFI shell.** `FS0:\> xen-X.Y.Z.efi -cfg=xen-rescue.cfg`. Useful as
a rescue path, not as an install target.

**BIOS / GRUB.**

> For GRUB users, the Xen package provides the `/etc/grub.d/09_xen` generator
> file. The file `/etc/xen/grub.conf` can be edited to customize the Xen boot
> commands. For example, to allocate 512 MiB of RAM to dom0 at boot, modify
> `/etc/xen/grub.conf` by replacing the line `#XEN_HYPERVISOR_CMDLINE="xsave=1"`
> with `XEN_HYPERVISOR_CMDLINE="dom0_mem=512M xsave=1"`.

then `grub-mkconfig -o /boot/grub/grub.cfg`.

**Syslinux.** `mboot.c32` with `---`-separated multiboot modules. Not applicable,
but it shows the multiboot module layout plainly:
`xen-X.Y.Z.gz --- vmlinuz-linux <dom0 args> --- initramfs-linux.img`.

### I.4 Networking

> Xen requires that network communications between domU and the dom0 (and
> beyond) be set up manually. […] A basic bridged network, in which a virtual
> switch is created in dom0 that every domU is attached to, can be set up by
> creating a network bridge with the expected name `xenbr0`.

The wiki then documents systemd-networkd and NetworkManager. `/etc/xen/scripts`
holds the alternative topologies (NAT, routed).

### I.5 Services

> The Xen dom0 requires the `xenstored.service`, `xenconsoled.service`,
> `xendomains.service` and `xen-init-dom0.service` to be started and possibly
> enabled.

### I.6 Post-install checks and best practice

- `xl list` must show `Domain-0`.
- A xenfs mount, straight from the wiki:
  ```
  none /proc/xen xenfs defaults 0 0
  ```
- Xen Project Best Practices: pin a fixed dom0 memory allocation and dedicate
  (pin) CPU cores to dom0. Disable toolstack autoballooning when `dom0_mem` is
  fixed.

### I.7 domU

Config files in `/etc/xen`, autostart via symlinks in `/etc/xen/auto` driven by
the `xendomains` service, disks as LVs / raw partitions / sparse images
(`truncate -s 10G domU.img`), `vif = [ 'mac=00:16:3e:XX:XX:XX,bridge=xenbr0' ]`.

The wiki's PV-domU section tells you to add `xen-blkfront`, `xen-fbfront`,
`xen-netfront`, `xen-kbdfront` to mkinitcpio. **This is guest-side advice and
must not be copied into dom0's `MODULES`** — a mistake that is easy to make when
mechanically porting the article. Dom0 wants the *backend* drivers
(`xen-blkback`, `xen-netback`, `xen-gntdev`, `xen-evtchn`), and it wants them at
runtime, not in the initramfs.

---

## Part II — Why none of that transfers unmodified

Five structural mismatches, each of which drives a section of the design.

| # | Wiki assumption | Deploytix reality |
|---|-----------------|-------------------|
| 1 | systemd units ship with the package and are enabled with `systemctl` | Four init systems, none of them systemd. The AUR package's units are dead weight; Deploytix must author service definitions for runit, OpenRC, s6 and dinit, as it already does for HHD, Decky and evdevhook2 |
| 2 | `/etc/default/grub` and `/etc/xen/grub.conf` are edited by hand, then `grub-mkconfig` is re-run whenever you like | `/etc/default/grub` is **generated wholesale** by `configure_grub_defaults*()` (`src/configure/boot/bootloader.rs`), and on SecureBoot+encryption the config that actually boots is embedded in a signed EFI binary — a bare `grub-mkconfig` writes a file nothing reads |
| 3 | The system is mutable; you install Xen whenever | `/` and `/usr` are read-only. `pacman -Syu` physically cannot run. Xen must be installed *during* the deploy, or transactionally through `deploytix update` |
| 4 | grub.cfg is regenerated freely | The LVM A/B backend **never** runs `grub-mkconfig` after install — `grub-probe` cannot canonicalize a dm-verity root. Boot changes are `sed` rewrites of two tokens (`src/immutable/lvm_ab.rs:activate_slot`) |
| 5 | Boot chain has one link (GRUB → Linux) | Boot chain gains a link (GRUB → Xen → dom0 Linux), and that new link sits *outside* whatever SecureBoot verification the existing chain has |

---

## Part III — Boot architecture

### III.1 Two boot modes, one primary

Deploytix should support the GRUB/multiboot2 path as the **primary and default**
mode, and treat `xen.efi` as an opt-in secondary mode.

**Mode A — GRUB multiboot2 (default).**

```
firmware → GRUB (EFI, possibly standalone+signed)
         → multiboot2 /xen.gz          [hypervisor + its cmdline]
         → module2   /vmlinuz-linux-zen [dom0 kernel + its cmdline]
         → module2   /initramfs-...img  [dom0 initramfs]
```

Why this is the default: **`xen.efi` reads its kernel and ramdisk from the ESP
using firmware file I/O only.** It cannot read an encrypted `/boot`, an LVM LV,
or a btrfs subvolume. GRUB can — that is the entire reason Deploytix's encrypted
layouts work today, with `GRUB_ENABLE_CRYPTODISK=y` and the LUKS modules baked
into the core image. Mode A therefore composes with encryption, LVM thin, btrfs
subvolumes and dm-verity; Mode B composes with none of them.

**Mode B — `xen.efi` chainloaded from GRUB (opt-in).**
`chainloader /xen.efi` with `xen.cfg`, kernel and initramfs all on the ESP.
Only offered when `disk.boot_encryption = false` and the root layout is one
whose kernel can be mirrored to the ESP. Its advantage is a shorter, cleaner
chain and Xen's own EFI memory-map handling; its cost is that the dom0 kernel
must live unencrypted on a FAT partition.

**Hard rule to encode in validation: Mode B is incompatible with
`boot_encryption`, and incompatible with the LVM A/B backend** (the A/B pointer
lives in the GRUB cmdline; `xen.cfg` has no equivalent that `activate_slot`
could sed without also re-signing).

### III.2 The three-way command line split

This is the crux of integrating Xen with Deploytix's existing cmdline
construction. Today `configure_grub_defaults()`,
`configure_grub_defaults_lvm_thin()` and `configure_grub_defaults_lvm_ab()` each
build **one** `GRUB_CMDLINE_LINUX_DEFAULT` carrying everything: `root=`,
`rootflags=subvol=@`, `cryptdevice=`, `cryptkey=`, `deploytix.slot=`,
`deploytix.roothash=`, `resume=`, `rw`/`ro`.

With Xen, those arguments split by destination:

| Destination | Carrier | Contents |
|---|---|---|
| Hypervisor | `XEN_HYPERVISOR_CMDLINE` in `/etc/xen/grub.conf` **[verify]**, or `GRUB_CMDLINE_XEN` / `GRUB_CMDLINE_XEN_DEFAULT` in `/etc/default/grub` | `dom0_mem=`, `dom0_max_vcpus=`, `dom0_vcpus_pin`, `iommu=`, `dom0-iommu=`, `ucode=scan`, `smt=`, `loglvl=`, `console=`, `com1=` |
| Dom0 kernel | `GRUB_CMDLINE_LINUX_DEFAULT` (unchanged) | everything Deploytix already emits, plus `xen-pciback.hide=` |
| Dom0 initramfs | `module2` line | n/a |

**The good news, verified against the code:** every Deploytix boot-pointer
mechanism keeps working, because the generator copies
`GRUB_CMDLINE_LINUX_DEFAULT` onto the `module2` dom0 line, and both pointer
rewriters are line-agnostic regular expressions:

- `src/immutable/boot.rs` — `sed -i 's|rootflags=subvol=[^ "]*|rootflags=subvol=<x>|'`
- `src/immutable/lvm_ab.rs` — `sed -i 's|deploytix.slot=[^ "]*|...|g; s|deploytix.roothash=[^ "]*|...|g'`

Neither anchors to a `linux` keyword, so both hit the `module2` line. **No
change is required to the pointer rewriters.** This should be asserted by a test
rather than assumed (see Part XI).

Implementation shape: add a `xen_hypervisor_cmdline(config) -> String` helper
alongside the existing cmdline builders, and have each of the three
`configure_grub_defaults*` functions append the Xen block when
`config.packages.install_xen` is set. Keep it a separate function so the
existing three stay readable.

Default hypervisor cmdline Deploytix should generate:

```
dom0_mem=<computed>,max:<computed> dom0_max_vcpus=<n> dom0_vcpus_pin ucode=scan
```

with `dom0_mem` defaulting to a computed value rather than the wiki's
`512M` — 512 MiB is a 2013-era number and is not enough for a dom0 running a
desktop. Proposal: `clamp(host_ram / 8, 2 GiB, 8 GiB)`, echoing the existing
proportional-partitioning idiom (fixed floors first, proportions after), and
pinned as `dom0_mem=N,max:N` so ballooning is off, per Xen Best Practices as the
wiki cites.

### III.3 GRUB module set — a concrete, verified gap

`GRUB_STANDALONE_MODULES` in `src/configure/boot/bootloader.rs:17` lists 60-odd
modules. It contains `linux`, `chain`, `cryptodisk`, `luks2`, the gcry ciphers —
and **no `multiboot2`**. On a SecureBoot + encryption install, `grub-mkstandalone`
builds `BOOTX64.EFI` from exactly that list, so a Xen menu entry in the embedded
grub.cfg would fail at `multiboot2: command not found` — after the machine has
already committed to booting it.

Fix: append `multiboot2` (and `multiboot` only if BIOS support is ever added) to
the constant. `grub-mkstandalone` resolves module dependencies from `moddep.lst`,
so `relocator` and friends come along automatically; they do not need listing.
Add `chain` — already present — for Mode B.

This is a one-line change with an outsized failure mode, and it should land with
a unit test asserting `multiboot2` is in the list whenever Xen is enabled.

### III.4 SecureBoot — what is actually protected, stated honestly

Deploytix has three SecureBoot methods (`SecureBootMethod::{Sbctl, ManualKeys,
Shim}`), and Xen interacts differently with each. This section is deliberately
blunt, because the comfortable answer here is the wrong one.

**sbctl + encryption (standalone GRUB).** `run_grub_mkstandalone()` builds a
self-contained binary with `--disable-shim-lock` and the whole thing is signed by
`sbctl sign-all`. Because the shim-lock verifier is disabled, GRUB's multiboot2
loader will happily load `xen.gz` **without verifying it**. The chain is:

```
firmware --verifies--> BOOTX64.EFI (signed)  ✓
BOOTX64.EFI ----------> xen.gz               ✗ unverified
xen.gz ---------------> vmlinuz              ✗ unverified
```

Today, without Xen, that same standalone GRUB loads `vmlinuz` through the
`linux` command — also unverified by shim, though `sign_boot_files()` does sign
every `/boot/vmlinuz-*`, so the kernel at least carries a signature even if
nothing checks it at that link. Adding Xen extends the unverified span by one
hop. **Deploytix must not claim Xen + SecureBoot gives a verified boot chain to
dom0.** What it gives is: firmware-verified GRUB, plus an encrypted and (on A/B)
dm-verity-integrity-checked root. That is a real and defensible security
posture; it is just not "verified boot end to end".

**Shim.** shim installs a verifier into GRUB that refuses to load unsigned
multiboot2 images. Loading `xen.gz` under shim requires the hypervisor binary to
be signed with a key shim trusts, which the AUR package cannot do for us.
**Recommendation: reject `secureboot_method = shim` together with Xen in
validation**, with an error pointing at sbctl, rather than shipping a
configuration that fails at the GRUB prompt on first boot. **[verify]** against
the shipped GRUB build before finalising — if Artix's GRUB is built without the
shim-lock verifier, this restriction can be relaxed to a warning.

**ManualKeys.** `sign-kernel` signs `/boot/vmlinuz-*` and `BOOTX64.EFI`. It
should be extended to also sign `/boot/xen*.efi` when Mode B is used. Signing
`xen.gz` is pointless (nothing checks a signature on a multiboot2 payload).

**Mode B and SecureBoot.** `xen.efi` is a PE binary and *can* be signed and
verified by firmware — so Mode B is the only configuration that could ever offer
a verified GRUB→Xen hop. Xen's own EFI loader then loads the dom0 kernel without
verification, so the chain still breaks one hop later. Worth stating in the
user-facing docs; not worth building the feature around.

**Actionable summary for the implementation:**
1. `sign_boot_files()` gains `/boot/efi/xen*.efi` when Mode B is enabled.
2. `99-secureboot.hook` gains `boot/xen*.efi` as a `Target`.
3. `95-grub-reinstall.hook` gains `boot/xen*.gz` as a `Target` — a Xen upgrade
   must rebuild the standalone binary, exactly as a kernel upgrade does.
4. The enrollment instructions printed by `print_enrollment_instructions()` gain
   a sentence on where verification stops.

### III.5 Encrypted `/boot`

No new mechanism needed, which is the point of choosing Mode A. GRUB unlocks the
LUKS1 `/boot` container using the cryptodisk modules already in
`GRUB_STANDALONE_MODULES`, then reads `xen.gz`, `vmlinuz` and `initramfs` from
the decrypted filesystem exactly as it reads the latter two today. The
`grub-btrfs` compatibility patch (`create_grub_btrfs_compat`) is likewise
unaffected — it rewrites `41_snapshots-btrfs`, which does not emit Xen entries at
all (see IV.5).

The one addition: `GRUB_ENABLE_CRYPTODISK=y` is currently written only when
`boot_encryption` is set. That logic is correct and unchanged.

### III.6 Menu entry ordering and `GRUB_DEFAULT`

Every `configure_grub_defaults*` writes `GRUB_DEFAULT=0`. If the Xen package
installs its generator as `/etc/grub.d/09_xen` **[verify]**, Xen entries sort
*before* `10_linux`, so entry 0 silently becomes "Xen" the moment the package
lands. That is probably the desired outcome on a machine that just asked for a
hypervisor — but it must be a decision, not an accident, and it interacts with
the immutable boot pointer, which assumes the default entry is the one it seds.

Design:
- Add `xen.default_boot: XenDefaultBoot { Xen, Linux }`, defaulting to `Xen`.
- For `Xen`, keep `GRUB_DEFAULT=0` and rely on generator ordering, but assert the
  ordering rather than trusting it: after `grub-mkconfig`, verify that entry 0's
  body contains `multiboot2`, and log loudly if not.
- For `Linux`, emit `GRUB_DEFAULT="<explicit menuentry id>"` and give the Linux
  entries a stable id via `GRUB_DISTRIBUTOR`/`--id`, rather than a fragile index.
- Always keep a plain non-Xen entry in the menu. The wiki says this twice and it
  is right:

  > Never assume your system will boot after changes to the boot system. […]
  > Make sure you have a alternative way to boot your system.

  Because `10_linux` still runs, this is free — but validation should refuse a
  configuration that would suppress it.

---

## Part IV — Immutability

This is where the wiki offers no guidance at all and where most of the design
work lives.

### IV.1 What is and is not affected

Xen dom0 is, from the root filesystem's point of view, just Linux. The initramfs
hooks (`mountcrypt`, `crypttab-unlock`, `verity-ab`) run unchanged; dm-verity
still measures the root LV; the read-only `/usr` is still read-only. Xen does not
need to write to `/` or `/usr` at runtime.

What *is* affected is everything to do with **where Xen's mutable state lives**
and **how grub.cfg is regenerated**.

### IV.2 State map — where Xen writes, and whether that survives

| Path | Used for | btrfs backend | LVM A/B backend | Verdict |
|---|---|---|---|---|
| `/var/lib/xen` | toolstack state, domU images | `@var`, writable, **not snapshotted** | shared `var` LV, excluded from the update rsync | ✅ correct home for VM images |
| `/var/lib/xenstored` | xenstore tdb | `@var` | shared | ✅ |
| `/var/log/xen` | logs | `@log`/`@var` | shared | ✅ |
| `/run/xen`, `/run/xenstored` | sockets | tmpfs | tmpfs | ✅ |
| `/etc/xen/*.cfg` | **domU definitions** | `@etc` — **snapshotted, rolls back** | `/etc` overlay — **per-slot** | ⚠️ see IV.3 |
| `/etc/xen/auto/` | autostart symlinks | `@etc` | `/etc` overlay | ⚠️ see IV.3 |
| `/proc/xen` | xenfs | fstab entry | fstab entry | ✅ |
| `/usr/lib/xen` | toolstack binaries | read-only `@usr` | inside sealed root LV | ✅ read-only is fine |

### IV.3 The domU-config rollback trap

`/etc` is part of the atomic snapshot set on the btrfs backend
(`{@, @usr, @etc}`, `src/immutable/mod.rs`) and is a per-slot overlay on the A/B
backend. So:

> Define a VM today, `deploytix rollback` tomorrow, and the VM definition is
> gone — while its 200 GiB disk image sits orphaned on `/var`.

That is a data-loss-shaped surprise even though no data is actually lost, and it
is exactly the class of bug the existing `WRITABLE_BIND_PATHS` mechanism was
introduced to prevent for `/root`, `/opt` and `/srv`.

**Design: treat domU configuration as machine state, not OS configuration.**

- Deploytix creates `/var/lib/xen/configs/` and `/var/lib/xen/auto/`.
- `/etc/xen/auto` is created as a **symlink** to `/var/lib/xen/auto`.
- The generated `xendomains` service reads from `/var/lib/xen/auto`.
- User-facing docs and the post-install summary say: put `.cfg` files in
  `/var/lib/xen/configs`, symlink into `/var/lib/xen/auto` to autostart.

A symlink is used here rather than an entry in `WRITABLE_BIND_PATHS` because
`/etc/xen` is package-owned only as a directory and the sub-path is ours; the
"bind mounts, not symlinks" reasoning in `immutable/mod.rs` applies to paths the
`filesystem` package owns, which this is not. **[verify]** that the AUR package
does not itself ship `/etc/xen/auto` as a real directory; if it does, use a bind
mount and add it to `WRITABLE_BIND_PATHS` instead.

`/etc/xen/xl.conf` and `/etc/xen/grub.conf` stay in `/etc` — they *are* OS
configuration, and rolling them back with the OS is correct.

### IV.4 The stale-`xen.gz` problem on the A/B backend

**This is the most serious integration hazard in the whole design.**

The A/B backend deliberately never runs `grub-mkconfig` after install
(`src/immutable/lvm_ab.rs`): `grub-probe` cannot canonicalize a dm-verity root,
so activation is a `sed` of `deploytix.slot=` and `deploytix.roothash=` in an
otherwise **frozen** `/boot/grub/grub.cfg`.

That frozen grub.cfg would contain a `multiboot2 /xen-4.20.0.gz` line. Upgrade
Xen inside slot B via `deploytix update`, and the new slot's `/usr` has Xen
4.20.1 — but `/boot` (shared, not snapshotted) now holds `xen-4.20.1.gz` while
grub.cfg still names `xen-4.20.0.gz`. If the package removes the old file, the
machine does not boot, and it does not boot for *both* slots, because grub.cfg is
shared. Rollback does not help.

**Mitigations, in order of preference:**

1. **Pin the entry to a stable path.** Xen ships `/boot/xen.gz` as a symlink to
   the versioned file **[verify]**. Deploytix's generated Xen entry should name
   `/xen.gz`, never the versioned name. This makes the frozen grub.cfg
   version-independent, exactly as the `mountcrypt` hook is deliberately
   version-independent (see the "Caveat: shared /boot" note in
   `src/immutable/update.rs`).
2. **Own the entry.** Rather than relying on the package's `09_xen` generator
   output surviving in a frozen file, have Deploytix write its own
   `/etc/grub.d/09_deploytix_xen` that emits exactly one entry using `/xen.gz`
   and `/vmlinuz-linux-zen` symlinks. Deterministic output is what makes a
   frozen config safe.
3. **Extend `activate_slot`.** Add a third sed for the multiboot2 path. This is
   the fallback if (1) turns out not to hold; it is worse, because it makes the
   pointer rewriter responsible for a filename it cannot validate.

The same reasoning applies, more weakly, to the btrfs backend: it *does* re-run
`grub-mkconfig` in a scratch chroot on every activation
(`src/immutable/boot.rs:activate_target`, which mounts the target set with
`/boot` and `/var` bound so the generators can run), so a version bump is picked
up. But pinning to `/xen.gz` costs nothing and removes a class of failure there
too.

### IV.5 grub-btrfs snapshot entries are not Xen entries

`grub-btrfs`'s `41_snapshots-btrfs` generator emits `linux`/`initrd` entries. It
has no multiboot2 support. So on a btrfs immutable install with
`install_grub_btrfs = true`, the snapshot submenu boots each snapshot as **plain
Linux, without the hypervisor**.

This is not a bug to fix — it is a genuinely useful recovery path (a broken
hypervisor config is recoverable by booting a snapshot natively) — but it is
surprising, and it must be documented in both this plan and the post-install
summary. Users who boot a snapshot and find `xl list` failing should find the
explanation immediately.

### IV.6 Installing and updating Xen transactionally

Xen is an AUR package (`install_yay = true` required), and AUR packages cannot be
built inside the `deploytix update` chroot — no unprivileged build user, no
guarantee of network, and a 30–90 minute compile inside what is supposed to be an
atomic transaction.

Fortunately the update path already has the mechanism needed:
`update::classify_args()` splits its arguments into **local package files** and
repo names, and `update::stage_local_pkgs()` stages the former into the target
before `pacman -U`. Both backends use it.

**Design:**
- At install time, after `yay` builds `xen`, keep the built package artifacts in
  `/var/cache/deploytix/xen/` (on `@var` / the shared `var` LV — surviving both
  rollback and slot switches).
- Document the update path as:
  ```
  deploytix update /var/cache/deploytix/xen/xen-<ver>-x86_64.pkg.tar.zst
  ```
- Optionally add a thin convenience wrapper later (`deploytix update --xen`) that
  rebuilds the AUR package on the live system into that cache directory and then
  invokes the transactional update with it. Out of scope for the first
  implementation; note it as a follow-up.

This keeps the transactional guarantee intact: the build happens outside the
transaction, the install happens inside it, and a failed install leaves the
running slot untouched.

### IV.7 dom0 memory versus the immutable model

Nothing immutability-specific, but one interaction is worth pinning down:
`dom0_mem` fixes dom0's RAM, and Deploytix sizes swap as `2×RAM clamped 4–20 GiB`
from the **host's** total RAM, not dom0's. With `dom0_mem=4G` on a 64 GiB host,
the swap partition is sized for 64 GiB while dom0 can only ever use 4 GiB. Not
harmful, just wasteful; worth a note in the layout computation and a line in the
install summary.

**Hibernation must be disabled.** Xen dom0 cannot hibernate. `resume=` /
`resume_offset=` are emitted by `resume_for_cmdline()` into the dom0 cmdline and
would be inert at best. Validation should reject `system.hibernation = true`
together with Xen.

---

## Part V — Init systems and services

The wiki's four systemd units map onto four Artix init systems. Xen upstream
ships SysV-style `xencommons`, `xendomains` and `xen-watchdog` scripts in
`hotplug/Linux/init.d/`, which is a useful starting point for OpenRC and a useful
*reference* for the others. **[verify]** whether the AUR package installs them.

Follow the established pattern exactly — `write_hhd_service()` /
`write_decky_service()` / `write_evdevhook2_service()` in
`src/install/packages.rs` — one `match config.system.init` with four arms.

### V.1 Service graph

```
xenstored   (longrun)  ─┬─> xenconsoled  (longrun)
                        ├─> xen-init-dom0 (oneshot, after xenstored is up)
                        └─> xendomains    (oneshot up / oneshot down)
xen-watchdog (longrun, optional)
```

`xen-init-dom0` must run **after** xenstored is accepting connections, not merely
after it is started. Under runit and dinit this needs an explicit readiness
check — a poll on `/run/xenstored/socket` — because neither offers a
notification protocol here that the AUR package's script would already satisfy.
This is the same class of ordering problem the existing code documents for
`iwd`-before-`NetworkManager` in `build_service_list()`, and deserves the same
treatment: get the order right *and* make the dependent robust to losing the
race.

### V.2 Per-init sketches

**runit** — `/etc/runit/sv/xenstored/run`, with a `log/run` piping to `svlogd`,
matching `write_hhd_service`:

```sh
#!/bin/sh
exec 2>&1
exec /usr/sbin/xenstored --no-fork
```

`xen-init-dom0` is a oneshot, which runit does not model natively; implement it
as a `run` script that polls for the xenstore socket, runs
`/usr/lib/xen/bin/xen-init-dom0`, and then `exec pause`s (or uses a `finish`
file), following whatever idiom the rest of the codebase settles on for oneshots.

**OpenRC** — `/etc/init.d/xenstored` in the `#!/sbin/openrc-run` form used by
`write_hhd_service`, with a real `depend()` block:

```sh
depend() {
    need udev
    before xenconsoled xendomains
}
```

OpenRC is the one init where the upstream SysV scripts can likely be adopted
with light edits.

**s6** — `/etc/s6/adminsv/xenstored/` with `type` = `longrun`;
`xen-init-dom0` as `type` = `oneshot` with `up`/`down` scripts, which is the one
init that models this cleanly. Dependencies via `dependencies.d/`. Note the
existing caveat in `enable_s6_service()`: enable with `s6 set enable` and commit,
never by writing `contents.d` directly.

**dinit** — `/etc/dinit.d/xenstored`:

```
type = process
command = /usr/sbin/xenstored --no-fork
restart = true
```

and `xen-init-dom0` as `type = scripted` with `depends-on = xenstored`.

### V.3 Wiring into `enable_services`

`build_service_list()` gains a block:

```rust
if config.packages.install_xen {
    services.push("xenstored".to_string());
    services.push("xenconsoled".to_string());
    services.push("xen-init-dom0".to_string());
    services.push("xendomains".to_string());
}
```

`build_service_packages()` must **skip** the `{service}-{init}` package lookup
for all four — there are no `xenstored-runit` packages in Artix; Deploytix writes
the definitions itself. This is the same exemption already made for `greetd` on
s6 and for `elogind`, so extend that existing `match`/`continue` logic rather
than adding a parallel mechanism.

### V.4 Other system configuration

- **xenfs**: append the wiki's `none /proc/xen xenfs defaults 0 0` to fstab.
  Note this is a pseudo-filesystem and so is unaffected by the
  `INITRAMFS_OWNED_MOUNTPOINTS` restriction that keeps `/`, `/usr`, `/etc` out of
  fstab on immutable layouts.
- **Dom0 backend modules**: `/etc/modules-load.d/xen-dom0.conf` with
  `xen-blkback`, `xen-netback`, `xen-gntdev`, `xen-evtchn`. **Not** in mkinitcpio
  `MODULES` — dom0 does not need them to reach its root.
- **`xen-pciback`** (only when passthrough is configured): the module *does*
  belong in the initramfs, since it must claim devices before the native driver
  binds. Add to `construct_modules()` under the passthrough flag, and add
  `xen-pciback.hide=(....)` to the **dom0 kernel** cmdline.
- **udev**: remove `/etc/udev/rules.d/xend.rules` if the package ships it — the
  wiki's troubleshooting section documents it as a deprecated file that produces
  a boot-time error.

---

## Part VI — Networking

The wiki assumes systemd-networkd or NetworkManager. Deploytix offers three
backends (`NetworkBackend::{Iwd, NetworkManager, NetworkManagerWpa}`), and one of
them cannot bridge at all.

### VI.1 NetworkManager / NetworkManagerWpa — bridge

Pre-seed a keyfile connection profile rather than scripting `nmcli` at install
time (there is no running NM in the chroot):

`/etc/NetworkManager/system-connections/xenbr0.nmconnection` (mode 0600), plus a
slave profile binding the wired interface. This lands on `@etc`, so it is
snapshotted with the OS — which is correct for a network profile.

### VI.2 Iwd — bridging is not available, and that is physics

`iwd` is a Wi-Fi supplicant. **802.11 does not bridge**: a station cannot forward
frames with a source MAC other than its own without 4-address mode, which
requires AP-side support. A `xenbr0` bridged onto a Wi-Fi link will silently drop
guest traffic.

So for `NetworkBackend::Iwd`, Deploytix should configure **NAT networking**, not
bridging: Xen's `vif-nat` script, `net.ipv4.ip_forward=1` via a
`/etc/sysctl.d/` drop-in (following the existing `install_sysctl_*` pattern), and
a generated `vif` line using `bridge=` only when a real bridge exists.

This should be surfaced as an install-time **warning**, not an error — a machine
with both Wi-Fi and Ethernet can still bridge the wired link, and Deploytix
cannot always tell which the user means.

### VI.3 A Deploytix-owned bridge service

For hosts with no NM (the iwd-only wired case), a small init-specific
`xen-bridge` service that runs

```sh
ip link add name xenbr0 type bridge
ip link set <iface> master xenbr0
ip link set xenbr0 up
```

is the minimal path. Same four-arm pattern as V.2.

---

## Part VII — Configuration schema

New fields on `PackagesConfig` (and a nested `XenConfig`), following the existing
documentation-comment style with explicit `Requires:` lines:

```rust
/// Install the Xen hypervisor and configure this system as a dom0.
///
/// Adds a GRUB multiboot2 boot path (GRUB -> xen.gz -> dom0 kernel), writes
/// init-specific services for xenstored/xenconsoled/xen-init-dom0/xendomains,
/// and sets up guest networking.
///
/// Requires: install_yay = true (AUR package: xen), bootloader = grub.
/// Incompatible with: system.hibernation, secureboot_method = shim.
#[serde(default)]
pub install_xen: bool,

/// Xen dom0 tuning. Ignored unless install_xen = true.
#[serde(default)]
pub xen: XenConfig,
```

```rust
pub struct XenConfig {
    /// Boot path: Grub (multiboot2, default; works with encryption and
    /// immutable layouts) or XenEfi (chainloaded xen.efi; requires an
    /// unencrypted /boot and no A/B backend).
    pub boot_mode: XenBootMode,          // default: Grub

    /// Fixed dom0 memory, e.g. "4G". None = computed from host RAM
    /// (clamp(RAM/8, 2G, 8G)), pinned as dom0_mem=N,max:N so ballooning is off.
    pub dom0_mem: Option<String>,

    /// dom0 vCPUs; None = computed (min(4, host_cpus/4), floor 2).
    pub dom0_max_vcpus: Option<u32>,

    /// Pin dom0 vCPUs to the first N physical cores.
    pub dom0_vcpus_pin: bool,            // default: true

    /// Enable the IOMMU (required for PCI passthrough).
    pub iommu: bool,                     // default: true

    /// PCI addresses to hide from dom0 for passthrough, e.g. ["0000:01:00.0"].
    /// Adds xen-pciback to the initramfs and xen-pciback.hide= to the dom0 cmdline.
    pub pciback_hide: Vec<String>,

    /// Guest networking: Bridge (xenbr0) or Nat (vif-nat). Defaults to Bridge
    /// on wired/NetworkManager setups and Nat on iwd-only hosts.
    pub networking: XenNetworking,

    /// Bridge name and the interface enslaved to it.
    pub bridge_name: String,             // default: "xenbr0"
    pub bridge_interface: Option<String>,

    /// Which menu entry boots by default.
    pub default_boot: XenDefaultBoot,    // default: Xen

    /// Install seabios / edk2-ovmf for BIOS and UEFI guests.
    pub guest_firmware: bool,            // default: true
}
```

Wizard integration: one top-level question ("Install the Xen hypervisor and
configure this host as a dom0?"), gated on `install_yay`, with the tuning fields
defaulted and reachable only from a config file — matching how
`install_grub_btrfs` and `immutable_root` are handled.

---

## Part VIII — Placement in the installation pipeline

Xen has an awkward requirement: it is an **AUR package** (so it must come after
`install_yay`, Phase 5.3) but it **changes the boot configuration** (which is
generated in Phase 4, long before).

The existing code already documents this exact hazard for `linux-tkg`:

> finalize() re-runs mkinitcpio but never grub-mkconfig, and the pacman hook that
> would otherwise catch a late kernel (`create_grub_reinstall_hook`) is only
> installed on encrypted or LVM-thin layouts. Installed any later on a plain
> layout, the kernel would get an initramfs and no boot entry.
> — `src/install/installer.rs:295`

The same trap applies to Xen, so the same care is needed.

**Proposed placement: Phase 5.9, after yay (5.3) and before finalize (6),**
with an explicit boot-config regeneration at the end of the phase rather than
relying on a hook:

```
Phase 4     configure_system()          -> /etc/default/grub written *including*
                                           the Xen hypervisor cmdline block
                                           (harmless while xen.gz is absent)
Phase 4.5   install_custom_hooks()      -> unchanged
Phase 4.6   SecureBoot setup            -> unchanged
Phase 5.3   install_yay()
Phase 5.9   install_xen()               -> build+install AUR xen, cache the
                                           package artifacts under /var/cache,
                                           write services, bridge/NAT, fstab
                                           xenfs entry, modules-load.d,
                                           /var/lib/xen layout, /etc/xen/auto
                                           symlink, then EXPLICITLY regenerate
                                           the boot config:
                                             - if REINSTALL_GRUB_PATH exists:
                                                 run it (covers standalone GRUB
                                                 rebuild + re-signing)
                                             - else:
                                                 grub-mkconfig -o /boot/grub/grub.cfg
Phase 6     finalize()                  -> mkinitcpio -P; on the A/B backend,
                                           veritysetup format seals slot A
                                           *including* the newly installed Xen,
                                           then the boot pointer is sed-patched
```

Two things make this ordering work, and both should be asserted in tests:

1. Writing the hypervisor cmdline into `/etc/default/grub` at Phase 4 is safe
   because `GRUB_CMDLINE_XEN` is inert when no Xen generator runs.
2. Phase 5.9 lands **before** the A/B verity sealing in Phase 6, so the sealed
   root hash covers the Xen files. Installing Xen any later would seal a root
   that does not contain it — or, worse, invalidate a hash already computed.

Reusing `REINSTALL_GRUB_PATH` rather than calling `grub-mkconfig` directly is
important: on SecureBoot + encryption, a bare `grub-mkconfig` writes a file
nothing reads, because the config that boots is embedded in the signed binary.
The reinstall script already handles mkconfig → mkstandalone → re-sign.

---

## Part IX — Validation rules

To add to `DeploymentConfig::validate()`, in its existing style:

| Condition | Result |
|---|---|
| `install_xen && !install_yay` | **Error** — "Xen requires install_yay = true (AUR package: xen)" |
| `install_xen && system.hibernation` | **Error** — dom0 cannot hibernate |
| `install_xen && secureboot_method == Shim` | **Error** — shim's verifier refuses unsigned multiboot2; use sbctl **[verify]** |
| `xen.boot_mode == XenEfi && disk.boot_encryption` | **Error** — xen.efi cannot read an encrypted /boot |
| `xen.boot_mode == XenEfi && immutable_lvm_ab()` | **Error** — the A/B pointer has no xen.cfg equivalent |
| `install_xen && network.backend == Iwd && xen.networking == Bridge` | **Warning** — 802.11 cannot bridge; suggest Nat |
| `install_xen && !xen.pciback_hide.is_empty() && !xen.iommu` | **Error** — passthrough requires the IOMMU |
| `install_xen && immutable_root` | **Warning** — domU configs under `/etc/xen` roll back with the OS; use `/var/lib/xen/configs` (see IV.3) |
| `install_xen && install_grub_btrfs` | **Warning** — snapshot menu entries boot without the hypervisor (see IV.5) |
| `install_xen && desktop.environment != None` | **Info** — dom0 with a desktop wants a larger `dom0_mem` than the computed default |

---

## Part X — Implementation plan

Ordered so each work package is independently reviewable and testable.

### WP1 — Schema and validation *(no behaviour change)*
- `src/config/deployment.rs`: `install_xen`, `XenConfig`, `XenBootMode`,
  `XenNetworking`, `XenDefaultBoot`, defaults, wizard prompt, all Part IX rules,
  unit tests for each rule.

### WP2 — GRUB module set and cmdline *(smallest change, largest failure mode)*
- `src/configure/boot/bootloader.rs`: add `multiboot2` to `GRUB_STANDALONE_MODULES`.
- New `xen_hypervisor_cmdline(&DeploymentConfig) -> String`.
- Append the `GRUB_CMDLINE_XEN` block in all three `configure_grub_defaults*`.
- Add `boot/xen*.gz` and `boot/xen*.efi` to `95-grub-reinstall.hook` targets.
- Tests: module list contains `multiboot2`; each of the three generators emits
  `dom0_mem=` exactly once; **and a regression test that both pointer `sed`
  expressions still match when the cmdline lives on a `module2` line** (III.2).

### WP3 — Package installation
- `src/install/packages.rs`: `install_xen()` — multilib enablement,
  `multilib-devel`, `yay -S xen xen-docs`, `seabios`/`edk2-ovmf` when
  `guest_firmware`, package-artifact caching to `/var/cache/deploytix/xen/`.
- **[verify] and likely patch**: the AUR PKGBUILD's systemd assumptions. Xen's
  configure has an `--enable-systemd` path that links `libsystemd`, which does
  not exist on Artix. If the PKGBUILD does not auto-disable it, Deploytix needs
  to build with `--disable-systemd`, which may mean carrying a small PKGBUILD
  override the way `pkg/PKGBUILD` already carries packaging for deploytix
  itself. **This is the single biggest unknown in the plan and should be
  resolved before WP3 is scheduled.**

### WP4 — Services
- `write_xen_services()` in the four-arm pattern of `write_hhd_service()`.
- `build_service_list()` / `build_service_packages()` wiring plus the
  `{service}-{init}` exemption.
- Readiness polling for `xen-init-dom0` (V.1).
- Tests asserting a definition is written for each init, and that no
  `xenstored-<init>` package is ever requested.

### WP5 — System configuration
- fstab xenfs entry; `/etc/modules-load.d/xen-dom0.conf`;
  `/var/lib/xen/{configs,auto,images}`; `/etc/xen/auto` symlink;
  `xen-pciback` in `construct_modules()` + `xen-pciback.hide=` on the dom0
  cmdline; removal of `xend.rules`.

### WP6 — Networking
- NM keyfile bridge profile; `xen-bridge` service for the non-NM case; NAT +
  `ip_forward` sysctl drop-in for iwd hosts.

### WP7 — Immutability integration
- Pin the Xen menu entry to `/xen.gz` (IV.4), preferably via a Deploytix-owned
  `/etc/grub.d/09_deploytix_xen` generator with deterministic output.
- Verify `activate_target`'s scratch-chroot `grub-mkconfig` regenerates Xen
  entries correctly on the btrfs backend.
- Document and test the `deploytix update <cached-xen-pkg>` path (IV.6).
- Post-install summary text covering IV.3 and IV.5.

### WP8 — Installer wiring
- Phase 5.9 per Part VIII, including the `REINSTALL_GRUB_PATH`-aware
  regeneration.
- `print_enrollment_instructions()` gains the SecureBoot-chain caveat (III.4).

### WP9 — Documentation
- `docs/XEN_DOM0.md` — user-facing: what was installed, where to put domU
  configs and images, how to update Xen on an immutable system, what the
  SecureBoot chain does and does not guarantee, how to boot without the
  hypervisor for recovery.
- `CLAUDE.md` — a short "Xen dom0" subsection under the transactional-immutable
  heading.

---

## Part XI — Test matrix

Dry-run and unit tests can cover most of this; the rest needs hardware or nested
virtualisation.

**Unit / dry-run**

| Case | Asserts |
|---|---|
| `multiboot2` in standalone module list | III.3 |
| Xen cmdline emitted by each of the three `configure_grub_defaults*` | III.2 |
| btrfs pointer sed matches a `module2` line | III.2 |
| A/B slot/roothash sed matches a `module2` line | III.2 |
| Four init service definitions written | V.2 |
| No `xenstored-<init>` package requested | V.3 |
| Every Part IX validation rule | IX |
| `/etc/xen/auto` is a symlink into `/var/lib/xen` | IV.3 |
| Xen menu entry names `/xen.gz`, never a versioned file | IV.4 |

**Integration (VM / nested)**

| Case | Layout |
|---|---|
| Plain install + Xen | ext4, no encryption, no SecureBoot |
| Encrypted + Xen | multi-LUKS btrfs, encrypted `/boot` |
| SecureBoot + encrypted + Xen | sbctl standalone GRUB — the case WP2 exists for |
| Immutable btrfs + Xen | `immutable_root` + `install_grub_btrfs`; verify update + rollback keeps VMs |
| Immutable A/B + Xen | `immutable_root` + `use_lvm_thin`; verify the frozen grub.cfg survives a Xen version bump |
| Xen update transactionally | `deploytix update <cached pkg>` on both backends |
| Recovery | boot the non-Xen entry; boot a grub-btrfs snapshot (expect no hypervisor) |

Each init system should get at least one pass; runit and dinit are the ones where
the `xen-init-dom0` ordering is most likely to break.

---

## Part XII — Risks, open questions, out of scope

**Risks**

1. **AUR `xen` on Artix may not build unmodified** (systemd linkage, WP3). Highest
   unknown; resolve first.
2. **Compile time.** 30–90 minutes added to an install. The wizard must say so
   before the user commits, the way long steps are flagged elsewhere.
3. **`09_xen` vs `20_linux_xen`.** Generator name and ordering determine whether
   `GRUB_DEFAULT=0` means Xen. WP7's own generator removes this dependency.
4. **The frozen-grub.cfg hazard (IV.4)** is the one that bricks machines rather
   than merely failing an install. Pin to `/xen.gz` and test the version-bump
   case explicitly.
5. **SecureBoot expectations.** Users will reasonably assume SecureBoot + Xen
   means a verified chain. It does not (III.4). Say so in the summary output, not
   only in docs.

**Open questions — all marked [verify] above**

- Does the AUR package ship `/etc/grub.d/09_xen`, `/etc/xen/grub.conf`, a
  `/boot/xen.gz` symlink, SysV init scripts, `/etc/xen/auto`, `xend.rules`?
- Is `xen` present in any Artix repo (galaxy), which would remove the `yay`
  dependency entirely and simplify WP3 and IV.6 substantially?
- Is Artix's GRUB built with the shim-lock verifier (decides whether the Shim
  restriction is an error or a warning)?
- `ovmf` vs `edk2-ovmf` package naming.

**Out of scope for the first implementation**

- domU provisioning (Deploytix installs a hypervisor; it does not create guests).
- PVH dom0.
- `xl` GUI management, or GUI wizard panels beyond a single checkbox.
- ARM / non-x86_64 dom0.
- Live migration, clustering, XAPI/XCP-ng toolstacks.
- `deploytix update --xen` convenience wrapper (noted as a follow-up in IV.6).

---

## Appendix A — Reference: files this feature touches

| File | Change |
|---|---|
| `src/config/deployment.rs` | schema, defaults, wizard, validation |
| `src/configure/boot/bootloader.rs` | `multiboot2` module, Xen cmdline, reinstall-hook targets |
| `src/install/packages.rs` | `install_xen()`, `write_xen_services()` |
| `src/configure/system/services.rs` | service list + package-lookup exemption |
| `src/configure/system/mkinitcpio.rs` | `xen-pciback` when passthrough is configured |
| `src/configure/system/network.rs` | bridge profile / NAT |
| `src/configure/crypto/secureboot.rs` | sign `xen*.efi`, hook target, enrollment caveat |
| `src/install/fstab.rs` | xenfs entry |
| `src/install/installer.rs` | Phase 5.9 |
| `src/immutable/lvm_ab.rs` | only if mitigation (3) of IV.4 is needed |
| `docs/XEN_DOM0.md` | new user-facing guide |
| `CLAUDE.md` | Xen subsection |

## Appendix B — Boot chain, side by side

```
Today (encrypted + SecureBoot + immutable A/B):

  firmware ──verify──> BOOTX64.EFI (standalone GRUB, sbctl-signed)
                       │  cryptomount (LUKS2)
                       └─ linux  /vmlinuz   root=/dev/mapper/deploytix_root
                          initrd /initramfs    deploytix.slot=A
                                               deploytix.roothash=<hash>
                          └─ verity-ab hook ──verify──> dm-verity root (ro)

With Xen (Mode A):

  firmware ──verify──> BOOTX64.EFI (standalone GRUB, sbctl-signed)
                       │  cryptomount (LUKS2)
                       ├─ multiboot2 /xen.gz   dom0_mem=4G,max:4G dom0_max_vcpus=4
                       │                       dom0_vcpus_pin ucode=scan iommu=1
                       ├─ module2 /vmlinuz     root=/dev/mapper/deploytix_root
                       │                       deploytix.slot=A
                       │                       deploytix.roothash=<hash>
                       └─ module2 /initramfs
                          └─ verity-ab hook ──verify──> dm-verity root (ro)

  ^ the hypervisor hop is NOT signature-verified (III.4);
    the root IS still integrity-verified by dm-verity.
```
