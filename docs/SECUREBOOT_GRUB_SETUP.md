# Secure Boot Setup with GRUB and sbctl

Guide for setting up Secure Boot on Arch/Artix Linux with GRUB and LUKS encryption using sbctl.

## Prerequisites

- UEFI system with Secure Boot capability
- `sbctl` package installed
- `grub` package installed
- EFI partition mounted (e.g., `/boot/efi`)

## 1. Create Secure Boot Keys

```bash
sudo sbctl create-keys
```

Keys are stored in `/var/lib/sbctl/keys/`.

## 2. Generate GRUB Config

```bash
sudo grub-mkconfig -o /boot/grub/grub.cfg
```

## 3. Create Standalone GRUB (Recommended)

For Secure Boot with LUKS encryption, use `grub-mkstandalone` to create a self-contained EFI binary with all modules and config embedded:

```bash
sudo grub-mkstandalone \
    --format=x86_64-efi \
    --output=/boot/efi/EFI/BOOT/BOOTX64.EFI \
    --disable-shim-lock \
    --modules="all_video boot btrfs cat chain configfile echo efifwsetup efinet ext2 fat font gettext gfxmenu gfxterm gfxterm_background gzio halt help hfsplus iso9660 jpeg keystatus loadenv loopback linux ls lsefi lsefimmap lsefisystab lssal memdisk minicmd normal ntfs part_apple part_msdos part_gpt password_pbkdf2 png probe reboot regexp search search_fs_uuid search_fs_file search_label sleep smbios squash4 test true video xfs zstd cryptodisk luks luks2 gcry_rijndael gcry_sha256 gcry_sha512" \
    "boot/grub/grub.cfg=/boot/grub/grub.cfg"
```

**Why standalone?**
- All modules embedded (no external module loading that triggers verification errors)
- Config embedded in a memdisk inside the EFI binary
- Single signed binary - no verification chain issues
- `--disable-shim-lock`: Required when using sbctl's own keys (not shim)

### Alternative: grub-install (may cause verification errors)

```bash
sudo grub-install --target=x86_64-efi \
    --efi-directory=/boot/efi \
    --bootloader-id=GRUB \
    --disable-shim-lock \
    --modules="all_video boot btrfs cat chain configfile echo efifwsetup efinet ext2 fat font gettext gfxmenu gfxterm gfxterm_background gzio halt help hfsplus iso9660 jpeg keystatus loadenv loopback linux ls lsefi lsefimmap lsefisystab lssal memdisk minicmd normal ntfs part_apple part_msdos part_gpt password_pbkdf2 png probe reboot regexp search search_fs_uuid search_fs_file search_label sleep smbios squash4 test true video xfs zstd cryptodisk luks luks2 gcry_rijndael gcry_sha256 gcry_sha512"
```

**Note:** This method may still produce "verification requested but nobody cares" errors when loading the kernel, as GRUB's internal verifier checks loaded files.

## 4. Sign EFI Binaries and Kernel

Sign and add to sbctl's database with `-s`:

```bash
# Sign standalone GRUB bootloader
sudo sbctl sign -s /boot/efi/EFI/BOOT/BOOTX64.EFI

# Sign kernel
sudo sbctl sign -s /boot/vmlinuz-linux
```

Verify signatures:

```bash
sudo sbctl verify
sudo sbctl list-files
```

## 5. Enroll Keys

Remove immutable attribute from EFI variables and enroll:

```bash
sudo chattr -i /sys/firmware/efi/efivars/{PK,KEK,db}-*
sudo sbctl enroll-keys --microsoft
```

The `--microsoft` flag includes Microsoft's keys for compatibility with some hardware/firmware.

Check status:

```bash
sudo sbctl status
```

## 6. Manage EFI Boot Entries

List current boot entries:

```bash
sudo efibootmgr
```

Create a new boot entry for the standalone GRUB:

```bash
sudo efibootmgr --create \
    --disk /dev/nvme0n1 \
    --part 1 \
    --label "Artix-SB" \
    --loader "\EFI\BOOT\BOOTX64.EFI"
```

Set boot order (replace XXXX with entry numbers):

```bash
sudo efibootmgr -o XXXX,YYYY,ZZZZ
```

Delete old/duplicate entries:

```bash
sudo efibootmgr -b XXXX -B
```

Clean up old GRUB directory if using standalone:

```bash
sudo rm -rf /boot/efi/EFI/GRUB
sudo sbctl remove-file /boot/efi/EFI/GRUB/grubx64.efi
```

## 7. Enable Secure Boot

1. Reboot the system
2. Enter UEFI/BIOS settings
3. Enable Secure Boot
4. Boot into Linux

Verify with:

```bash
sudo sbctl status
# Should show:
# Setup Mode:    ✗ Disabled
# Secure Boot:   ✓ Enabled
```

## Troubleshooting

### Error: `shim_lock_verifier_init:177:prohibited by secure boot policy`

GRUB was installed without `--disable-shim-lock`. Reinstall GRUB with the flag.

### Error: `verification requested but nobody cares ... normal.mod`

GRUB modules aren't embedded. Reinstall GRUB with `--modules="..."` to embed required modules.

### Error: `verification requested but nobody cares: /vmlinuz-*`

GRUB's internal verifier is checking the kernel but can't validate it. This happens even with `--disable-shim-lock` when using `grub-install`.

**Solution:** Use `grub-mkstandalone` instead (see Section 3). The standalone GRUB doesn't trigger internal verification for loaded files.

### Error: `File is immutable`

Run before enrolling keys:

```bash
sudo chattr -i /sys/firmware/efi/efivars/{PK,KEK,db}-*
```

### sbctl says "File has already been signed" but won't add to database

Manually edit `/var/lib/sbctl/files.json` to add the file entry:

```json
{
    "/path/to/file.efi": {
        "file": "/path/to/file.efi",
        "output_file": "/path/to/file.efi"
    }
}
```

### Kernel updates break Secure Boot

Ensure the sbctl pacman hook is installed (`/usr/share/libalpm/hooks/zz-sbctl.hook`). It automatically re-signs files in the database after updates.

## Pacman Hooks

sbctl installs a hook at `/usr/share/libalpm/hooks/zz-sbctl.hook` that runs `sbctl sign-all -g` after package updates affecting boot files.

## File Locations

- sbctl keys: `/var/lib/sbctl/keys/`
- sbctl file database: `/var/lib/sbctl/files.json`
- EFI binaries: `/boot/efi/EFI/`
- GRUB config: `/boot/grub/grub.cfg`
- Kernel cmdline (for UKI): `/etc/kernel/cmdline`

## Quick Reference

```bash
# Check status
sudo sbctl status
sudo sbctl verify
sudo sbctl list-files
sudo efibootmgr

# Re-sign all after updates
sudo sbctl sign-all

# Re-enroll keys if needed
sudo chattr -i /sys/firmware/efi/efivars/{PK,KEK,db}-*
sudo sbctl enroll-keys --microsoft

# Rebuild standalone GRUB after grub.cfg changes
sudo grub-mkconfig -o /boot/grub/grub.cfg
sudo grub-mkstandalone \
    --format=x86_64-efi \
    --output=/boot/efi/EFI/BOOT/BOOTX64.EFI \
    --disable-shim-lock \
    --modules="all_video boot btrfs cat chain configfile echo efifwsetup efinet ext2 fat font gettext gfxmenu gfxterm gfxterm_background gzio halt help hfsplus iso9660 jpeg keystatus loadenv loopback linux ls lsefi lsefimmap lsefisystab lssal memdisk minicmd normal ntfs part_apple part_msdos part_gpt password_pbkdf2 png probe reboot regexp search search_fs_uuid search_fs_file search_label sleep smbios squash4 test true video xfs zstd cryptodisk luks luks2 gcry_rijndael gcry_sha256 gcry_sha512" \
    "boot/grub/grub.cfg=/boot/grub/grub.cfg"
sudo sbctl sign-all
```
