# Crypttab Hooks Documentation

This document provides a comprehensive analysis of the `crypttab-unlock` and `mountcrypt` mkinitcpio hooks, along with recommendations for expansion, optimization, and methodological insights for templatizing across different disk layouts.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Architecture Analysis](#2-architecture-analysis)
3. [Hook Behavior Details](#3-hook-behavior-details)
4. [Integration with Disk Layouts](#4-integration-with-disk-layouts)
5. [Recommendations for Expansion](#5-recommendations-for-expansion)
6. [Optimization Opportunities](#6-optimization-opportunities)
7. [Templatization Methodology](#7-templatization-methodology)
8. [Implementation Roadmap](#8-implementation-roadmap)

---

## 1. Overview

### 1.1 Purpose

The crypttab hooks provide early boot LUKS encryption support for Artix Linux installations, enabling:

- **Multi-partition encryption**: Unlock multiple LUKS containers during initramfs phase
- **Keyfile-based unlocking**: Support for passwordless unlock using keyfiles
- **Flexible device identification**: UUID-based device resolution for reliability

### 1.2 Components

| Component | File | Purpose |
|-----------|------|---------|
| `crypttab-unlock` hook | `ref/hooks_crypttab-unlock` | Parses `/etc/crypttab` and unlocks LUKS volumes |
| `crypttab-unlock` install | `ref/install_crypttab-unlock` | Adds cryptsetup and dependencies to initramfs |
| `mountcrypt` hook | `ref/hooks_mountcrypt` | Mounts unlocked volumes to filesystem hierarchy |
| `mountcrypt` install | `ref/install_mountcrypt` | Install script (minimal dependencies) |
| `crypttab` | `ref/crypttab` | Configuration file defining encrypted volumes |

### 1.3 Boot Sequence

```
1. Kernel loads initramfs
2. mkinitcpio executes hooks in order
3. crypttab-unlock:
   └── Parse /etc/crypttab
   └── Wait for devices (UUID resolution)
   └── cryptsetup open → /dev/mapper/Crypt-{Name}
4. mountcrypt:
   └── Wait for mapped devices
   └── Mount root (Crypt-Root) to /new_root
   └── Mount subvolumes (@usr, @var, @home, @boot)
   └── Auto-detect and mount EFI partition
5. Switch root to /new_root
```

---

## 2. Architecture Analysis

### 2.1 crypttab-unlock Hook

**Strengths:**

- **Portable shell**: Written in ash for busybox compatibility
- **Robust device detection**: Uses `wait_for_device()` with configurable timeout
- **UUID support**: Converts `UUID=` syntax to `/dev/disk/by-uuid/` paths
- **Graceful degradation**: Continues processing on individual partition failures
- **Consistent naming**: Applies title-case formatting to mapper names (`Crypt-Root`)

**Design Patterns:**

```ash
# Pattern: Device Wait with Timeout
wait_for_device() {
    local devpath="$1"
    local timeout=10
    while [ ! -e "$devpath" ] && [ $timeout -gt 0 ]; do
        sleep 0.5
        timeout=$((timeout - 1))
    done
}
```

```ash
# Pattern: Crypttab Line Parsing
while IFS= read -r line; do
    case "$line" in
        ''|\#*) continue ;;  # Skip empty/comments
    esac
    set -- $line  # Positional parsing
    local mapping="$1" device="$2" keyfile="$3"
done < "$crypttab"
```

### 2.2 mountcrypt Hook

**Strengths:**

- **Dynamic device discovery**: Iterates `/dev/mapper/Crypt-*` for mounted devices
- **Btrfs subvolume support**: Mounts `@`, `@usr`, `@var`, `@home`, `@boot` subvolumes
- **EFI auto-detection**: Finds VFAT partition by label or type
- **Non-fatal warnings**: Missing optional partitions don't abort boot

**Design Patterns:**

```ash
# Pattern: Mapper Device Discovery
for crypt_device in $(ls /dev/mapper/ | grep -E '^Crypt-'); do
    device_path="/dev/mapper/$crypt_device"
    # Process device...
done
```

```ash
# Pattern: Subvolume Mount with Fallback
if [ -b "$mapped_device" ]; then
    mount -o rw,subvol=${subvol} "$mapped_device" "$target"
else
    echo "Warning: Mapped device $mapped_device not found"
fi
```

### 2.3 Install Script Architecture

The `install_crypttab-unlock` script demonstrates proper mkinitcpio install patterns:

```bash
build() {
    # Kernel modules
    map add_module 'dm-crypt' 'dm-integrity' 'hid-generic?'
    add_all_modules '/crypto/'

    # Userspace binaries
    add_binary 'cryptsetup'
    add_binary '/usr/lib/libgcc_s.so.1'      # pthread requirement
    add_binary '/usr/lib/ossl-modules/legacy.so'  # OpenSSL legacy provider

    # Udev rules for device mapper
    map add_udev_rule '10-dm.rules' '13-dm-disk.rules' '95-dm-notify.rules'

    add_runscript
}
```

---

## 3. Hook Behavior Details

### 3.1 Crypttab Format

```
# MappingName   Device          KeyFile              Options
Root            UUID=<uuid>     /etc/cryptfs.key
Home            UUID=<uuid>     /etc/cryptfs.key
Boot            UUID=<uuid>     password
Swap            UUID=<uuid>     /etc/cryptfs.key
```

| Field | Description |
|-------|-------------|
| MappingName | Becomes `/dev/mapper/Crypt-{Name}` |
| Device | Block device or `UUID=<uuid>` |
| KeyFile | Path to keyfile, `none`, or `password` for interactive |
| Options | Additional cryptsetup options |

### 3.2 Naming Convention

The hooks enforce a consistent naming scheme:

```
Input:  ROOT  → Output: /dev/mapper/Crypt-Root
Input:  home  → Output: /dev/mapper/Crypt-Home
Input:  USR   → Output: /dev/mapper/Crypt-Usr
```

This is implemented via:
```ash
formatted_mapping=$(echo "$mapping" | awk '{print toupper(substr($0,1,1)) tolower(substr($0,2))}')
```

### 3.3 Mount Point Mapping

The mountcrypt hook hardcodes the following mount hierarchy:

| Mapper Device | Mount Point | Subvolume |
|---------------|-------------|-----------|
| Crypt-Root | /new_root | @ |
| Crypt-Boot | /new_root/boot | @boot |
| Crypt-Usr | /new_root/usr | @usr |
| Crypt-Var | /new_root/var | @var |
| Crypt-Home | /new_root/home | @home |

---

## 4. Integration with Disk Layouts

### 4.1 Current Layouts

**Standard Layout (7 partitions):**
```
Part | Name | Type     | Encrypted | Mapper
-----|------|----------|-----------|------------
1    | EFI  | FAT32    | No        | -
2    | Boot | Btrfs    | Yes*      | Crypt-Boot
3    | Swap | swap     | Yes       | Crypt-Swap
4    | Root | Btrfs    | Yes       | Crypt-Root
5    | Usr  | Btrfs    | Yes       | Crypt-Usr
6    | Var  | Btrfs    | Yes       | Crypt-Var
7    | Home | Btrfs    | Yes       | Crypt-Home
```
*Boot encryption requires password prompt (no keyfile in initramfs)

**Minimal Layout (3 partitions):**
```
Part | Name | Type     | Encrypted | Mapper
-----|------|----------|-----------|------------
1    | EFI  | FAT32    | No        | -
2    | Swap | swap     | Yes       | Crypt-Swap
3    | Root | Btrfs    | Yes       | Crypt-Root
```

### 4.2 Layout-to-Hook Mapping Gaps

| Issue | Impact | Current State |
|-------|--------|---------------|
| Hardcoded subvolume list | Minimal layout doesn't use separate `/usr`, `/var`, `/home` | Warning messages during boot |
| No boot encryption detection | Password always prompted for Boot if defined | User confusion |
| Static partition assumptions | `mountcrypt` assumes standard layout structure | Breaks with custom layouts |

---

## 5. Recommendations for Expansion

### 5.1 Dynamic Layout Detection

**Problem:** Hooks assume a fixed partition structure.

**Solution:** Implement layout discovery from crypttab entries:

```ash
# Proposed: Dynamic mount detection
run_hook() {
    for crypt_device in $(ls /dev/mapper/ | grep -E '^Crypt-'); do
        name="${crypt_device#Crypt-}"
        device_path="/dev/mapper/$crypt_device"

        case "$name" in
            Root) mount_root "$device_path" ;;
            Home) mount_secondary "$device_path" "/home" "@home" ;;
            Usr)  mount_secondary "$device_path" "/usr" "@usr" ;;
            Var)  mount_secondary "$device_path" "/var" "@var" ;;
            Boot) mount_secondary "$device_path" "/boot" "@boot" ;;
            Swap) ;; # Handled by swapon
            *)    echo "Unknown mapping: $name" ;;
        esac
    done
}
```

### 5.2 Keyfile Management

**Current Limitation:** Keyfiles must be embedded in initramfs at build time.

**Recommended Enhancements:**

1. **USB keyfile support:**
   ```ash
   find_keyfile() {
       # Check for keyfile on removable media
       for dev in /dev/sd[a-z]1; do
           mount -t vfat "$dev" /mnt 2>/dev/null && {
               if [ -f "/mnt/cryptfs.key" ]; then
                   echo "/mnt/cryptfs.key"
                   return 0
               fi
               umount /mnt
           }
       done
       return 1
   }
   ```

2. **TPM-based unlock (systemd-cryptenroll):**
   ```
   # Future integration point for TPM2 unlock
   options tpm2-device=auto
   ```

3. **Network-based key server:**
   ```ash
   # For enterprise deployments
   fetch_key_from_server() {
       curl -s "http://keyserver/keys/$1" > /tmp/key
   }
   ```

### 5.3 LUKS2 Features

The current implementation is LUKS1-compatible. LUKS2 enhancements to support:

| Feature | Benefit | Implementation |
|---------|---------|----------------|
| Argon2id | Memory-hard KDF | `cryptsetup --pbkdf argon2id` |
| Integrity | Authenticated encryption | `dm-integrity` kernel module |
| Tokens | External unlock mechanisms | TPM, FIDO2, PKCS#11 |

### 5.4 Hibernation Support

**Problem:** Encrypted swap requires special handling for hibernation.

**Solution:** Add resume hook integration:

```ash
# In crypttab-unlock, after unlocking swap:
if [ -n "$SWAP_DEVICE" ] && [ -f /sys/power/resume ]; then
    SWAP_MAJOR=$(stat -c '%t' "$SWAP_DEVICE")
    SWAP_MINOR=$(stat -c '%T' "$SWAP_DEVICE")
    echo "${SWAP_MAJOR}:${SWAP_MINOR}" > /sys/power/resume
fi
```

### 5.5 Multi-Device LUKS (RAID/LVM)

Support for advanced storage configurations:

```ash
# Detect and assemble md arrays before LUKS
if [ -x /sbin/mdadm ]; then
    mdadm --assemble --scan
    udevadm settle
fi

# Activate LVM after LUKS unlock
if [ -x /sbin/lvm ]; then
    lvm vgscan
    lvm vgchange -ay
fi
```

---

## 6. Optimization Opportunities

### 6.1 Parallel Unlock

**Current:** Sequential unlock of each LUKS volume.

**Optimization:** Parallel unlock for multiple volumes with same keyfile:

```ash
# Pseudo-parallel approach using background processes
unlock_parallel() {
    local pids=""
    while IFS= read -r line; do
        parse_crypttab_line "$line"
        if [ "$keyfile" = "$SHARED_KEYFILE" ]; then
            cryptsetup open "$device" "Crypt-$mapping" --key-file "$keyfile" &
            pids="$pids $!"
        fi
    done < /etc/crypttab

    for pid in $pids; do
        wait "$pid"
    done
}
```

**Estimated Impact:** 2-4x faster boot with multiple encrypted volumes.

### 6.2 Reduced Timeout Strategy

**Current:** Fixed 10-second timeout per device.

**Optimization:** Adaptive timeout based on device type:

```ash
get_device_timeout() {
    case "$1" in
        /dev/nvme*)    echo 5 ;;   # NVMe: fast
        /dev/sd*)      echo 10 ;;  # SATA: medium
        /dev/mmcblk*)  echo 15 ;;  # SD card: slow
        *)             echo 10 ;;  # Default
    esac
}
```

### 6.3 Initramfs Size Reduction

**Current:** Includes all crypto modules.

**Optimization:** Use `autodetect` hook + minimal crypto module set:

```bash
# In install_crypttab-unlock
if [[ -n "$CRYPTO_MODULES" ]]; then
    for mod in $CRYPTO_MODULES; do
        add_module "$mod"
    done
else
    # Minimal set for AES-XTS (most common)
    map add_module 'aes' 'xts' 'sha256' 'dm-crypt'
fi
```

### 6.4 Caching for Repeated Keyfile Use

```ash
# Cache decrypted keyfile in memory
CACHED_KEY=""
get_cached_key() {
    if [ -z "$CACHED_KEY" ]; then
        CACHED_KEY=$(cat "$1")
    fi
    echo "$CACHED_KEY"
}
```

---

## 7. Templatization Methodology

### 7.1 Layout Abstraction Layer

Create a layout definition format that hooks can consume:

```toml
# /etc/deploytix/layout.conf
[layout]
type = "standard"  # standard, minimal, custom

[partitions.root]
mapper = "Crypt-Root"
mount = "/"
subvolume = "@"
required = true

[partitions.home]
mapper = "Crypt-Home"
mount = "/home"
subvolume = "@home"
required = false

[partitions.usr]
mapper = "Crypt-Usr"
mount = "/usr"
subvolume = "@usr"
required = false
```

### 7.2 Hook Template Generator

Implement in Rust to generate hooks from layout configuration:

```rust
// src/configure/hooks.rs

pub struct HookGenerator {
    layout: ComputedLayout,
    encryption_config: EncryptionConfig,
}

impl HookGenerator {
    pub fn generate_crypttab_unlock_hook(&self) -> String {
        let mut script = String::from(HOOK_HEADER);

        // Add wait_for_device function
        script.push_str(&self.generate_wait_function());

        // Add run_hook based on layout
        script.push_str(&self.generate_run_hook());

        script
    }

    pub fn generate_mountcrypt_hook(&self) -> String {
        let mut script = String::from(HOOK_HEADER);

        // Generate mount commands based on layout
        for part in &self.layout.partitions {
            if part.encrypted && part.mount_point.is_some() {
                script.push_str(&self.generate_mount_command(part));
            }
        }

        script
    }
}
```

### 7.3 Template Variables

Define substitution variables for hook templates:

| Variable | Description | Example |
|----------|-------------|---------|
| `{{ROOT_DEVICE}}` | Root device mapper path | `/dev/mapper/Crypt-Root` |
| `{{ROOT_SUBVOL}}` | Root subvolume | `@` |
| `{{KEYFILE_PATH}}` | Keyfile location | `/etc/cryptfs.key` |
| `{{MOUNT_TARGETS}}` | Generated mount commands | Dynamic |
| `{{TIMEOUT_SECONDS}}` | Device wait timeout | `10` |

### 7.4 Layout-Specific Hook Variants

**Standard Layout Template:**
```ash
#!/usr/bin/ash
# Generated for: Standard Layout (7 partitions)

MOUNT_TARGETS="boot:@boot usr:@usr var:@var home:@home"

run_hook() {
    mount -o rw,subvol=@ /dev/mapper/Crypt-Root /new_root

    for target in $MOUNT_TARGETS; do
        name="${target%:*}"
        subvol="${target#*:}"
        mkdir -p "/new_root/$name"
        mount -o rw,subvol="$subvol" "/dev/mapper/Crypt-$(echo $name | ...)" "/new_root/$name"
    done
}
```

**Minimal Layout Template:**
```ash
#!/usr/bin/ash
# Generated for: Minimal Layout (3 partitions)

run_hook() {
    mount -o rw /dev/mapper/Crypt-Root /new_root
    # No additional mount targets
}
```

### 7.5 Integration with Rust Codebase

Extend the existing mkinitcpio configuration:

```rust
// src/configure/mkinitcpio.rs - Extended

pub fn construct_hooks_with_encryption(config: &DeploymentConfig) -> Vec<String> {
    let mut hooks = construct_hooks(config);

    if config.disk.encryption {
        // Insert custom hooks after 'block', before 'filesystems'
        let block_idx = hooks.iter().position(|h| h == "block").unwrap_or(0) + 1;

        hooks.insert(block_idx, "crypttab-unlock".to_string());
        hooks.insert(block_idx + 1, "mountcrypt".to_string());

        // Remove standard encrypt hook (we use custom)
        hooks.retain(|h| h != "encrypt");
    }

    hooks
}

pub fn generate_encryption_hooks(
    config: &DeploymentConfig,
    layout: &ComputedLayout,
) -> Result<Vec<GeneratedHook>> {
    let generator = HookGenerator::new(layout, &config.disk);

    Ok(vec![
        GeneratedHook {
            name: "crypttab-unlock".to_string(),
            hook_content: generator.generate_crypttab_unlock_hook(),
            install_content: generator.generate_crypttab_unlock_install(),
        },
        GeneratedHook {
            name: "mountcrypt".to_string(),
            hook_content: generator.generate_mountcrypt_hook(),
            install_content: generator.generate_mountcrypt_install(),
        },
    ])
}
```

### 7.6 Crypttab Generation

Generate crypttab from layout configuration:

```rust
// src/install/crypttab.rs

pub fn generate_crypttab(
    device: &str,
    layout: &ComputedLayout,
    keyfile_path: &str,
) -> Result<String> {
    let mut content = String::from("# /etc/crypttab - Generated by Deploytix\n");
    content.push_str("# <name> <device> <keyfile> <options>\n\n");

    for part in &layout.partitions {
        if !part.encrypted {
            continue;
        }

        let part_path = partition_path(device, part.number);
        let uuid = get_partition_uuid(&part_path)?;

        let keyfile = if part.name == "Boot" {
            "password"  // Boot requires interactive password
        } else {
            keyfile_path
        };

        content.push_str(&format!(
            "{}\tUUID={}\t{}\n",
            part.name, uuid, keyfile
        ));
    }

    Ok(content)
}
```

---

## 8. Implementation Roadmap

### Phase 1: Core Encryption Support

| Task | Priority | Effort | Dependencies |
|------|----------|--------|--------------|
| Implement `setup_encryption()` in Rust | P0 | High | None |
| Generate crypttab from layout | P0 | Medium | Layout system |
| Copy hooks to initramfs | P0 | Low | Hook files |
| Update mkinitcpio.conf generation | P0 | Low | None |

### Phase 2: Layout Templatization

| Task | Priority | Effort | Dependencies |
|------|----------|--------|--------------|
| Define layout configuration format | P1 | Medium | None |
| Implement hook generator | P1 | High | Phase 1 |
| Support minimal layout encryption | P1 | Medium | Hook generator |
| Add custom layout support | P2 | High | Hook generator |

### Phase 3: Advanced Features

| Task | Priority | Effort | Dependencies |
|------|----------|--------|--------------|
| USB keyfile support | P2 | Medium | Phase 1 |
| TPM2 unlock integration | P3 | High | systemd-cryptenroll |
| LUKS2 integrity support | P3 | High | Phase 1 |
| Parallel unlock optimization | P3 | Medium | Phase 1 |

### Phase 4: Testing & Validation

| Task | Priority | Effort | Dependencies |
|------|----------|--------|--------------|
| Unit tests for hook generation | P1 | Medium | Phase 2 |
| Integration tests (VM-based) | P1 | High | Phase 1 |
| Boot failure recovery documentation | P2 | Low | All phases |

---

## Appendix A: Reference Implementation Files

### A.1 Complete Hook Files Location

```
ref/
├── hooks_crypttab-unlock    # Runtime hook (ash shell)
├── hooks_mountcrypt         # Mount hook (ash shell)
├── install_crypttab-unlock  # mkinitcpio install script
├── install_mountcrypt       # mkinitcpio install script
└── crypttab                 # Sample crypttab configuration
```

### A.2 Rust Integration Points

```
src/
├── configure/
│   ├── encryption.rs        # LUKS setup (needs implementation)
│   └── mkinitcpio.rs        # Hook configuration
├── disk/
│   └── layouts.rs           # Partition definitions
└── install/
    └── fstab.rs             # Related filesystem configuration
```

---

## Appendix B: Glossary

| Term | Definition |
|------|------------|
| **crypttab** | `/etc/crypttab` - Configuration file for encrypted block devices |
| **LUKS** | Linux Unified Key Setup - Disk encryption specification |
| **mkinitcpio** | Arch/Artix tool for generating initramfs images |
| **initramfs** | Initial RAM filesystem loaded by bootloader |
| **dm-crypt** | Device-mapper crypto target in Linux kernel |
| **mapper device** | Virtual block device created by device-mapper (`/dev/mapper/*`) |

---

*Documentation generated: 2026-01-30*
*Last updated: 2026-01-30*
