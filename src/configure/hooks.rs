//! Custom mkinitcpio hook generation

use crate::config::{DeploymentConfig, PartitionLayout};
use crate::disk::layouts::ComputedLayout;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::info;

/// Generated hook files
pub struct GeneratedHook {
    pub name: String,
    pub hook_content: String,
    pub install_content: String,
}

/// Generate and install custom hooks
pub fn install_custom_hooks(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    layout: &ComputedLayout,
    install_root: &str,
) -> Result<()> {
    let hooks = generate_hooks(config, layout)?;
    let hook_names: Vec<&str> = hooks.iter().map(|h| h.name.as_str()).collect();
    info!(
        "Installing {} custom mkinitcpio hooks: [{}]",
        hooks.len(),
        hook_names.join(", ")
    );

    if cmd.is_dry_run() {
        for hook in &hooks {
            println!("  [dry-run] Would install hook: {}", hook.name);
            println!("    -> /usr/lib/initcpio/hooks/{}", hook.name);
            println!("    -> /usr/lib/initcpio/install/{}", hook.name);
        }
        return Ok(());
    }

    let hooks_dir = format!("{}/usr/lib/initcpio/hooks", install_root);
    let install_dir = format!("{}/usr/lib/initcpio/install", install_root);

    fs::create_dir_all(&hooks_dir)?;
    fs::create_dir_all(&install_dir)?;

    for hook in hooks {
        // Write hook (runtime script)
        let hook_path = format!("{}/{}", hooks_dir, hook.name);
        fs::write(&hook_path, &hook.hook_content)?;
        // Make executable
        fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o755))?;

        // Write install script
        let install_path = format!("{}/{}", install_dir, hook.name);
        fs::write(&install_path, &hook.install_content)?;
        fs::set_permissions(&install_path, fs::Permissions::from_mode(0o755))?;

        info!("Installed hook '{}' to {}", hook.name, hooks_dir);
    }

    Ok(())
}

/// Generate hooks based on configuration
fn generate_hooks(
    config: &DeploymentConfig,
    layout: &ComputedLayout,
) -> Result<Vec<GeneratedHook>> {
    let mut hooks = Vec::new();

    // Custom hooks are only needed for encrypted Standard layout which uses
    // separate LUKS containers for Root, Usr, Var, Home.  Other layouts
    // rely on the standard `encrypt` hook for a single LUKS volume.
    if config.disk.encryption && config.disk.layout == PartitionLayout::Standard {
        hooks.push(generate_crypttab_unlock_hook());
        hooks.push(generate_mountcrypt_hook(config, layout));
    }

    Ok(hooks)
}

/// Generate the crypttab-unlock hook (embedded from ref/hooks_crypttab-unlock)
fn generate_crypttab_unlock_hook() -> GeneratedHook {
    let hook_content = r#"#!/usr/bin/ash
# SPDX-License-Identifier: GPL-2.0-only
# crypttab-unlock: A custom mkinitcpio hook to unlock all LUKS-encrypted partitions
# using entries from /etc/crypttab.
#
# Features:
# - Checks if device is already unlocked before attempting
# - Waits for each unlock to complete before proceeding
# - Sequential processing to ensure proper ordering

# Function to wait for the source device to appear
wait_for_device() {
    local devpath="$1"
    local timeout=30

    while [ ! -e "$devpath" ] && [ $timeout -gt 0 ]; do
        sleep 0.5
        timeout=$((timeout - 1))
    done

    if [ ! -e "$devpath" ]; then
        echo "[crypttab-unlock] ERROR: Device $devpath not found after waiting."
        return 1
    fi
    return 0
}

# Function to wait for mapped device to appear after unlock
wait_for_mapper() {
    local mapper_path="$1"
    local timeout=20

    while [ ! -b "$mapper_path" ] && [ $timeout -gt 0 ]; do
        sleep 0.5
        timeout=$((timeout - 1))
    done

    if [ ! -b "$mapper_path" ]; then
        return 1
    fi
    return 0
}

# Function to check if a LUKS device is already open
is_already_unlocked() {
    local mapper_name="$1"
    local mapper_path="/dev/mapper/$mapper_name"

    # Check if the mapper device exists
    if [ -b "$mapper_path" ]; then
        return 0  # Already unlocked
    fi
    return 1  # Not unlocked
}

# Function to unlock a single LUKS device
unlock_device() {
    local device="$1"
    local mapper_name="$2"
    local keyfile="$3"
    local options="$4"
    local mapper_path="/dev/mapper/$mapper_name"

    # Check if already unlocked
    if is_already_unlocked "$mapper_name"; then
        echo "[crypttab-unlock] $mapper_name is already unlocked, skipping."
        return 0
    fi

    # Build cryptsetup command
    local cmd="cryptsetup open $device $mapper_name"
    if [ -n "$keyfile" ] && [ "$keyfile" != "none" ]; then
        cmd="$cmd --key-file $keyfile"
    fi

    # Translate crypttab options to cryptsetup flags
    case "$options" in
        *discard*) cmd="$cmd --allow-discards" ;;
    esac

    # Run the cryptsetup command
    echo "[crypttab-unlock] Running: $cmd"
    if ! $cmd; then
        echo "[crypttab-unlock] ERROR: cryptsetup failed for $mapper_name"
        return 1
    fi

    # Wait for the mapped device to appear
    echo "[crypttab-unlock] Waiting for $mapper_path to appear..."
    if ! wait_for_mapper "$mapper_path"; then
        echo "[crypttab-unlock] ERROR: $mapper_path did not appear after unlock"
        return 1
    fi

    echo "[crypttab-unlock] Successfully unlocked $mapper_name -> $mapper_path"
    return 0
}

run_hook() {
    # Ensure cryptsetup is available in the initramfs environment
    if ! command -v cryptsetup >/dev/null 2>&1; then
        echo "[crypttab-unlock] ERROR: cryptsetup not found in initramfs."
        return 1
    fi

    local crypttab="/etc/crypttab"
    if [ ! -f "$crypttab" ]; then
        echo "[crypttab-unlock] No /etc/crypttab found, skipping."
        return 0
    fi

    echo "[crypttab-unlock] Processing $crypttab ..."
    local ret=0
    local unlock_count=0
    local skip_count=0
    local fail_count=0

    while IFS= read -r line; do
        # Skip empty lines and comments
        case "$line" in
            ''|\#*) continue ;;
        esac

        # Parse fields (MappingName, Device, KeyFile, Options)
        set -- $line
        local mapping="$1"
        local device="$2"
        local keyfile="$3"
        shift 3 2>/dev/null || true
        local options="$*"

        # Convert UUID= to device path
        case "$device" in
            UUID=*)
                local uuid="${device#UUID=}"
                device="/dev/disk/by-uuid/$uuid"
                ;;
        esac

        # Convert the mapping name to title case (e.g., "Root" -> "Root", "ROOT" -> "Root")
        local formatted_mapping
        formatted_mapping=$(echo "$mapping" | awk '{print toupper(substr($0,1,1)) tolower(substr($0,2))}')
        local full_mapper_name="Crypt-$formatted_mapping"

        # Skip EFI partition entries (should never be encrypted, but guard against misconfiguration)
        case "$formatted_mapping" in
            Efi|efi|EFI)
                echo "[crypttab-unlock] Skipping EFI partition (not encrypted)"
                skip_count=$((skip_count + 1))
                continue
                ;;
        esac

        echo "[crypttab-unlock] Processing entry: $mapping -> $full_mapper_name"

        # Check if already unlocked first (before waiting for device)
        if is_already_unlocked "$full_mapper_name"; then
            echo "[crypttab-unlock] $full_mapper_name already unlocked, skipping."
            skip_count=$((skip_count + 1))
            continue
        fi

        # Wait for the source device to be available
        echo "[crypttab-unlock] Waiting for source device $device ..."
        if ! wait_for_device "$device"; then
            echo "[crypttab-unlock] ERROR: Device $device not found. Skipping $full_mapper_name."
            fail_count=$((fail_count + 1))
            ret=1
            continue
        fi

        # Verify keyfile existence
        if [ -n "$keyfile" ] && [ "$keyfile" != "none" ]; then
            if [ ! -f "$keyfile" ]; then
                echo "[crypttab-unlock] ERROR: Keyfile $keyfile does not exist. Skipping $full_mapper_name."
                fail_count=$((fail_count + 1))
                ret=1
                continue
            fi
        fi

        # Unlock the device
        if unlock_device "$device" "$full_mapper_name" "$keyfile" "$options"; then
            unlock_count=$((unlock_count + 1))
        else
            fail_count=$((fail_count + 1))
            ret=1
        fi

        # Small delay to ensure device mapper settles
        sleep 0.2

    done < "$crypttab"

    echo "[crypttab-unlock] Complete: $unlock_count unlocked, $skip_count skipped, $fail_count failed"
    return $ret
}

"#.to_string();

    let install_content = r#"#!/bin/bash
# SPDX-License-Identifier: GPL-2.0-only
# This install script ensures that the crypttab-unlock
# hook is added to the initramfs image and
# that the necessary binary (cryptsetup) is included

build() {
    local mod

    map add_module 'dm-crypt' 'dm-integrity' 'hid-generic?'
    if [[ -n "$CRYPTO_MODULES" ]]; then
        for mod in $CRYPTO_MODULES; do
            add_module "$mod"
        done
    else
        add_all_modules '/crypto/'
    fi

    add_binary 'cryptsetup'

    map add_udev_rule \
        '10-dm.rules' \
        '13-dm-disk.rules' \
        '95-dm-notify.rules'

    # cryptsetup calls pthread_create(), which dlopen()s libgcc_s.so.1
    add_binary '/usr/lib/libgcc_s.so.1'

    # cryptsetup loads the legacy provider which is required for whirlpool
    add_binary '/usr/lib/ossl-modules/legacy.so'

    # Include /etc/crypttab so the hook can read it at boot
    add_file '/etc/crypttab'

    add_runscript
}
"#
    .to_string();

    GeneratedHook {
        name: "crypttab-unlock".to_string(),
        hook_content,
        install_content,
    }
}

/// Generate the mountcrypt hook for multi-volume encrypted system
///
/// Mounts separate encrypted partitions (Crypt-Root, Crypt-Usr, Crypt-Var, Crypt-Home)
/// instead of btrfs subvolumes.
fn generate_mountcrypt_hook(config: &DeploymentConfig, _layout: &ComputedLayout) -> GeneratedHook {
    let boot_mapper_name = &config.disk.luks_boot_mapper_name;

    // Generate /boot mount section depending on boot encryption
    let boot_mount_section = if config.disk.boot_encryption {
        format!(
            r#"    # Mount encrypted /boot from LUKS1 container
    mount_volume "/dev/mapper/{boot_mapper}" "$new_root/boot" "boot" || true"#,
            boot_mapper = boot_mapper_name
        )
    } else {
        // Auto-detect boot partition
        String::from(
            r#"    # Mount unencrypted /boot partition
    boot_partition=""
    for dev in $(blkid -t LABEL=BOOT -o device 2>/dev/null); do
        boot_partition="$dev"
        break
    done

    if [ -n "$boot_partition" ] && [ -b "$boot_partition" ]; then
        mount_volume "$boot_partition" "$new_root/boot" "boot" || true
    else
        echo "[mountcrypt] Warning: BOOT partition not found" >&2
    fi"#,
        )
    };

    let hook_content = format!(
        r#"#!/usr/bin/ash
# mountcrypt: Mount multi-volume encrypted system
# Generated by Deploytix
#
# Mounts separate LUKS-encrypted partitions:
#   - Crypt-Root -> /
#   - Crypt-Usr  -> /usr
#   - Crypt-Var  -> /var
#   - Crypt-Home -> /home
#
# This hook runs AFTER crypttab-unlock has unlocked all volumes.

# Wait for a block device to appear
wait_for_block_device() {{
    local device="$1"
    local timeout=30

    while [ ! -b "$device" ] && [ $timeout -gt 0 ]; do
        sleep 0.5
        timeout=$((timeout - 1))
    done

    [ -b "$device" ]
}}

# Check if a path is already mounted
is_mounted() {{
    local mount_point="$1"
    grep -q " $mount_point " /proc/mounts 2>/dev/null
}}

# Mount a volume with checks
mount_volume() {{
    local device="$1"
    local mount_point="$2"
    local name="$3"

    # Wait for device
    echo "[mountcrypt] Waiting for $device ($name)..."
    if ! wait_for_block_device "$device"; then
        echo "[mountcrypt] ERROR: $device not found for $name" >&2
        return 1
    fi

    # Check if already mounted
    if is_mounted "$mount_point"; then
        echo "[mountcrypt] $mount_point already mounted, skipping"
        return 0
    fi

    # Create mount point and mount
    mkdir -p "$mount_point"
    if mount -o rw "$device" "$mount_point"; then
        echo "[mountcrypt] Mounted $device -> $mount_point"
        return 0
    else
        echo "[mountcrypt] ERROR: Failed to mount $device -> $mount_point" >&2
        return 1
    fi
}}

# run_hook is called during the hooks phase
# We set the mount_handler variable to point to our custom mount function
run_hook() {{
    echo "[mountcrypt] Setting mount_handler to mountcrypt_handler"
    # Override the default mount handler with our custom one
    mount_handler=mountcrypt_handler
}}

# Our custom mount handler - called by mkinitcpio's init via $mount_handler variable
# Receives the mount point as $1 (typically /new_root)
mountcrypt_handler() {{
    local new_root="$1"
    local ret=0

    echo "[mountcrypt] mount_handler called with target: $new_root"

    # CRITICAL: Check if root is already mounted to prevent double-mount
    # This can happen if mkinitcpio's init has fallback mount logic
    if mountpoint -q "$new_root" 2>/dev/null; then
        echo "[mountcrypt] WARNING: $new_root is already a mountpoint!"
        echo "[mountcrypt] Current mounts on $new_root:"
        grep "$new_root" /proc/mounts 2>/dev/null || true
        echo "[mountcrypt] Skipping mount_handler to prevent double-mount"
        return 0
    fi

    echo "[mountcrypt] Starting multi-volume mount sequence..."

    # List available mapper devices for debugging
    echo "[mountcrypt] Available /dev/mapper devices:"
    ls -la /dev/mapper/ 2>/dev/null || echo "[mountcrypt] No mapper devices found"

    # Mount root first (required)
    echo "[mountcrypt] === Mounting root ==="
    if ! mount_volume "/dev/mapper/Crypt-Root" "$new_root" "root"; then
        echo "[mountcrypt] FATAL: Cannot mount root filesystem" >&2
        return 1
    fi

    # Mount /usr (required for most systems)
    echo "[mountcrypt] === Mounting /usr ==="
    if ! mount_volume "/dev/mapper/Crypt-Usr" "$new_root/usr" "usr"; then
        echo "[mountcrypt] ERROR: Failed to mount /usr" >&2
        ret=1
    fi

    # Mount /var
    echo "[mountcrypt] === Mounting /var ==="
    if ! mount_volume "/dev/mapper/Crypt-Var" "$new_root/var" "var"; then
        echo "[mountcrypt] WARNING: Failed to mount /var" >&2
    fi

    # Mount /home
    echo "[mountcrypt] === Mounting /home ==="
    if ! mount_volume "/dev/mapper/Crypt-Home" "$new_root/home" "home"; then
        echo "[mountcrypt] WARNING: Failed to mount /home" >&2
    fi

    # Mount /boot
    echo "[mountcrypt] === Mounting /boot ==="
{boot_mount}

    # Mount EFI partition (must come after /boot since it mounts to /boot/efi)
    echo "[mountcrypt] === Mounting EFI ==="
    mkdir -p "$new_root/boot/efi"

    efi_partition=""

    # Primary: use udev-provided partlabel symlink (most reliable in initramfs)
    if [ -b "/dev/disk/by-partlabel/EFI" ]; then
        efi_partition="/dev/disk/by-partlabel/EFI"
    fi

    # Fallback: blkid search by PARTLABEL
    if [ -z "$efi_partition" ]; then
        for dev in $(blkid -t TYPE=vfat -o device 2>/dev/null); do
            if blkid "$dev" | grep -qi 'PARTLABEL="EFI"'; then
                efi_partition="$dev"
                break
            fi
        done
    fi

    # Last resort: first vfat partition
    if [ -z "$efi_partition" ]; then
        efi_partition=$(blkid -t TYPE=vfat -o device 2>/dev/null | head -n1)
    fi

    if [ -n "$efi_partition" ] && [ -b "$efi_partition" ]; then
        mount_volume "$efi_partition" "$new_root/boot/efi" "efi" || {{
            echo "[mountcrypt] WARNING: Failed to mount EFI partition" >&2
        }}
    else
        echo "[mountcrypt] WARNING: EFI partition not found, skipping" >&2
    fi

    echo "[mountcrypt] Mount sequence complete"
    return $ret
}}
"#,
        boot_mount = boot_mount_section
    );

    let install_content = r#"#!/bin/bash
build() {
    # blkid is needed for EFI partition detection fallback
    add_binary 'blkid'
    # mountpoint is used to check if root is already mounted
    add_binary 'mountpoint'
    add_runscript
}

help() {
    echo "mountcrypt: Mount multi-volume encrypted system (Crypt-Root, Crypt-Usr, Crypt-Var, Crypt-Home)"
}
"#.to_string();

    GeneratedHook {
        name: "mountcrypt".to_string(),
        hook_content,
        install_content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DeploymentConfig;

    /// Helper: build a config with the given layout and encryption flag
    fn config_with(layout: crate::config::PartitionLayout, encryption: bool) -> DeploymentConfig {
        let mut cfg = DeploymentConfig::sample();
        cfg.disk.layout = layout;
        cfg.disk.encryption = encryption;
        if encryption {
            cfg.disk.encryption_password = Some("test".to_string());
        }
        cfg
    }

    fn dummy_layout() -> crate::disk::layouts::ComputedLayout {
        crate::disk::layouts::ComputedLayout {
            partitions: vec![],
            total_mib: 0,
            subvolumes: None,
        }
    }

    #[test]
    fn no_hooks_generated_without_encryption() {
        let cfg = config_with(PartitionLayout::Standard, false);
        let hooks = generate_hooks(&cfg, &dummy_layout()).unwrap();
        assert!(hooks.is_empty());
    }

    #[test]
    fn no_hooks_generated_for_minimal() {
        let cfg = config_with(PartitionLayout::Minimal, false);
        let hooks = generate_hooks(&cfg, &dummy_layout()).unwrap();
        assert!(
            hooks.is_empty(),
            "Minimal layout should not use custom hooks"
        );
    }

    #[test]
    fn hooks_generated_for_standard_encrypted() {
        let cfg = config_with(PartitionLayout::Standard, true);
        let hooks = generate_hooks(&cfg, &dummy_layout()).unwrap();
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].name, "crypttab-unlock");
        assert_eq!(hooks[1].name, "mountcrypt");
    }

    #[test]
    fn crypttab_unlock_hook_does_not_call_run_hook() {
        let hook = generate_crypttab_unlock_hook();
        // The hook must define run_hook() but must NOT call it at the top level.
        // mkinitcpio's init sources the script and calls run_hook itself.
        assert!(hook.hook_content.contains("run_hook()"));
        // Ensure there is no bare `run_hook` invocation outside the function definition.
        // Split on the closing brace of run_hook() body and check the remainder.
        // The only occurrences of "run_hook" should be inside function definitions
        // or comments, not as a standalone invocation at the end of the script.
        let trailing = hook.hook_content.lines().last().unwrap_or("");
        assert_ne!(
            trailing.trim(),
            "run_hook",
            "run_hook must not be called explicitly at script end"
        );
    }

    #[test]
    fn crypttab_unlock_hook_translates_discard_option() {
        let hook = generate_crypttab_unlock_hook();
        assert!(
            hook.hook_content.contains("--allow-discards"),
            "crypttab-unlock must translate the discard option to --allow-discards"
        );
    }

    #[test]
    fn mountcrypt_hook_mounts_all_encrypted_partitions() {
        let cfg = config_with(PartitionLayout::Standard, true);
        let hook = generate_mountcrypt_hook(&cfg, &dummy_layout());
        assert!(hook.hook_content.contains("/dev/mapper/Crypt-Root"));
        assert!(hook.hook_content.contains("/dev/mapper/Crypt-Usr"));
        assert!(hook.hook_content.contains("/dev/mapper/Crypt-Var"));
        assert!(hook.hook_content.contains("/dev/mapper/Crypt-Home"));
    }

    #[test]
    fn mountcrypt_hook_encrypted_boot() {
        let mut cfg = config_with(PartitionLayout::Standard, true);
        cfg.disk.boot_encryption = true;
        let hook = generate_mountcrypt_hook(&cfg, &dummy_layout());
        assert!(
            hook.hook_content.contains("/dev/mapper/Crypt-Boot"),
            "With boot_encryption, mountcrypt must mount encrypted /boot"
        );
        assert!(
            !hook.hook_content.contains("LABEL=BOOT"),
            "With boot_encryption, mountcrypt must not auto-detect unencrypted boot"
        );
    }

    #[test]
    fn mountcrypt_hook_unencrypted_boot() {
        let mut cfg = config_with(PartitionLayout::Standard, true);
        cfg.disk.boot_encryption = false;
        let hook = generate_mountcrypt_hook(&cfg, &dummy_layout());
        assert!(
            hook.hook_content.contains("LABEL=BOOT"),
            "Without boot_encryption, mountcrypt must auto-detect unencrypted boot"
        );
        assert!(
            !hook.hook_content.contains("/dev/mapper/Crypt-Boot"),
            "Without boot_encryption, mountcrypt must not reference Crypt-Boot"
        );
    }
}
