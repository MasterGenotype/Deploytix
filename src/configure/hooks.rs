//! Custom mkinitcpio hook generation

use crate::config::DeploymentConfig;
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
    info!("Installing custom mkinitcpio hooks");

    let hooks = generate_hooks(config, layout)?;

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

        info!("Installed hook: {}", hook.name);
    }

    Ok(())
}

/// Generate hooks based on configuration
fn generate_hooks(
    config: &DeploymentConfig,
    layout: &ComputedLayout,
) -> Result<Vec<GeneratedHook>> {
    let mut hooks = Vec::new();

    if config.disk.encryption {
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

# Function to wait for the device to appear
wait_for_device() {
    local devpath="$1"
    local timeout=10

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
        shift 3
        local options="$*"

        # Convert UUID= to device path
        case "$device" in
            UUID=*)
                local uuid="${device#UUID=}"
                device="/dev/disk/by-uuid/$uuid"
                ;;
        esac

        # Ensure the device is available before proceeding
        wait_for_device "$device" || {
            echo "[crypttab-unlock] ERROR: Device $device not found. Skipping $mapping."
            ret=1
            continue
        }

        # Convert the mapping name to follow proper capitalization (e.g., "ROOT" -> "Root")
        local formatted_mapping
        formatted_mapping=$(echo "$mapping" | awk '{print toupper(substr($0,1,1)) tolower(substr($0,2))}')

        echo "[crypttab-unlock] Unlocking $formatted_mapping from $device ..."

        # Verify keyfile existence
        if [ -n "$keyfile" ] && [ "$keyfile" != "none" ] && [ ! -f "$keyfile" ]; then
            echo "[crypttab-unlock] ERROR: Keyfile $keyfile does not exist. Skipping $mapping."
            ret=1
            continue
        fi

        # Build and run cryptsetup command with the properly formatted mapping name
        local cmd="cryptsetup open $device Crypt-$formatted_mapping"
        if [ -n "$keyfile" ] && [ "$keyfile" != "none" ]; then
            cmd="$cmd --key-file $keyfile"
        fi

        # Add any additional options from /etc/crypttab
        if [ -n "$options" ]; then
            cmd="$cmd $options"
        fi

        # Run the cryptsetup command
        if ! $cmd; then
            echo "[crypttab-unlock] ERROR: Failed to unlock $formatted_mapping from $device."
            ret=1
            continue
        fi

        echo "[crypttab-unlock] Successfully unlocked $formatted_mapping from $device."
    done < "$crypttab"

    return $ret
}

run_hook
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

    add_runscript
}
"#.to_string();

    GeneratedHook {
        name: "crypttab-unlock".to_string(),
        hook_content,
        install_content,
    }
}

/// Generate the mountcrypt hook (dynamically based on layout)
fn generate_mountcrypt_hook(config: &DeploymentConfig, layout: &ComputedLayout) -> GeneratedHook {
    let mapper_name = &config.disk.luks_mapper_name;

    // Generate subvolume mount commands
    let subvolume_mounts = if let Some(ref subvols) = layout.subvolumes {
        subvols
            .iter()
            .filter(|sv| sv.mount_point != "/")
            .map(|sv| {
                format!(
                    r#"    mkdir -p "$new_root{mp}"
    mount -o rw,subvol={name} "$cryptroot" "$new_root{mp}" || {{
        echo "Warning: Could not mount subvolume {name} on $new_root{mp}" >&2
    }}"#,
                    mp = sv.mount_point,
                    name = sv.name
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    } else {
        String::new()
    };

    let hook_content = format!(
        r#"#!/usr/bin/ash
# mountcrypt: Mount decrypted LUKS device with btrfs subvolumes
# Generated by Deploytix

run_hook() {{
    new_root="/new_root"
    cryptroot="/dev/mapper/{mapper}"

    # Wait for mapped device
    timeout=20
    while [ ! -b "$cryptroot" ] && [ $timeout -gt 0 ]; do
        sleep 0.5
        timeout=$((timeout - 1))
    done

    if [ ! -b "$cryptroot" ]; then
        echo "Error: $cryptroot not found" >&2
        return 1
    fi

    mkdir -p "$new_root"

    # Mount root subvolume (@)
    mount -o rw,subvol=@ "$cryptroot" "$new_root" || {{
        echo "Error mounting root subvolume @" >&2
        return 1
    }}

    # Mount additional subvolumes
{subvol_mounts}

    # Auto-detect and mount EFI partition
    mkdir -p "$new_root/boot/efi"
    efi_partition=""
    for dev in $(blkid -t TYPE=vfat -o device); do
        if blkid "$dev" | grep -qi 'PARTLABEL="EFI"'; then
            efi_partition="$dev"
            break
        fi
    done

    if [ -z "$efi_partition" ]; then
        # Fallback: first vfat partition
        efi_partition=$(blkid -t TYPE=vfat -o device | head -n1)
    fi

    if [ -n "$efi_partition" ] && [ -b "$efi_partition" ]; then
        mount -o rw "$efi_partition" "$new_root/boot/efi" || {{
            echo "Warning: Failed to mount EFI partition $efi_partition" >&2
        }}
        echo "Mounted EFI partition $efi_partition to $new_root/boot/efi"
    else
        echo "Warning: EFI partition not found, skipping EFI mount" >&2
    fi
}}
"#,
        mapper = mapper_name,
        subvol_mounts = subvolume_mounts
    );

    let install_content = r#"#!/bin/bash
build() {
    # No extra binaries are needed.
    add_runscript
}

help() {
    echo "mountcrypt: Mount decrypted Crypt-Root and its Btrfs subvolumes"
}
"#.to_string();

    GeneratedHook {
        name: "mountcrypt".to_string(),
        hook_content,
        install_content,
    }
}
