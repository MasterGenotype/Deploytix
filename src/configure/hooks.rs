//! Custom mkinitcpio hook generation

use crate::config::{DeploymentConfig, Filesystem};
use crate::disk::layouts::{is_root_partition, multi_volume_subvolumes, ComputedLayout};
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

/// Generate hooks based on configuration.
///
/// Hook generation is feature-driven, not layout-driven:
/// - Multi-LUKS (encryption without LVM thin): crypttab-unlock + mountcrypt
/// - LVM thin with boot encryption: crypttab-unlock
/// - Single-LUKS (LVM thin with encryption, no boot encryption): standard `encrypt` hook suffices
fn generate_hooks(
    config: &DeploymentConfig,
    layout: &ComputedLayout,
) -> Result<Vec<GeneratedHook>> {
    let uses_lvm_thin = config.disk.use_lvm_thin;
    let uses_multi_luks = config.disk.encryption && !uses_lvm_thin;

    let mut hooks = Vec::new();

    // Multi-LUKS: separate LUKS containers for each data partition.
    // Needs crypttab-unlock to open all containers, and mountcrypt to mount them.
    if uses_multi_luks {
        hooks.push(generate_crypttab_unlock_hook());
        hooks.push(generate_mountcrypt_hook(config, layout));
    }

    // LVM thin with boot encryption: crypttab-unlock opens the LUKS1 /boot
    // container. The main Crypt-LVM container is handled by the encrypt hook.
    if uses_lvm_thin && config.disk.boot_encryption {
        hooks.push(generate_crypttab_unlock_hook());
    }

    // LVM immutable A/B: verity-ab opens the active slot's dm-verity device,
    // mounts it read-only as /, layers the writable /etc overlay, and mounts the
    // persistent var/home + boot/EFI.
    if config.immutable_lvm_ab() {
        hooks.push(generate_verity_ab_hook(config));
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
        sleep 1
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
        sleep 1
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

        # Determine the full mapper name.
        # If the crypttab name already starts with "Crypt-", use it as-is.
        # Otherwise, title-case it and prepend "Crypt-" (e.g., "Root" -> "Crypt-Root").
        local full_mapper_name
        local formatted_mapping=""
        case "$mapping" in
            Crypt-*)
                full_mapper_name="$mapping"
                # Extract the portion after "Crypt-" for the EFI check below
                formatted_mapping="${mapping#Crypt-}"
                ;;
            *)
                formatted_mapping=$(echo "$mapping" | awk '{print toupper(substr($0,1,1)) tolower(substr($0,2))}')
                full_mapper_name="Crypt-$formatted_mapping"
                ;;
        esac

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

/// Generate the mountcrypt hook for multi-volume encrypted system.
///
/// Dynamically generates mount entries based on the actual LUKS partitions
/// in the layout, so it works for Standard (Root, Usr, Var, Home), Minimal
/// (Root only), and Custom layouts.
fn generate_mountcrypt_hook(config: &DeploymentConfig, layout: &ComputedLayout) -> GeneratedHook {
    let boot_mapper_name = &config.disk.luks_boot_mapper_name;

    // Collect encrypted data partitions from layout (non-EFI, non-boot, non-swap, is_luks)
    let luks_data_parts: Vec<&crate::disk::layouts::PartitionDef> = layout
        .partitions
        .iter()
        .filter(|p| p.is_luks && !p.is_efi && !p.is_boot_fs && !p.is_swap && !p.is_bios_boot)
        .collect();

    // Boot mount options: include subvol=@boot when boot filesystem is btrfs
    let boot_extra_opts = if config.disk.boot_filesystem == Filesystem::Btrfs {
        " \"subvol=@boot,noatime,compress=zstd\""
    } else {
        ""
    };

    // Generate /boot mount section depending on boot encryption
    let boot_mount_section = if config.disk.boot_encryption {
        format!(
            r#"    # Mount encrypted /boot from LUKS1 container
    mount_volume "/dev/mapper/{boot_mapper}" "$new_root/boot" "boot"{boot_opts} || true"#,
            boot_mapper = boot_mapper_name,
            boot_opts = boot_extra_opts,
        )
    } else {
        format!(
            r#"    # Mount unencrypted /boot partition
    boot_partition=""
    for dev in $(blkid -t LABEL=BOOT -o device 2>/dev/null); do
        boot_partition="$dev"
        break
    done

    if [ -n "$boot_partition" ] && [ -b "$boot_partition" ]; then
        mount_volume "$boot_partition" "$new_root/boot" "boot"{boot_opts} || true
    else
        echo "[mountcrypt] Warning: BOOT partition not found" >&2
    fi"#,
            boot_opts = boot_extra_opts,
        )
    };

    // Build the dynamic volume mount section from layout partitions.
    // Root is mounted first (fatal on failure). Other volumes are best-effort.
    let mut volume_mounts = String::new();
    let use_subvolumes = layout.uses_subvolumes();

    // Immutable root: `/` and `/usr` are mounted read-only, /etc lives on a
    // writable @etc subvolume, and the live boot is layered with the same
    // ephemeral overlay the snapshot boots use (so /tmp, /root, etc. stay
    // writable over the read-only root).
    let immutable = config.packages.immutable_root;
    // `,ro` suffix for read-only mounts under the immutable model.
    let ro_suffix = if immutable { ",ro" } else { "" };

    // Root must always be first
    let has_root = luks_data_parts.iter().any(|p| is_root_partition(p));
    if has_root {
        if use_subvolumes {
            // Root subvolume comes from the kernel cmdline: grub-btrfs snapshot
            // menu entries override rootflags=...,subvol=<path>, and honouring
            // it here is what makes those entries boot the selected snapshot
            // instead of the live subvolume.
            let root_svols = multi_volume_subvolumes("Root");
            volume_mounts.push_str(&format!(
                r#"    # Resolve root subvol from cmdline (snapshot booting support)
    local root_subvol
    root_subvol=$(resolve_root_subvol)
    echo "[mountcrypt] Root subvol resolved from cmdline: $root_subvol"

    # Mount root first (required)
    echo "[mountcrypt] === Mounting root (subvol=$root_subvol) ==="
    if ! mount_volume "/dev/mapper/Crypt-Root" "$new_root" "root" "subvol=${{root_subvol}},{sv_opts}{ro_suffix}"; then
        echo "[mountcrypt] FATAL: Cannot mount root filesystem (subvol=$root_subvol)" >&2
        if [ "$root_subvol" != "{sv_name}" ]; then
            echo "[mountcrypt] HINT: snapper snapshots are read-only by default; this mount tried rw." >&2
            echo "[mountcrypt]       To boot a RO snapshot, make it writable from the live system first:" >&2
            echo "[mountcrypt]         snapper -c root modify <num> --read-write" >&2
            echo "[mountcrypt]       Or set the property directly: btrfs property set <path> ro false" >&2
        fi
        return 1
    fi
"#,
                sv_name = root_svols[0].name,
                sv_opts = root_svols[0].mount_options,
                ro_suffix = ro_suffix,
            ));

            // The overlay exists for read-only *snapper* snapshots, which
            // cannot be booted any other way. It is deliberately NOT layered
            // for the layout root or for a deploytix snapshot set: under the
            // immutable model those are mounted read-only on purpose, and
            // turning `/` into an overlayfs is what stops `grub-probe`,
            // grub-btrfs's `41_snapshots-btrfs` generator and
            // `findmnt -no FSROOT /` from working against the running system —
            // i.e. it is what makes snapshot menu entries and the update/
            // rollback boot pointer unreliable. A read-only root keeps `/` a
            // real btrfs mount; the few paths inside it that must stay
            // writable get explicit fstab entries instead (see
            // `immutable_writable_paths` in `install/fstab.rs`).
            let overlay_guard = format!(
                "case \"$root_subvol\" in {root}|{sets}/*) false ;; *) true ;; esac",
                root = root_svols[0].name,
                sets = crate::immutable::snapshot::SETS_DIR,
            );
            // Immutable model: once the overlay is active, resolve the paired
            // /usr and /etc subvolumes from the `.deploytix-pair` marker inside
            // the booted root and mount them — /usr read-only, /etc read-write.
            //
            // The marker is what makes rollback consistent: booting a snapshot
            // set (rootflags=subvol=@deploytix-sets/<id>/root) picks up that
            // set's matching /usr and /etc rather than the live ones. The live
            // `@` boot's marker names `@usr`/`@etc`, so the default boot is
            // unchanged. `/usr`'s device is fixed by the layout (its own LUKS
            // container when present, else the root btrfs).
            let usr_device = luks_data_parts
                .iter()
                .find(|p| p.mount_point.as_deref() == Some("/usr"))
                .map(|p| {
                    let mut c = p.name.chars();
                    let title = match c.next() {
                        None => String::new(),
                        Some(f) => f.to_uppercase().to_string() + &c.as_str().to_lowercase(),
                    };
                    format!("/dev/mapper/Crypt-{}", title)
                })
                .unwrap_or_else(|| "/dev/mapper/Crypt-Root".to_string());
            let etc_mount = if immutable {
                format!(
                    r#"
    # Immutable root: resolve paired /usr and /etc from the boot marker.
    _pair_usr=@usr
    _pair_etc=@etc
    if [ -r "$new_root/.deploytix-pair" ]; then
        while IFS='=' read -r _k _v; do
            case "$_k" in
                usr) [ -n "$_v" ] && _pair_usr="$_v" ;;
                etc) [ -n "$_v" ] && _pair_etc="$_v" ;;
            esac
        done < "$new_root/.deploytix-pair"
    fi
    echo "[mountcrypt] Immutable pair: usr=$_pair_usr etc=$_pair_etc"
    echo "[mountcrypt] === Mounting /usr (subvol=$_pair_usr, ro) ==="
    if ! mount_volume "{usr_device}" "$new_root/usr" "usr" "subvol=$_pair_usr,noatime,compress=zstd,ro"; then
        echo "[mountcrypt] ERROR: Failed to mount /usr" >&2
        ret=1
    fi
    echo "[mountcrypt] === Mounting /etc (subvol=$_pair_etc, rw) ==="
    mount_volume "/dev/mapper/Crypt-Root" "$new_root/etc" "etc" "subvol=$_pair_etc,noatime,compress=zstd" || \
        echo "[mountcrypt] WARNING: failed to mount /etc" >&2
"#,
                    usr_device = usr_device,
                )
            } else {
                String::new()
            };

            // Read-only snapshot boot support. The stock grub-btrfs-overlayfs
            // latehook is NOT usable here: by latehook time this handler has
            // already mounted /usr, /var and /home inside $new_root, and its
            // `mount --move` would bury them in the overlay's lowerdir where
            // overlayfs hides submounts (/usr would appear empty). Instead the
            // overlay is layered right after the root mount, BEFORE the other
            // volume mounts, so they land on top of the overlay and stay
            // visible. Requires the `overlay` module (added to MODULES when
            // install_grub_btrfs is set on encrypted layouts).
            if config.packages.install_grub_btrfs {
                volume_mounts.push_str(&format!(
                    r#"
    # RO snapshot boot (grub-btrfs): snapper snapshots are read-only by
    # default. btrfs mounts a RO subvolume rw-at-the-vfs-level, so probe with
    # an actual write and layer an overlayfs when it fails.
    #
    # The upperdir is a dedicated writable @overlay subvolume on the root
    # btrfs: disk-backed (tens of GB free, not the ~50%-of-RAM tmpfs ceiling)
    # yet still ephemeral because upper/work are wiped on every boot. Temp and
    # build-heavy work (/tmp, /etc, /root all live in the overlay upper) then
    # has real disk to spend instead of competing for RAM. Falls back to a
    # tmpfs upper when @overlay is absent (installs predating this subvolume).
    if {overlay_guard}; then
        if touch "$new_root/.deploytix-rw-probe" 2>/dev/null; then
            rm -f "$new_root/.deploytix-rw-probe"
        else
            echo "[mountcrypt] Root subvol $root_subvol is read-only; layering overlay (changes are ephemeral)"
            mkdir -p /run/deploytix-overlay
            mount -t tmpfs -o mode=0755 deploytix-overlay /run/deploytix-overlay
            mkdir -p /run/deploytix-overlay/lower /run/deploytix-overlay/scratch
            mount --move "$new_root" /run/deploytix-overlay/lower
            # Prefer a disk-backed ephemeral upper on the @overlay subvolume;
            # wipe last boot's contents so changes stay non-persistent.
            if mount -t btrfs -o subvol=@overlay,rw,noatime,compress=zstd /dev/mapper/Crypt-Root /run/deploytix-overlay/scratch 2>/dev/null; then
                rm -rf /run/deploytix-overlay/scratch/upper /run/deploytix-overlay/scratch/work
                mkdir -p /run/deploytix-overlay/scratch/upper /run/deploytix-overlay/scratch/work
                _dpx_upper=/run/deploytix-overlay/scratch/upper
                _dpx_work=/run/deploytix-overlay/scratch/work
                echo "[mountcrypt] Overlay upper is disk-backed (@overlay subvolume)"
            else
                echo "[mountcrypt] @overlay subvolume unavailable; falling back to tmpfs (RAM) upper" >&2
                mkdir -p /run/deploytix-overlay/upper /run/deploytix-overlay/work
                _dpx_upper=/run/deploytix-overlay/upper
                _dpx_work=/run/deploytix-overlay/work
            fi
            if mount -t overlay overlay -o "lowerdir=/run/deploytix-overlay/lower,upperdir=$_dpx_upper,workdir=$_dpx_work" "$new_root"; then
                echo "[mountcrypt] Overlay root active (upper=$_dpx_upper)"
            else
                echo "[mountcrypt] WARNING: overlay mount failed; restoring direct (read-only) root mount" >&2
                umount /run/deploytix-overlay/scratch 2>/dev/null || true
                mount --move /run/deploytix-overlay/lower "$new_root"
            fi
        fi
    fi
{etc_mount}"#,
                    overlay_guard = overlay_guard,
                    etc_mount = etc_mount,
                ));
            }
        } else {
            volume_mounts.push_str(
                r#"    # Mount root first (required)
    echo "[mountcrypt] === Mounting root ==="
    if ! mount_volume "/dev/mapper/Crypt-Root" "$new_root" "root"; then
        echo "[mountcrypt] FATAL: Cannot mount root filesystem" >&2
        return 1
    fi
"#,
            );
        }
    }

    // Remaining encrypted volumes
    for part in &luks_data_parts {
        if is_root_partition(part) {
            continue; // Already handled above
        }
        let mp = match part.mount_point.as_deref() {
            Some(mp) => mp,
            None => continue,
        };
        // Title-case the partition name for the mapper device
        let title = {
            let mut c = part.name.chars();
            match c.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().to_string() + &c.as_str().to_lowercase(),
            }
        };
        let mapper = format!("Crypt-{}", title);

        if use_subvolumes {
            let svols = multi_volume_subvolumes(&title);
            for sv in &svols {
                // Immutable model: /usr is mounted from the boot marker (ro) in
                // the pairing block above, so skip it in this generic loop.
                if immutable && sv.mount_point == "/usr" {
                    continue;
                }
                // /usr failure is a hard error; everything else is a warning
                let severity = if sv.mount_point == "/usr" {
                    "ERROR"
                } else {
                    "WARNING"
                };
                let fail_action = if sv.mount_point == "/usr" {
                    "        ret=1".to_string()
                } else {
                    String::new()
                };
                volume_mounts.push_str(&format!(
                    r#"
    # Mount {mp} (subvol={sv_name})
    echo "[mountcrypt] === Mounting {mp} ==="
    if ! mount_volume "/dev/mapper/{mapper}" "$new_root{mp}" "{name}" "subvol={sv_name},{sv_opts}"; then
        echo "[mountcrypt] {severity}: Failed to mount {mp}" >&2
{fail_action}
    fi
"#,
                    mp = sv.mount_point,
                    mapper = mapper,
                    sv_name = sv.name,
                    sv_opts = sv.mount_options,
                    name = sv.name.trim_start_matches('@'),
                    severity = severity,
                    fail_action = fail_action,
                ));
            }
        } else {
            // /usr failure is a hard error; everything else is a warning
            let severity = if mp == "/usr" { "ERROR" } else { "WARNING" };
            let fail_action = if mp == "/usr" {
                "        ret=1".to_string()
            } else {
                String::new()
            };
            volume_mounts.push_str(&format!(
                r#"
    # Mount {mp}
    echo "[mountcrypt] === Mounting {mp} ==="
    if ! mount_volume "/dev/mapper/{mapper}" "$new_root{mp}" "{name}"; then
        echo "[mountcrypt] {severity}: Failed to mount {mp}" >&2
{fail_action}
    fi
"#,
                mp = mp,
                mapper = mapper,
                name = part.name.to_lowercase(),
                severity = severity,
                fail_action = fail_action,
            ));
        }
    }

    // resolve_root_subvol is only emitted (and used) when root is mounted from
    // a btrfs subvolume; plain-filesystem roots ignore rootflags entirely.
    let resolve_fn = if use_subvolumes && has_root {
        let root_svols = multi_volume_subvolumes("Root");
        format!(
            r#"# Resolve the root subvolume from /proc/cmdline.
# Honours rootflags=subvol=<path>; last occurrence wins (matching kernel
# behaviour for repeated parameters). Defaults to {default} when nothing matches.
# Strips surrounding double-quotes from the value (grub-btrfs emits them).
resolve_root_subvol() {{
    local subvol="{default}"
    local arg rf f v
    local old_ifs="$IFS"

    for arg in $(cat /proc/cmdline 2>/dev/null); do
        case "$arg" in
            rootflags=*)
                rf="${{arg#rootflags=}}"
                IFS=','
                for f in $rf; do
                    case "$f" in
                        subvol=*)
                            v="${{f#subvol=}}"
                            v="${{v#\"}}"
                            v="${{v%\"}}"
                            [ -n "$v" ] && subvol="$v"
                            ;;
                    esac
                done
                IFS="$old_ifs"
                ;;
        esac
    done

    echo "$subvol"
}}

"#,
            default = root_svols[0].name,
        )
    } else {
        String::new()
    };

    // Build description comment listing actual volumes
    let volume_list: Vec<String> = luks_data_parts
        .iter()
        .map(|p| {
            let mp = p.mount_point.as_deref().unwrap_or("-");
            let title = {
                let mut c = p.name.chars();
                match c.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().to_string() + &c.as_str().to_lowercase(),
                }
            };
            format!("#   - Crypt-{} -> {}", title, mp)
        })
        .collect();
    let volume_comment = volume_list.join("\n");

    let hook_content = format!(
        r#"#!/usr/bin/ash
# mountcrypt: Mount multi-volume encrypted system
# Generated by Deploytix
#
# Mounts separate LUKS-encrypted partitions:
{volume_comment}
#
# This hook runs AFTER crypttab-unlock has unlocked all volumes.

# Wait for a block device to appear
wait_for_block_device() {{
    local device="$1"
    local timeout=30

    while [ ! -b "$device" ] && [ $timeout -gt 0 ]; do
        sleep 1
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
# Args: device mount_point name [extra_opts]
mount_volume() {{
    local device="$1"
    local mount_point="$2"
    local name="$3"
    local extra_opts="${{4:-}}"

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

    # Build mount options. When extra_opts is given it fully controls the
    # options (mount defaults to rw unless it contains "ro"); this lets callers
    # request read-only mounts (immutable /, /usr). Bare mounts default to rw.
    local opts="$extra_opts"
    if [ -z "$opts" ]; then
        opts="rw"
    fi

    # Create mount point and mount
    mkdir -p "$mount_point"
    if mount -o "$opts" "$device" "$mount_point"; then
        echo "[mountcrypt] Mounted $device -> $mount_point ($opts)"
        return 0
    else
        echo "[mountcrypt] ERROR: Failed to mount $device -> $mount_point" >&2
        return 1
    fi
}}

{resolve_fn}# run_hook is called during the hooks phase
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

{volume_mounts}
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
        volume_comment = volume_comment,
        volume_mounts = volume_mounts,
        boot_mount = boot_mount_section,
        resolve_fn = resolve_fn,
    );

    let help_volumes: Vec<String> = luks_data_parts
        .iter()
        .filter_map(|p| {
            let title = {
                let mut c = p.name.chars();
                match c.next() {
                    None => return None,
                    Some(first) => first.to_uppercase().to_string() + &c.as_str().to_lowercase(),
                }
            };
            Some(format!("Crypt-{}", title))
        })
        .collect();

    let install_content = format!(
        r#"#!/bin/bash
build() {{
    # blkid is needed for EFI partition detection fallback
    add_binary 'blkid'
    # mountpoint is used to check if root is already mounted
    add_binary 'mountpoint'
    add_runscript
}}

help() {{
    echo "mountcrypt: Mount multi-volume encrypted system ({})"
}}
"#,
        help_volumes.join(", ")
    );

    GeneratedHook {
        name: "mountcrypt".to_string(),
        hook_content,
        install_content,
    }
}

/// Generate the `verity-ab` hook for the LVM immutable A/B backend.
///
/// At boot it reads `deploytix.slot=` / `deploytix.roothash=` from the kernel
/// cmdline (set per slot by GRUB), opens the active slot's dm-verity device with
/// that root hash, mounts it **read-only** as `/`, layers a writable overlay over
/// `/etc` (upper on the persistent `etc_overlay` LV), and mounts the persistent
/// `var`/`home` plus the shared `/boot` and EFI. Any dm-verity failure aborts the
/// mount (the kernel then falls through to the other slot's GRUB entry / recovery).
fn generate_verity_ab_hook(config: &DeploymentConfig) -> GeneratedHook {
    let vg = &config.disk.lvm_vg_name;
    let verity_name = crate::configure::verity::VERITY_MAPPER_NAME;
    use crate::disk::lvm::ab;

    // The hook mounts only what is required before switch_root: the read-only
    // verity root and its writable /etc overlay. /var, /home, /boot and EFI are
    // stable-UUID volumes left to systemd via /etc/fstab (see generate_fstab_lvm_ab),
    // which avoids double-mount races with the initramfs handler.
    let hook_content = format!(
        r#"#!/usr/bin/ash
# verity-ab: Mount the active dm-verity root slot (LVM immutable A/B)
# Generated by Deploytix
#
# Volume group: {vg}
# Slots: {root_a}/{hash_a} (A), {root_b}/{hash_b} (B)
# Writable /etc overlay upper: {etc_overlay}

wait_for_block_device() {{
    local device="$1" timeout=30
    while [ ! -b "$device" ] && [ $timeout -gt 0 ]; do
        sleep 1; timeout=$((timeout - 1))
    done
    [ -b "$device" ]
}}

# Read a key from /proc/cmdline (last occurrence wins).
cmdline_value() {{
    local key="$1" arg out=""
    for arg in $(cat /proc/cmdline 2>/dev/null); do
        case "$arg" in
            "$key"=*) out="${{arg#$key=}}" ;;
        esac
    done
    echo "$out"
}}

run_hook() {{
    mount_handler=deploytix_verity_handler
}}

deploytix_verity_handler() {{
    local new_root="$1"
    local ret=0
    local vg="{vg}"

    if mountpoint -q "$new_root" 2>/dev/null; then
        echo "[verity-ab] $new_root already mounted; skipping"
        return 0
    fi

    local slot roothash root_lv hash_lv
    slot=$(cmdline_value deploytix.slot)
    roothash=$(cmdline_value deploytix.roothash)
    [ -n "$slot" ] || slot=A

    case "$slot" in
        A|a) root_lv="{root_a}"; hash_lv="{hash_a}" ;;
        B|b) root_lv="{root_b}"; hash_lv="{hash_b}" ;;
        *)   echo "[verity-ab] Unknown slot '$slot'; defaulting to A" >&2
             root_lv="{root_a}"; hash_lv="{hash_a}" ;;
    esac
    echo "[verity-ab] Active slot: $slot (root=$root_lv hash=$hash_lv)"

    if [ -z "$roothash" ]; then
        echo "[verity-ab] FATAL: no deploytix.roothash= on cmdline" >&2
        return 1
    fi

    local data_dev="/dev/$vg/$root_lv"
    local hash_dev="/dev/$vg/$hash_lv"

    echo "[verity-ab] Waiting for $data_dev and $hash_dev ..."
    if ! wait_for_block_device "$data_dev" || ! wait_for_block_device "$hash_dev"; then
        echo "[verity-ab] FATAL: slot devices not found (VG not activated?)" >&2
        return 1
    fi

    echo "[verity-ab] Opening dm-verity ($data_dev, roothash=$roothash)"
    if ! veritysetup open "$data_dev" {verity_name} "$hash_dev" "$roothash"; then
        echo "[verity-ab] FATAL: dm-verity open failed (image tampered or wrong hash)" >&2
        return 1
    fi

    echo "[verity-ab] === Mounting root (read-only) ==="
    mkdir -p "$new_root"
    if ! mount -o ro "/dev/mapper/{verity_name}" "$new_root"; then
        echo "[verity-ab] FATAL: cannot mount verified root" >&2
        return 1
    fi

    # Writable /etc overlay: lower = read-only image /etc, upper/work persist on
    # the etc_overlay LV so config edits survive reboots and slot flips.
    echo "[verity-ab] === Layering writable /etc overlay ==="
    if wait_for_block_device "/dev/$vg/{etc_overlay}"; then
        mkdir -p /run/deploytix-etc
        if mount "/dev/$vg/{etc_overlay}" /run/deploytix-etc; then
            mkdir -p /run/deploytix-etc/upper /run/deploytix-etc/work
            if ! mount -t overlay overlay \
                -o "lowerdir=$new_root/etc,upperdir=/run/deploytix-etc/upper,workdir=/run/deploytix-etc/work" \
                "$new_root/etc"; then
                echo "[verity-ab] WARNING: /etc overlay failed; /etc is read-only this boot" >&2
            fi
        else
            echo "[verity-ab] WARNING: could not mount etc_overlay LV" >&2
        fi
    else
        echo "[verity-ab] WARNING: etc_overlay LV missing; /etc is read-only" >&2
    fi

    # /var, /home, /boot and EFI are mounted post-switch_root by systemd from
    # /etc/fstab (stable UUIDs) — not here — to avoid double-mount races.
    echo "[verity-ab] Mount sequence complete (slot $slot)"
    return $ret
}}
"#,
        vg = vg,
        verity_name = verity_name,
        root_a = ab::ROOT_A,
        root_b = ab::ROOT_B,
        hash_a = ab::HASH_A,
        hash_b = ab::HASH_B,
        etc_overlay = ab::ETC_OVERLAY,
    );

    let install_content = r#"#!/bin/bash
build() {
    add_binary 'veritysetup'
    add_binary 'blkid'
    add_binary 'mountpoint'
    add_module 'dm-verity'
    add_module 'overlay'
    add_runscript
}

help() {
    echo "verity-ab: Mount the active dm-verity root slot (LVM immutable A/B)"
}
"#
    .to_string();

    GeneratedHook {
        name: "verity-ab".to_string(),
        hook_content,
        install_content,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DeploymentConfig;

    /// Helper: build a config with the given encryption flag
    fn config_encrypted(encryption: bool) -> DeploymentConfig {
        let mut cfg = DeploymentConfig::sample();
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
            planned_thin_volumes: None,
        }
    }

    /// Layout resembling Standard with 4 encrypted data partitions
    fn standard_encrypted_layout() -> crate::disk::layouts::ComputedLayout {
        use crate::disk::layouts::PartitionDef;
        crate::disk::layouts::ComputedLayout {
            partitions: vec![
                PartitionDef {
                    number: 1,
                    name: "EFI".into(),
                    size_mib: 512,
                    type_guid: String::new(),
                    mount_point: Some("/boot/efi".into()),
                    is_swap: false,
                    is_efi: true,
                    is_luks: false,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                    subvolume_name: None,
                    pinned: None,
                },
                PartitionDef {
                    number: 2,
                    name: "BOOT".into(),
                    size_mib: 2048,
                    type_guid: String::new(),
                    mount_point: Some("/boot".into()),
                    is_swap: false,
                    is_efi: false,
                    is_luks: false,
                    is_bios_boot: false,
                    is_boot_fs: true,
                    attributes: None,
                    subvolume_name: None,
                    pinned: None,
                },
                PartitionDef {
                    number: 3,
                    name: "SWAP".into(),
                    size_mib: 4096,
                    type_guid: String::new(),
                    mount_point: None,
                    is_swap: true,
                    is_efi: false,
                    is_luks: false,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                    subvolume_name: None,
                    pinned: None,
                },
                PartitionDef {
                    number: 4,
                    name: "ROOT".into(),
                    size_mib: 20480,
                    type_guid: String::new(),
                    mount_point: Some("/".into()),
                    is_swap: false,
                    is_efi: false,
                    is_luks: true,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                    subvolume_name: None,
                    pinned: None,
                },
                PartitionDef {
                    number: 5,
                    name: "USR".into(),
                    size_mib: 20480,
                    type_guid: String::new(),
                    mount_point: Some("/usr".into()),
                    is_swap: false,
                    is_efi: false,
                    is_luks: true,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                    subvolume_name: None,
                    pinned: None,
                },
                PartitionDef {
                    number: 6,
                    name: "VAR".into(),
                    size_mib: 8192,
                    type_guid: String::new(),
                    mount_point: Some("/var".into()),
                    is_swap: false,
                    is_efi: false,
                    is_luks: true,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                    subvolume_name: None,
                    pinned: None,
                },
                PartitionDef {
                    number: 7,
                    name: "HOME".into(),
                    size_mib: 0,
                    type_guid: String::new(),
                    mount_point: Some("/home".into()),
                    is_swap: false,
                    is_efi: false,
                    is_luks: true,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                    subvolume_name: None,
                    pinned: None,
                },
            ],
            total_mib: 100000,
            subvolumes: None,
            planned_thin_volumes: None,
        }
    }

    /// Layout resembling Minimal with only Root encrypted
    fn minimal_encrypted_layout() -> crate::disk::layouts::ComputedLayout {
        use crate::disk::layouts::PartitionDef;
        crate::disk::layouts::ComputedLayout {
            partitions: vec![
                PartitionDef {
                    number: 1,
                    name: "EFI".into(),
                    size_mib: 512,
                    type_guid: String::new(),
                    mount_point: Some("/boot/efi".into()),
                    is_swap: false,
                    is_efi: true,
                    is_luks: false,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                    subvolume_name: None,
                    pinned: None,
                },
                PartitionDef {
                    number: 2,
                    name: "BOOT".into(),
                    size_mib: 2048,
                    type_guid: String::new(),
                    mount_point: Some("/boot".into()),
                    is_swap: false,
                    is_efi: false,
                    is_luks: false,
                    is_bios_boot: false,
                    is_boot_fs: true,
                    attributes: None,
                    subvolume_name: None,
                    pinned: None,
                },
                PartitionDef {
                    number: 3,
                    name: "SWAP".into(),
                    size_mib: 4096,
                    type_guid: String::new(),
                    mount_point: None,
                    is_swap: true,
                    is_efi: false,
                    is_luks: false,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                    subvolume_name: None,
                    pinned: None,
                },
                PartitionDef {
                    number: 4,
                    name: "ROOT".into(),
                    size_mib: 0,
                    type_guid: String::new(),
                    mount_point: Some("/".into()),
                    is_swap: false,
                    is_efi: false,
                    is_luks: true,
                    is_bios_boot: false,
                    is_boot_fs: false,
                    attributes: None,
                    subvolume_name: None,
                    pinned: None,
                },
            ],
            total_mib: 100000,
            subvolumes: None,
            planned_thin_volumes: None,
        }
    }

    #[test]
    fn no_hooks_generated_without_encryption() {
        let cfg = config_encrypted(false);
        let hooks = generate_hooks(&cfg, &dummy_layout()).unwrap();
        assert!(hooks.is_empty());
    }

    #[test]
    fn hooks_generated_for_encrypted() {
        let cfg = config_encrypted(true);
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
        let cfg = config_encrypted(true);
        let hook = generate_mountcrypt_hook(&cfg, &standard_encrypted_layout());
        assert!(hook.hook_content.contains("/dev/mapper/Crypt-Root"));
        assert!(hook.hook_content.contains("/dev/mapper/Crypt-Usr"));
        assert!(hook.hook_content.contains("/dev/mapper/Crypt-Var"));
        assert!(hook.hook_content.contains("/dev/mapper/Crypt-Home"));
    }

    #[test]
    fn mountcrypt_hook_minimal_only_mounts_root() {
        let cfg = config_encrypted(true);
        let hook = generate_mountcrypt_hook(&cfg, &minimal_encrypted_layout());
        assert!(
            hook.hook_content.contains("/dev/mapper/Crypt-Root"),
            "Minimal encrypted must mount Crypt-Root"
        );
        assert!(
            !hook.hook_content.contains("/dev/mapper/Crypt-Usr"),
            "Minimal encrypted must NOT reference Crypt-Usr"
        );
        assert!(
            !hook.hook_content.contains("/dev/mapper/Crypt-Home"),
            "Minimal encrypted must NOT reference Crypt-Home"
        );
    }

    #[test]
    fn mountcrypt_hook_encrypted_boot() {
        let mut cfg = config_encrypted(true);
        cfg.disk.boot_encryption = true;
        let hook = generate_mountcrypt_hook(&cfg, &standard_encrypted_layout());
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
        let mut cfg = config_encrypted(true);
        cfg.disk.boot_encryption = false;
        let hook = generate_mountcrypt_hook(&cfg, &standard_encrypted_layout());
        assert!(
            hook.hook_content.contains("LABEL=BOOT"),
            "Without boot_encryption, mountcrypt must auto-detect unencrypted boot"
        );
        assert!(
            !hook.hook_content.contains("/dev/mapper/Crypt-Boot"),
            "Without boot_encryption, mountcrypt must not reference Crypt-Boot"
        );
    }

    #[test]
    fn mountcrypt_hook_resolves_root_subvol_from_cmdline() {
        let cfg = config_encrypted(true);
        let mut layout = standard_encrypted_layout();
        layout.subvolumes = Some(multi_volume_subvolumes("Root"));
        let hook = generate_mountcrypt_hook(&cfg, &layout);
        assert!(
            hook.hook_content.contains("resolve_root_subvol()"),
            "Subvolume layouts must define resolve_root_subvol"
        );
        assert!(
            hook.hook_content
                .contains(r#"mount_volume "/dev/mapper/Crypt-Root" "$new_root" "root" "subvol=${root_subvol},"#),
            "Root mount must use the cmdline-resolved subvolume, not a literal"
        );
        assert!(
            hook.hook_content.contains("rootflags=*)"),
            "resolve_root_subvol must parse rootflags= from the cmdline"
        );
        // Non-root volumes stay pinned to their layout subvolumes: they live on
        // separate LUKS containers that a root snapshot does not cover.
        assert!(hook.hook_content.contains("subvol=@usr,"));
    }

    #[test]
    fn mountcrypt_hook_is_valid_shell_syntax() {
        let cfg = config_encrypted(true);
        let mut layout = standard_encrypted_layout();
        layout.subvolumes = Some(multi_volume_subvolumes("Root"));
        let hook = generate_mountcrypt_hook(&cfg, &layout);

        let path = std::env::temp_dir().join(format!(
            "deploytix-mountcrypt-syntax-{}",
            std::process::id()
        ));
        fs::write(&path, &hook.hook_content).unwrap();
        let status = std::process::Command::new("sh")
            .arg("-n")
            .arg(&path)
            .status();
        let _ = fs::remove_file(&path);
        // Skip silently when no sh is available (syntax check is best-effort)
        if let Ok(status) = status {
            assert!(
                status.success(),
                "generated mountcrypt hook fails sh -n syntax check"
            );
        }
    }

    fn config_lvm_ab(encryption: bool) -> DeploymentConfig {
        let mut cfg = DeploymentConfig::sample();
        cfg.disk.use_lvm_thin = true;
        cfg.disk.encryption = encryption;
        cfg.disk.boot_encryption = false;
        if encryption {
            cfg.disk.encryption_password = Some("test".to_string());
        }
        cfg.packages.install_grub_btrfs = false;
        cfg.packages.immutable_root = true;
        cfg
    }

    #[test]
    fn verity_ab_hook_is_valid_shell_syntax() {
        let cfg = config_lvm_ab(true);
        assert!(cfg.immutable_lvm_ab());
        let hook = generate_verity_ab_hook(&cfg);
        assert_eq!(hook.name, "verity-ab");
        // Opens the active slot's verity device and mounts / read-only.
        assert!(hook.hook_content.contains("veritysetup open"));
        assert!(hook.hook_content.contains("mount -o ro"));
        assert!(hook.hook_content.contains("deploytix.roothash"));
        // Layers a writable /etc overlay and mounts persistent state.
        assert!(hook.hook_content.contains("-t overlay overlay"));
        assert!(hook.install_content.contains("add_binary 'veritysetup'"));

        let path =
            std::env::temp_dir().join(format!("deploytix-verity-ab-syntax-{}", std::process::id()));
        fs::write(&path, &hook.hook_content).unwrap();
        let status = std::process::Command::new("sh")
            .arg("-n")
            .arg(&path)
            .status();
        let _ = fs::remove_file(&path);
        if let Ok(status) = status {
            assert!(
                status.success(),
                "generated verity-ab hook fails sh -n syntax check"
            );
        }
    }

    #[test]
    fn verity_ab_hook_is_selected_only_for_lvm_immutable() {
        // LVM immutable → verity-ab present.
        let hooks = generate_hooks(&config_lvm_ab(true), &dummy_layout()).unwrap();
        assert!(hooks.iter().any(|h| h.name == "verity-ab"));
        // Plain multi-LUKS btrfs → no verity-ab.
        let hooks = generate_hooks(&config_encrypted(true), &standard_encrypted_layout()).unwrap();
        assert!(!hooks.iter().any(|h| h.name == "verity-ab"));
    }

    #[test]
    fn mountcrypt_hook_without_subvolumes_has_no_cmdline_parsing() {
        let cfg = config_encrypted(true);
        let hook = generate_mountcrypt_hook(&cfg, &standard_encrypted_layout());
        assert!(
            !hook.hook_content.contains("resolve_root_subvol"),
            "Plain-filesystem layouts must not parse rootflags"
        );
    }

    #[test]
    fn mountcrypt_hook_overlay_block_only_with_grub_btrfs() {
        let mut layout = standard_encrypted_layout();
        layout.subvolumes = Some(multi_volume_subvolumes("Root"));

        // Flag off: no overlay logic (regression guard for the default path).
        let cfg = config_encrypted(true);
        let hook = generate_mountcrypt_hook(&cfg, &layout);
        assert!(
            !hook.hook_content.contains("deploytix-overlay"),
            "Overlay block must not be emitted without install_grub_btrfs"
        );

        // Flag on: overlay block present, layered before the other volume
        // mounts so /usr, /var and /home land on top of the overlay.
        let mut cfg = config_encrypted(true);
        cfg.packages.install_grub_btrfs = true;
        let hook = generate_mountcrypt_hook(&cfg, &layout);
        let overlay_pos = hook
            .hook_content
            .find("mount -t overlay overlay")
            .expect("Overlay mount missing with install_grub_btrfs");
        let usr_pos = hook
            .hook_content
            .find("=== Mounting /usr ===")
            .expect("/usr mount section missing");
        assert!(
            overlay_pos < usr_pos,
            "Overlay must be layered before /usr is mounted into the root"
        );
        assert!(hook.hook_content.contains("/run/deploytix-overlay"));

        // The upperdir must prefer the disk-backed @overlay subvolume (wiped
        // each boot for ephemerality) and fall back to a tmpfs upper when the
        // subvolume is absent, so temp/build writes are not RAM-capped.
        assert!(
            hook.hook_content
                .contains("mount -t btrfs -o subvol=@overlay,rw,noatime,compress=zstd /dev/mapper/Crypt-Root /run/deploytix-overlay/scratch"),
            "overlay upper must be backed by the @overlay subvolume"
        );
        assert!(
            hook.hook_content.contains(
                "rm -rf /run/deploytix-overlay/scratch/upper /run/deploytix-overlay/scratch/work"
            ),
            "disk-backed upper must be wiped each boot to stay ephemeral"
        );
        assert!(
            hook.hook_content
                .contains("falling back to tmpfs (RAM) upper"),
            "must retain a tmpfs fallback for installs without @overlay"
        );
    }

    #[test]
    fn mountcrypt_hook_immutable_mounts_ro_root_usr_and_etc() {
        let mut layout = standard_encrypted_layout();
        layout.subvolumes = Some(multi_volume_subvolumes("Root"));

        let mut cfg = config_encrypted(true);
        cfg.packages.install_grub_btrfs = true;
        cfg.packages.immutable_root = true;
        let hook = generate_mountcrypt_hook(&cfg, &layout);

        // Root is mounted read-only.
        assert!(
            hook.hook_content
                .contains("subvol=${root_subvol},defaults,noatime,compress=zstd,ro"),
            "immutable root must be mounted read-only"
        );
        // The pairing marker is consulted to resolve /usr and /etc subvols.
        assert!(
            hook.hook_content.contains("$new_root/.deploytix-pair"),
            "immutable boot must read the pairing marker"
        );
        // /usr is mounted read-only from the paired subvol.
        assert!(
            hook.hook_content
                .contains("subvol=$_pair_usr,noatime,compress=zstd,ro"),
            "immutable /usr must be mounted read-only from the paired subvol"
        );
        // /etc is mounted read-write from the paired subvol.
        assert!(
            hook.hook_content.contains(
                "mount_volume \"/dev/mapper/Crypt-Root\" \"$new_root/etc\" \"etc\" \"subvol=$_pair_etc,noatime,compress=zstd\""
            ),
            "immutable /etc must be mounted rw from the paired subvol"
        );
        // /usr must NOT also be mounted by the generic volume loop.
        assert!(
            !hook
                .hook_content
                .contains("=== Mounting /usr (subvol=@usr) ==="),
            "immutable /usr must not be double-mounted by the generic loop"
        );
        // The overlay must NOT be layered for the immutable root or for a
        // snapshot set. Both are mounted read-only deliberately, and turning
        // `/` into an overlayfs is what stops grub-probe, grub-btrfs's
        // generator and `findmnt -no FSROOT /` from working against the
        // running system. Only snapper's read-only snapshots get the overlay.
        assert!(
            !hook.hook_content.contains("if true; then"),
            "immutable boot must not layer the overlay unconditionally"
        );
        assert!(
            hook.hook_content
                .contains("case \"$root_subvol\" in @|@deploytix-sets/*) false ;; *) true ;; esac"),
            "the overlay must be scoped to subvols that are neither @ nor a set"
        );

        // And it must still be valid shell.
        let path = std::env::temp_dir().join(format!(
            "deploytix-mountcrypt-immutable-syntax-{}",
            std::process::id()
        ));
        fs::write(&path, &hook.hook_content).unwrap();
        let status = std::process::Command::new("sh")
            .arg("-n")
            .arg(&path)
            .status();
        let _ = fs::remove_file(&path);
        if let Ok(status) = status {
            assert!(
                status.success(),
                "generated immutable mountcrypt hook fails sh -n syntax check"
            );
        }
    }

    #[test]
    fn mountcrypt_hook_with_overlay_is_valid_shell_syntax() {
        let mut cfg = config_encrypted(true);
        cfg.packages.install_grub_btrfs = true;
        let mut layout = standard_encrypted_layout();
        layout.subvolumes = Some(multi_volume_subvolumes("Root"));
        let hook = generate_mountcrypt_hook(&cfg, &layout);

        let path = std::env::temp_dir().join(format!(
            "deploytix-mountcrypt-overlay-syntax-{}",
            std::process::id()
        ));
        fs::write(&path, &hook.hook_content).unwrap();
        let status = std::process::Command::new("sh")
            .arg("-n")
            .arg(&path)
            .status();
        let _ = fs::remove_file(&path);
        if let Ok(status) = status {
            assert!(
                status.success(),
                "generated mountcrypt hook (overlay variant) fails sh -n syntax check"
            );
        }
    }

    #[test]
    fn lvm_thin_boot_encryption_generates_crypttab_unlock_hook() {
        let mut cfg = config_encrypted(true);
        cfg.disk.use_lvm_thin = true;
        cfg.disk.boot_encryption = true;
        let hooks = generate_hooks(&cfg, &dummy_layout()).unwrap();
        assert_eq!(
            hooks.len(),
            1,
            "LVM thin + boot encryption should generate 1 hook"
        );
        assert_eq!(hooks[0].name, "crypttab-unlock");
    }

    #[test]
    fn lvm_thin_no_boot_encryption_no_hooks() {
        let mut cfg = config_encrypted(true);
        cfg.disk.use_lvm_thin = true;
        let hooks = generate_hooks(&cfg, &dummy_layout()).unwrap();
        assert!(
            hooks.is_empty(),
            "LVM thin without boot encryption should not generate custom hooks"
        );
    }

    #[test]
    fn crypttab_unlock_hook_handles_crypt_prefixed_names() {
        let hook = generate_crypttab_unlock_hook();
        // The hook should handle names that already start with "Crypt-"
        assert!(
            hook.hook_content.contains("Crypt-*)"),
            "crypttab-unlock must handle names already prefixed with Crypt-"
        );
    }
}
