# Artix Linux (runit) + GRUB + mkinitcpio + LUKS + btrfs Subvolumes Install Spec

## Boot Sequence

1. **GRUB** loads **kernel** and **initramfs**
2. **initramfs** sets up the disk and then passes control.  
   `mkinitcpio` hooks execute in order:
   - **crypttab-unlock**
     - Parse `/etc/crypttab`
     - Wait for devices (UUID resolution)
     - `cryptsetup open` → `/dev/mapper/Crypt-{Name}`
   - **mountcrypt**
     - Wait for mapped devices
     - Mount root (`Crypt-Root`) to `/new_root`
     - Mount subvolumes (`@usr`, `@var`, `@home`, `@boot`)
     - Auto-detect and mount EFI partition
3. **switch_root** to `/new_root`
4. Kernel starts userland
   - **runit** inits the system
5. runit system services start
6. **seatd** service starts (seat management)
7. **greetd** service starts and launches KDE Plasma

---

## Proposed Configuration Set (Inputs)

Lock these **before** generating files:

- Bootloader: **GRUB (EFI)**
- Initramfs: **mkinitcpio**
- Init: **runit**
- Encryption: **LUKS** for root container
- Filesystem: **btrfs** inside encrypted root
- btrfs subvolumes:
  - `@`    → `/`
  - `@usr` → `/usr`
  - `@var` → `/var`
  - `@home`→ `/home`
  - `@boot`→ `/boot` (encrypted; EFI is separate and mounted at `/boot/efi`)
- Services directory: `/etc/runit/sv/`
- Enabled services symlink directory: `/run/runit/service/`

**Placeholders you must fill:**
- `<LUKS-ROOT-UUID>`: UUID of the LUKS container that holds btrfs root
- `<USERNAME>`: user to launch plasma as (for greetd)

---

## Stage A — Disk + Filesystem Construction (Live Environment)

### A1) Partitioning (example: GPT + EFI + LUKS)
- Create:
  - EFI System Partition (ESP): ~512MiB, type `EF00`, vfat
  - Root partition: remaining space, will be LUKS

### A2) Format EFI
```sh
mkfs.vfat -F32 -n EFI /dev/<esp-partition>
```

### A3) Create and open LUKS container
```sh
cryptsetup luksFormat /dev/<root-partition>
cryptsetup open /dev/<root-partition> Crypt-Root
```

### A4) Create btrfs and subvolumes
```sh
mkfs.btrfs -L ROOT /dev/mapper/Crypt-Root
mount /dev/mapper/Crypt-Root /mnt

btrfs subvolume create /mnt/@
btrfs subvolume create /mnt/@usr
btrfs subvolume create /mnt/@var
btrfs subvolume create /mnt/@home
btrfs subvolume create /mnt/@boot

umount /mnt
```

### A5) Mount target layout
```sh
# root
mount -o subvol=@ /dev/mapper/Crypt-Root /mnt

# subvolumes
mkdir -p /mnt/{usr,var,home,boot}
mount -o subvol=@usr  /dev/mapper/Crypt-Root /mnt/usr
mount -o subvol=@var  /dev/mapper/Crypt-Root /mnt/var
mount -o subvol=@home /dev/mapper/Crypt-Root /mnt/home
mount -o subvol=@boot /dev/mapper/Crypt-Root /mnt/boot

# EFI
mkdir -p /mnt/boot/efi
mount /dev/<esp-partition> /mnt/boot/efi
```

---

## Stage B — Base System Install (Artix + runit)

Install base system, kernel, firmware, and required tools. (Exact packages may vary by profile.)

Minimum must-haves for this design:
- `runit`, `runit-rc` (if using rc-style), or your chosen runit setup
- `mkinitcpio`
- `cryptsetup`
- `btrfs-progs`
- `grub`, `efibootmgr`
- `seatd` + `seatd-runit` (preferred)
- `greetd` + `greetd-runit` (preferred)
- KDE Plasma stack (whatever you use)

---

## Stage C — Pre-populated Configuration Files

### C1) `/etc/crypttab`
Authoritative mapping definition used by initramfs hook:
```text
Crypt-Root UUID=<LUKS-ROOT-UUID> none luks,discard
```

### C2) `/etc/fstab` (btrfs subvolumes + EFI)
Use UUIDs. Example structure (fill in real UUIDs):
```text
# Encrypted btrfs root (mapped as /dev/mapper/Crypt-Root in initramfs)
UUID=<BTRFS-FS-UUID>  /         btrfs  subvol=@,defaults,noatime  0 0
UUID=<BTRFS-FS-UUID>  /usr      btrfs  subvol=@usr,defaults,noatime 0 0
UUID=<BTRFS-FS-UUID>  /var      btrfs  subvol=@var,defaults,noatime 0 0
UUID=<BTRFS-FS-UUID>  /home     btrfs  subvol=@home,defaults,noatime 0 0
UUID=<BTRFS-FS-UUID>  /boot     btrfs  subvol=@boot,defaults,noatime 0 0

# EFI
UUID=<EFI-UUID>       /boot/efi vfat   umask=0077,defaults        0 2
```

### C3) mkinitcpio hook ordering: `/etc/mkinitcpio.conf`
Ensure custom hooks are **before** filesystems:
```text
HOOKS=(base udev autodetect modconf block keyboard crypttab-unlock mountcrypt filesystems)
```

---

## Stage D — Custom mkinitcpio Hooks

> Create both the hook (runtime) and install file (adds binaries/config into initramfs).

### D1) `crypttab-unlock`
**Files:**
- `/usr/lib/initcpio/hooks/crypttab-unlock`
- `/usr/lib/initcpio/install/crypttab-unlock`

**Responsibilities:**
- Read `/etc/crypttab`
- Wait for `/dev/disk/by-uuid/<UUID>` to appear
- `cryptsetup open <dev> <name>`

### D2) `mountcrypt`
**Files:**
- `/usr/lib/initcpio/hooks/mountcrypt`
- `/usr/lib/initcpio/install/mountcrypt`

**Responsibilities:**
- Wait for `/dev/mapper/Crypt-Root`
- Mount root to `/new_root`
- Mount subvolumes
- Detect EFI (vfat with ESP PARTTYPE) and mount to `/new_root/boot/efi`

### D3) Build initramfs
```sh
mkinitcpio -P
```

---

## Stage E — GRUB (EFI)

### E1) `/etc/default/grub`
Pre-populate kernel args required for handoff:

```text
GRUB_CMDLINE_LINUX_DEFAULT="quiet cryptdevice=UUID=<LUKS-ROOT-UUID>:Crypt-Root root=/dev/mapper/Crypt-Root rootflags=subvol=@ rw"
GRUB_ENABLE_CRYPTODISK=y
```

### E2) Install and generate GRUB config
```sh
grub-install --target=x86_64-efi --efi-directory=/boot/efi --bootloader-id=Artix
grub-mkconfig -o /boot/grub/grub.cfg
```

---

## Stage F — runit Services

### F1) Rule: prefer `*-runit` service packages when available
Example:
```sh
sudo pacman -Sy seatd seatd-runit
sudo pacman -Sy greetd greetd-runit
```

### F2) For packages without `-runit`, construct services
Service skeleton:
```text
/etc/runit/sv/<service>/
├── run
└── log/
    └── run
```

`/etc/runit/sv/<service>/run` template:
```sh
#!/bin/sh
exec 2>&1
exec <binary> <args>
```

Enable a service by symlinking into supervision dir:
```sh
ln -s /etc/runit/sv/<service> /run/runit/service/<service>
```

> Note: Some Artix setups use `/run/runit/service/` directly; keep your installer consistent with the target system’s runit layout.

---

## Stage G — seatd + greetd + Plasma

### G1) greetd config: `/etc/greetd/config.toml`
Wayland example:
```toml
[terminal]
vt = 1

[default_session]
command = "startplasma-wayland"
user = "<USERNAME>"
```

X11 example:
```toml
[terminal]
vt = 1

[default_session]
command = "startplasma-x11"
user = "<USERNAME>"
```

---

## End-State Validation Checklist

- GRUB loads kernel + initramfs from `/boot` (encrypted) and ESP at `/boot/efi`
- initramfs:
  - Parses `/etc/crypttab`
  - Unlocks root → `/dev/mapper/Crypt-Root`
  - Mounts btrfs `@` → `/new_root`, plus subvolumes
  - Mounts EFI → `/new_root/boot/efi`
  - `switch_root` succeeds
- runit reaches stage 2
- seatd starts before greetd
- greetd launches plasma session

---

**This file is intended to be used as input for an installer that pre-populates and writes the configuration files exactly as above.**
