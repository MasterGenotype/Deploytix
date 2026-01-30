# Integration Guide: Artix Linux LUKS + btrfs Subvolumes

This document provides step-by-step instructions for integrating the `artix-runit-crypto-install-spec.md` specification into the Deploytix codebase. The integration enables full-disk encryption with btrfs subvolumes on Artix Linux with runit.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Prerequisites](#2-prerequisites)
3. [Phase 1: Configuration Schema Updates](#3-phase-1-configuration-schema-updates)
4. [Phase 2: New Partition Layout](#4-phase-2-new-partition-layout)
5. [Phase 3: LUKS Encryption Implementation](#5-phase-3-luks-encryption-implementation)
6. [Phase 4: Btrfs Subvolume Management](#6-phase-4-btrfs-subvolume-management)
7. [Phase 5: Custom mkinitcpio Hooks](#7-phase-5-custom-mkinitcpio-hooks)
8. [Phase 6: Crypttab Generation](#8-phase-6-crypttab-generation)
9. [Phase 7: Fstab Generation](#9-phase-7-fstab-generation)
10. [Phase 8: Bootloader Configuration](#10-phase-8-bootloader-configuration)
11. [Phase 9: Service Configuration](#11-phase-9-service-configuration)
12. [Phase 10: Installer Workflow Integration](#12-phase-10-installer-workflow-integration)
13. [Phase 11: Testing & Validation](#13-phase-11-testing--validation)
14. [Appendix: File Change Summary](#appendix-file-change-summary)

---

## 1. Overview

### 1.1 Target Architecture

The spec defines an encrypted Artix Linux installation with:

| Component | Choice |
|-----------|--------|
| Bootloader | GRUB (EFI) |
| Initramfs | mkinitcpio |
| Init System | runit |
| Encryption | LUKS (single container for root) |
| Filesystem | btrfs with subvolumes inside LUKS |
| Display Manager | greetd (only with desktop) |
| Seat Manager | seatd (only with desktop) |

### 1.2 Partition Structure

**Minimum 3 partitions required for reliable bootability:**

```
Partition 1: EFI System Partition (512 MiB, vfat, unencrypted)
Partition 2: BIOS Boot Partition (1 MiB, unformatted, for GRUB legacy)
Partition 3: LUKS Container (remainder of disk)
             └── btrfs filesystem
                 ├── @      → /
                 ├── @usr   → /usr
                 ├── @var   → /var
                 ├── @home  → /home
                 └── @boot  → /boot (encrypted)
```

> **Note:** The BIOS Boot partition ensures GRUB can boot on both UEFI and legacy BIOS systems.

### 1.3 Boot Sequence

**Default (headless):**
```
GRUB → kernel + initramfs → crypttab-unlock → mountcrypt → switch_root → runit → login prompt
```

**With desktop environment (e.g., KDE Plasma):**
```
GRUB → kernel + initramfs → crypttab-unlock → mountcrypt → switch_root → runit → seatd → greetd → desktop session
```

> **Note:** Desktop services (seatd, greetd) are only configured when a desktop environment is selected. The default installation is headless.

---

## 2. Prerequisites

### 2.1 Required Packages (Target System)

```toml
# Base
["runit", "runit-rc", "mkinitcpio", "cryptsetup", "btrfs-progs"]

# Bootloader
["grub", "efibootmgr"]

# Services (only with desktop)
["seatd", "seatd-runit", "greetd", "greetd-runit"]

# Network
["iwd"]  # Standalone wifi
# OR
["networkmanager", "iwd"]  # NetworkManager with iwd backend

# Desktop (optional)
["plasma-desktop", "startplasma-wayland"]
```

> **Important:** When using NetworkManager with iwd as the wifi backend, NetworkManager must be built from source with iwd support enabled. The installer will build and install NetworkManager when this option is selected. See `ref/meson_options.txt` for the build configuration:
> - Line 33: `-Diwd=true` — Enable iwd support
> - Line 26: `-Dconfig_wifi_backend_default=iwd` — Set iwd as default wifi backend

### 2.2 Codebase Files to Modify

| File | Changes |
|------|---------|
| `src/config/deployment.rs` | Add new layout type, encryption options |
| `src/disk/layouts.rs` | Implement `CryptoSubvolume` layout |
| `src/disk/formatting.rs` | Add LUKS formatting, btrfs subvolume creation |
| `src/configure/encryption.rs` | Full LUKS implementation |
| `src/configure/mkinitcpio.rs` | Add custom hook generation |
| `src/install/fstab.rs` | Support btrfs subvolume entries |
| `src/configure/bootloader.rs` | Add LUKS kernel parameters |
| `src/configure/services.rs` | Add greetd configuration |
| `src/install/installer.rs` | Update workflow order |

### 2.3 New Files to Create

| File | Purpose |
|------|---------|
| `src/configure/hooks.rs` | Generate custom mkinitcpio hooks |
| `src/install/crypttab.rs` | Generate `/etc/crypttab` |
| `src/configure/greetd.rs` | Configure greetd display manager |
| `src/configure/networkmanager.rs` | Build NetworkManager from source with iwd support |

---

## 3. Phase 1: Configuration Schema Updates

### 3.1 Add New Partition Layout Enum

**File:** `src/config/deployment.rs`

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PartitionLayout {
    #[default]
    Standard,
    Minimal,
    /// LUKS container with btrfs subvolumes (from spec)
    CryptoSubvolume,
    Custom,
}

impl std::fmt::Display for PartitionLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Standard => write!(f, "Standard (EFI, Boot, Swap, Root, Usr, Var, Home)"),
            Self::Minimal => write!(f, "Minimal (EFI, Swap, Root)"),
            Self::CryptoSubvolume => write!(f, "Encrypted (EFI + LUKS with btrfs subvolumes)"),
            Self::Custom => write!(f, "Custom"),
        }
    }
}
```

### 3.2 Extend DiskConfig

**File:** `src/config/deployment.rs`

Add new fields to `DiskConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskConfig {
    pub device: String,
    #[serde(default)]
    pub layout: PartitionLayout,
    #[serde(default)]
    pub filesystem: Filesystem,
    #[serde(default)]
    pub encryption: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption_password: Option<String>,
    
    // NEW FIELDS
    /// Name for the LUKS mapper device (default: "Crypt-Root")
    #[serde(default = "default_luks_name")]
    pub luks_mapper_name: String,
    /// Path to keyfile (None = password prompt)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyfile_path: Option<String>,
}

fn default_luks_name() -> String {
    "Crypt-Root".to_string()
}
```

### 3.3 Add Validation for CryptoSubvolume

**File:** `src/config/deployment.rs`

In `DeploymentConfig::validate()`:

```rust
pub fn validate(&self) -> Result<()> {
    // ... existing validation ...

    // CryptoSubvolume layout requires encryption
    if self.disk.layout == PartitionLayout::CryptoSubvolume {
        if !self.disk.encryption {
            return Err(DeploytixError::ValidationError(
                "CryptoSubvolume layout requires encryption to be enabled".to_string(),
            ));
        }
        if self.disk.filesystem != Filesystem::Btrfs {
            return Err(DeploytixError::ValidationError(
                "CryptoSubvolume layout requires btrfs filesystem".to_string(),
            ));
        }
    }

    Ok(())
}
```

### 3.4 Update Configuration Wizard

**File:** `src/config/deployment.rs`

In `DeploymentConfig::from_wizard()`, add the new layout option:

```rust
// Partition layout
let layouts = [
    PartitionLayout::Standard,
    PartitionLayout::Minimal,
    PartitionLayout::CryptoSubvolume,  // NEW
];
let layout_idx = prompt_select("Partition layout", &layouts, 0)?;
let layout = layouts[layout_idx].clone();

// Auto-enable encryption for CryptoSubvolume
let encryption = if layout == PartitionLayout::CryptoSubvolume {
    true
} else {
    prompt_confirm("Enable LUKS encryption?", false)?
};
```

---

## 4. Phase 2: New Partition Layout

### 4.1 Define CryptoSubvolume Layout Structure

**File:** `src/disk/layouts.rs`

Add new struct for subvolume definitions:

```rust
/// Btrfs subvolume definition
#[derive(Debug, Clone)]
pub struct SubvolumeDef {
    /// Subvolume name (e.g., "@", "@home")
    pub name: String,
    /// Mount point (e.g., "/", "/home")
    pub mount_point: String,
    /// Mount options
    pub mount_options: String,
}

/// Extended layout for LUKS + btrfs subvolumes
#[derive(Debug, Clone)]
pub struct CryptoSubvolumeLayout {
    /// EFI partition size in MiB
    pub efi_mib: u64,
    /// LUKS container partition number
    pub luks_partition: u32,
    /// Btrfs subvolumes to create
    pub subvolumes: Vec<SubvolumeDef>,
    /// Total disk size
    pub total_mib: u64,
}
```

### 4.2 Implement Layout Computation

**File:** `src/disk/layouts.rs`

```rust
/// BIOS Boot partition size (1 MiB, required for GRUB on GPT disks)
const BIOS_BOOT_MIB: u64 = 1;

/// Compute the crypto-subvolume layout (EFI + BIOS Boot + LUKS container)
/// Minimum 3 partitions required for reliable bootability
fn compute_crypto_subvolume_layout(disk_mib: u64) -> Result<ComputedLayout> {
    // Minimum: 512 MiB EFI + 1 MiB BIOS Boot + at least 20 GiB for root
    let min_total_mib = EFI_MIB + BIOS_BOOT_MIB + 20480;
    if disk_mib < min_total_mib {
        return Err(DeploytixError::DiskTooSmall {
            size_mib: disk_mib,
            required_mib: min_total_mib,
        });
    }

    let partitions = vec![
        // Partition 1: EFI System Partition
        PartitionDef {
            number: 1,
            name: "EFI".to_string(),
            size_mib: EFI_MIB,
            type_guid: partition_types::EFI.to_string(),
            mount_point: Some("/boot/efi".to_string()),
            is_swap: false,
            is_efi: true,
            is_luks: false,
            is_bios_boot: false,
            attributes: None,
        },
        // Partition 2: BIOS Boot (for GRUB legacy support on GPT)
        PartitionDef {
            number: 2,
            name: "BIOS".to_string(),
            size_mib: BIOS_BOOT_MIB,
            type_guid: partition_types::BIOS_BOOT.to_string(),
            mount_point: None, // Never mounted
            is_swap: false,
            is_efi: false,
            is_luks: false,
            is_bios_boot: true,
            attributes: Some("LegacyBIOSBootable".to_string()),
        },
        // Partition 3: LUKS Container (root with btrfs subvolumes)
        PartitionDef {
            number: 3,
            name: "LUKS".to_string(),
            size_mib: 0, // Remainder
            type_guid: partition_types::LINUX_FILESYSTEM.to_string(),
            mount_point: None, // Handled specially via LUKS
            is_swap: false,
            is_efi: false,
            is_luks: true,
            is_bios_boot: false,
            attributes: None,
        },
    ];

    Ok(ComputedLayout {
        partitions,
        total_mib: disk_mib,
        subvolumes: Some(default_subvolumes()),
    })
}

/// Default btrfs subvolumes per spec
pub fn default_subvolumes() -> Vec<SubvolumeDef> {
    vec![
        SubvolumeDef {
            name: "@".to_string(),
            mount_point: "/".to_string(),
            mount_options: "defaults,noatime".to_string(),
        },
        SubvolumeDef {
            name: "@usr".to_string(),
            mount_point: "/usr".to_string(),
            mount_options: "defaults,noatime".to_string(),
        },
        SubvolumeDef {
            name: "@var".to_string(),
            mount_point: "/var".to_string(),
            mount_options: "defaults,noatime".to_string(),
        },
        SubvolumeDef {
            name: "@home".to_string(),
            mount_point: "/home".to_string(),
            mount_options: "defaults,noatime".to_string(),
        },
        SubvolumeDef {
            name: "@boot".to_string(),
            mount_point: "/boot".to_string(),
            mount_options: "defaults,noatime".to_string(),
        },
    ]
}
```

### 4.3 Update PartitionDef

**File:** `src/disk/layouts.rs`

Add `is_luks` and `is_bios_boot` fields:

```rust
#[derive(Debug, Clone)]
pub struct PartitionDef {
    pub number: u32,
    pub name: String,
    pub size_mib: u64,
    pub type_guid: String,
    pub mount_point: Option<String>,
    pub is_swap: bool,
    pub is_efi: bool,
    pub is_luks: bool,       // NEW: LUKS container partition
    pub is_bios_boot: bool,  // NEW: BIOS Boot partition for GRUB legacy
    pub attributes: Option<String>,
}
```

### 4.4 Update compute_layout

**File:** `src/disk/layouts.rs`

```rust
pub fn compute_layout(layout: &PartitionLayout, disk_mib: u64) -> Result<ComputedLayout> {
    match layout {
        PartitionLayout::Standard => compute_standard_layout(disk_mib),
        PartitionLayout::Minimal => compute_minimal_layout(disk_mib),
        PartitionLayout::CryptoSubvolume => compute_crypto_subvolume_layout(disk_mib),
        PartitionLayout::Custom => Err(DeploytixError::ConfigError(
            "Custom layouts not yet implemented".to_string(),
        )),
    }
}
```

---

## 5. Phase 3: LUKS Encryption Implementation

### 5.1 Implement Full LUKS Setup

**File:** `src/configure/encryption.rs`

```rust
//! LUKS encryption setup

use crate::config::DeploymentConfig;
use crate::disk::detection::partition_path;
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use tracing::info;

/// LUKS container information
#[derive(Debug, Clone)]
pub struct LuksContainer {
    /// Source device (e.g., /dev/sda2)
    pub device: String,
    /// Mapper name (e.g., Crypt-Root)
    pub mapper_name: String,
    /// Mapped device path (e.g., /dev/mapper/Crypt-Root)
    pub mapped_path: String,
}

/// Setup LUKS encryption for the root partition
pub fn setup_encryption(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    luks_partition: u32,
) -> Result<LuksContainer> {
    if !config.disk.encryption {
        return Err(DeploytixError::ConfigError(
            "Encryption not enabled in configuration".to_string(),
        ));
    }

    let password = config
        .disk
        .encryption_password
        .as_ref()
        .ok_or_else(|| DeploytixError::ValidationError(
            "Encryption password required".to_string()
        ))?;

    let luks_device = partition_path(device, luks_partition);
    let mapper_name = &config.disk.luks_mapper_name;
    let mapped_path = format!("/dev/mapper/{}", mapper_name);

    info!("Setting up LUKS encryption on {}", luks_device);

    if cmd.is_dry_run() {
        println!("  [dry-run] cryptsetup luksFormat {}", luks_device);
        println!("  [dry-run] cryptsetup open {} {}", luks_device, mapper_name);
        return Ok(LuksContainer {
            device: luks_device,
            mapper_name: mapper_name.clone(),
            mapped_path,
        });
    }

    // Format LUKS container
    luks_format(cmd, &luks_device, password)?;

    // Open LUKS container
    luks_open(cmd, &luks_device, mapper_name, password)?;

    info!("LUKS encryption setup complete: {}", mapped_path);

    Ok(LuksContainer {
        device: luks_device,
        mapper_name: mapper_name.clone(),
        mapped_path,
    })
}

/// Format a device as LUKS
fn luks_format(cmd: &CommandRunner, device: &str, password: &str) -> Result<()> {
    info!("Formatting {} as LUKS container", device);

    // Use stdin to pass password securely
    let mut child = Command::new("cryptsetup")
        .args([
            "luksFormat",
            "--type", "luks2",
            "--cipher", "aes-xts-plain64",
            "--key-size", "512",
            "--hash", "sha256",
            "--pbkdf", "argon2id",
            "--batch-mode",
            device,
        ])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| DeploytixError::CommandFailed(format!("cryptsetup luksFormat: {}", e)))?;

    // Write password to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(password.as_bytes())?;
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(DeploytixError::CommandFailed(
            "cryptsetup luksFormat failed".to_string()
        ));
    }

    Ok(())
}

/// Open a LUKS container
fn luks_open(cmd: &CommandRunner, device: &str, mapper_name: &str, password: &str) -> Result<()> {
    info!("Opening LUKS container {} as {}", device, mapper_name);

    let mut child = Command::new("cryptsetup")
        .args(["open", device, mapper_name])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| DeploytixError::CommandFailed(format!("cryptsetup open: {}", e)))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(password.as_bytes())?;
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(DeploytixError::CommandFailed(
            "cryptsetup open failed".to_string()
        ));
    }

    // Wait for device to appear
    std::thread::sleep(std::time::Duration::from_millis(500));

    Ok(())
}

/// Close a LUKS container
pub fn close_luks(cmd: &CommandRunner, mapper_name: &str) -> Result<()> {
    info!("Closing LUKS container {}", mapper_name);

    if cmd.is_dry_run() {
        println!("  [dry-run] cryptsetup close {}", mapper_name);
        return Ok(());
    }

    cmd.run("cryptsetup", &["close", mapper_name])?;
    Ok(())
}

/// Get UUID of LUKS container
pub fn get_luks_uuid(device: &str) -> Result<String> {
    let output = Command::new("cryptsetup")
        .args(["luksUUID", device])
        .output()
        .map_err(|e| DeploytixError::CommandFailed(format!("cryptsetup luksUUID: {}", e)))?;

    if !output.status.success() {
        return Err(DeploytixError::CommandFailed(
            "Failed to get LUKS UUID".to_string()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
```

---

## 6. Phase 4: Btrfs Subvolume Management

### 6.1 Create Subvolume Functions

**File:** `src/disk/formatting.rs`

Add new functions:

```rust
use crate::disk::layouts::SubvolumeDef;

/// Create btrfs filesystem on a device
pub fn create_btrfs_filesystem(
    cmd: &CommandRunner,
    device: &str,
    label: &str,
) -> Result<()> {
    info!("Creating btrfs filesystem on {} with label {}", device, label);

    if cmd.is_dry_run() {
        println!("  [dry-run] mkfs.btrfs -L {} {}", label, device);
        return Ok(());
    }

    cmd.run("mkfs.btrfs", &["-f", "-L", label, device])?;
    Ok(())
}

/// Create btrfs subvolumes
pub fn create_btrfs_subvolumes(
    cmd: &CommandRunner,
    device: &str,
    subvolumes: &[SubvolumeDef],
    temp_mount: &str,
) -> Result<()> {
    info!("Creating btrfs subvolumes on {}", device);

    if cmd.is_dry_run() {
        for sv in subvolumes {
            println!("  [dry-run] btrfs subvolume create {}/{}", temp_mount, sv.name);
        }
        return Ok(());
    }

    // Create temp mount point
    std::fs::create_dir_all(temp_mount)?;

    // Mount the btrfs root
    cmd.run("mount", &[device, temp_mount])?;

    // Create each subvolume
    for sv in subvolumes {
        let subvol_path = format!("{}/{}", temp_mount, sv.name);
        cmd.run("btrfs", &["subvolume", "create", &subvol_path])?;
        info!("Created subvolume: {}", sv.name);
    }

    // Unmount
    cmd.run("umount", &[temp_mount])?;

    Ok(())
}

/// Mount btrfs subvolumes for installation
pub fn mount_btrfs_subvolumes(
    cmd: &CommandRunner,
    device: &str,
    subvolumes: &[SubvolumeDef],
    install_root: &str,
) -> Result<()> {
    info!("Mounting btrfs subvolumes to {}", install_root);

    if cmd.is_dry_run() {
        for sv in subvolumes {
            println!(
                "  [dry-run] mount -o subvol={},{} {} {}{}",
                sv.name, sv.mount_options, device, install_root, sv.mount_point
            );
        }
        return Ok(());
    }

    // Sort subvolumes by mount point depth (root first)
    let mut sorted_subvolumes = subvolumes.to_vec();
    sorted_subvolumes.sort_by(|a, b| {
        a.mount_point.matches('/').count()
            .cmp(&b.mount_point.matches('/').count())
    });

    for sv in &sorted_subvolumes {
        let target = format!("{}{}", install_root, sv.mount_point);
        std::fs::create_dir_all(&target)?;

        let options = format!("subvol={},{}", sv.name, sv.mount_options);
        cmd.run("mount", &["-o", &options, device, &target])?;
        info!("Mounted {} to {}", sv.name, target);
    }

    Ok(())
}
```

---

## 7. Phase 5: Custom mkinitcpio Hooks

### 7.1 Create Hooks Module

**File:** `src/configure/hooks.rs` (NEW FILE)

```rust
//! Custom mkinitcpio hook generation

use crate::config::DeploymentConfig;
use crate::disk::layouts::ComputedLayout;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
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

        // Write install script
        let install_path = format!("{}/{}", install_dir, hook.name);
        fs::write(&install_path, &hook.install_content)?;

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
        hooks.push(generate_crypttab_unlock_hook(config)?);
        hooks.push(generate_mountcrypt_hook(config, layout)?);
    }

    Ok(hooks)
}

/// Generate the crypttab-unlock hook
fn generate_crypttab_unlock_hook(config: &DeploymentConfig) -> Result<GeneratedHook> {
    let hook_content = include_str!("../../ref/hooks_crypttab-unlock").to_string();
    let install_content = include_str!("../../ref/install_crypttab-unlock").to_string();

    Ok(GeneratedHook {
        name: "crypttab-unlock".to_string(),
        hook_content,
        install_content,
    })
}

/// Generate the mountcrypt hook (potentially customized based on layout)
fn generate_mountcrypt_hook(
    config: &DeploymentConfig,
    layout: &ComputedLayout,
) -> Result<GeneratedHook> {
    let hook_content = generate_mountcrypt_script(config, layout);
    let install_content = include_str!("../../ref/install_mountcrypt").to_string();

    Ok(GeneratedHook {
        name: "mountcrypt".to_string(),
        hook_content,
        install_content,
    })
}

/// Generate mountcrypt script based on layout
fn generate_mountcrypt_script(
    config: &DeploymentConfig,
    layout: &ComputedLayout,
) -> String {
    let mapper_name = &config.disk.luks_mapper_name;

    let subvolume_mounts = if let Some(ref subvols) = layout.subvolumes {
        subvols
            .iter()
            .filter(|sv| sv.mount_point != "/")
            .map(|sv| {
                format!(
                    r#"    mkdir -p "$new_root{mp}"
    mount -o rw,subvol={name} "$cryptroot" "$new_root{mp}""#,
                    mp = sv.mount_point,
                    name = sv.name
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        String::new()
    };

    format!(
        r#"#!/bin/ash
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
        echo "Error mounting root subvolume" >&2
        return 1
    }}

    # Mount additional subvolumes
{subvol_mounts}

    # Auto-detect and mount EFI partition
    mkdir -p "$new_root/boot/efi"
    efi_partition=$(blkid -t TYPE=vfat -o device | head -n1)
    if [ -n "$efi_partition" ] && [ -b "$efi_partition" ]; then
        mount -o rw "$efi_partition" "$new_root/boot/efi"
    fi
}}
"#,
        mapper = mapper_name,
        subvol_mounts = subvolume_mounts
    )
}
```

### 7.2 Update mkinitcpio.conf Generation

**File:** `src/configure/mkinitcpio.rs`

```rust
/// Construct the HOOKS array based on configuration
pub fn construct_hooks(config: &DeploymentConfig) -> Vec<String> {
    let mut hooks = vec![
        "base".to_string(),
        "udev".to_string(),
        "autodetect".to_string(),
        "modconf".to_string(),
        "block".to_string(),
        "keyboard".to_string(),
    ];

    // For CryptoSubvolume layout, use custom hooks
    if config.disk.layout == PartitionLayout::CryptoSubvolume && config.disk.encryption {
        hooks.push("crypttab-unlock".to_string());
        hooks.push("mountcrypt".to_string());
    } else if config.disk.encryption {
        // Standard encrypt hook for other layouts
        hooks.push("encrypt".to_string());
    }

    // Note: filesystems hook is NOT needed when using mountcrypt
    // as mountcrypt handles all mounting
    if config.disk.layout != PartitionLayout::CryptoSubvolume {
        hooks.push("filesystems".to_string());
        hooks.push("fsck".to_string());
    }

    hooks
}
```

---

## 8. Phase 6: Crypttab Generation

### 8.1 Create Crypttab Module

**File:** `src/install/crypttab.rs` (NEW FILE)

```rust
//! Crypttab generation for LUKS containers

use crate::config::DeploymentConfig;
use crate::configure::encryption::get_luks_uuid;
use crate::disk::detection::partition_path;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// Generate /etc/crypttab for the installed system
pub fn generate_crypttab(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    luks_partition: u32,
    install_root: &str,
) -> Result<()> {
    info!("Generating /etc/crypttab");

    let luks_device = partition_path(device, luks_partition);
    let mapper_name = config
        .disk
        .luks_mapper_name
        .trim_start_matches("Crypt-")
        .to_string();

    if cmd.is_dry_run() {
        println!("  [dry-run] Would generate /etc/crypttab");
        return Ok(());
    }

    let uuid = get_luks_uuid(&luks_device)?;

    // Determine keyfile path
    let keyfile = config
        .disk
        .keyfile_path
        .as_deref()
        .unwrap_or("none");

    let content = format!(
        r#"# /etc/crypttab - Generated by Deploytix
# <name>    <device>              <keyfile>    <options>
{name}    UUID={uuid}    {keyfile}    luks,discard
"#,
        name = mapper_name,
        uuid = uuid,
        keyfile = keyfile,
    );

    let crypttab_path = format!("{}/etc/crypttab", install_root);
    fs::create_dir_all(format!("{}/etc", install_root))?;
    fs::write(&crypttab_path, content)?;

    info!("Crypttab written to {}", crypttab_path);
    Ok(())
}
```

---

## 9. Phase 7: Fstab Generation

### 9.1 Update Fstab for Btrfs Subvolumes

**File:** `src/install/fstab.rs`

Add function for crypto-subvolume layout:

```rust
/// Generate fstab for CryptoSubvolume layout
pub fn generate_fstab_crypto_subvolume(
    cmd: &CommandRunner,
    mapped_device: &str,
    efi_device: &str,
    subvolumes: &[SubvolumeDef],
    install_root: &str,
) -> Result<()> {
    info!("Generating fstab for encrypted btrfs subvolumes");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would generate fstab with btrfs subvolumes");
        return Ok(());
    }

    let btrfs_uuid = get_partition_uuid(mapped_device)?;
    let efi_uuid = get_partition_uuid(efi_device)?;

    let mut content = String::from(
        "# /etc/fstab: static file system information.\n\
         # Generated by Deploytix\n\
         #\n\
         # <file system> <mount point> <type> <options> <dump> <pass>\n\n\
         # Encrypted btrfs root (mapped as /dev/mapper/Crypt-Root in initramfs)\n"
    );

    // Add subvolume entries
    for sv in subvolumes {
        let pass = if sv.mount_point == "/" { 1 } else { 0 };
        content.push_str(&format!(
            "UUID={uuid}  {mp}  btrfs  subvol={name},{opts}  0  {pass}\n",
            uuid = btrfs_uuid,
            mp = sv.mount_point,
            name = sv.name,
            opts = sv.mount_options,
            pass = pass,
        ));
    }

    // Add EFI entry
    content.push_str(&format!(
        "\n# EFI System Partition\n\
         UUID={}  /boot/efi  vfat  umask=0077,defaults  0  2\n",
        efi_uuid
    ));

    let fstab_path = format!("{}/etc/fstab", install_root);
    fs::create_dir_all(format!("{}/etc", install_root))?;
    fs::write(&fstab_path, content)?;

    info!("Fstab written to {}", fstab_path);
    Ok(())
}
```

---

## 10. Phase 8: Bootloader Configuration

### 10.1 Update GRUB for LUKS

**File:** `src/configure/bootloader.rs`

```rust
/// Configure GRUB defaults for encrypted root
fn configure_grub_defaults(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    luks_partition: u32,
    install_root: &str,
) -> Result<()> {
    let grub_default_path = format!("{}/etc/default/grub", install_root);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure /etc/default/grub for LUKS");
        return Ok(());
    }

    let luks_device = partition_path(device, luks_partition);
    let luks_uuid = get_luks_uuid(&luks_device)?;
    let mapper_name = &config.disk.luks_mapper_name;

    // Build kernel command line
    let mut cmdline_parts = vec![
        "quiet".to_string(),
        format!("cryptdevice=UUID={}:{}", luks_uuid, mapper_name),
        format!("root=/dev/mapper/{}", mapper_name),
        "rootflags=subvol=@".to_string(),
        "rw".to_string(),
    ];

    let cmdline = cmdline_parts.join(" ");

    let content = format!(
        r#"# GRUB boot loader configuration
# Generated by Deploytix

GRUB_DEFAULT=0
GRUB_TIMEOUT=5
GRUB_DISTRIBUTOR="Artix"
GRUB_CMDLINE_LINUX_DEFAULT="{cmdline}"
GRUB_ENABLE_CRYPTODISK=y
"#,
        cmdline = cmdline
    );

    fs::write(&grub_default_path, content)?;

    Ok(())
}

/// Install GRUB for encrypted system
fn install_grub_encrypted(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    luks_partition: u32,
    install_root: &str,
) -> Result<()> {
    info!("Installing GRUB bootloader for encrypted system");

    // Configure GRUB defaults
    configure_grub_defaults(cmd, config, device, luks_partition, install_root)?;

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] grub-install --target=x86_64-efi --efi-directory=/boot/efi --bootloader-id=Artix"
        );
        println!("  [dry-run] grub-mkconfig -o /boot/grub/grub.cfg");
        return Ok(());
    }

    // Install GRUB
    cmd.run_in_chroot(
        install_root,
        "grub-install --target=x86_64-efi --efi-directory=/boot/efi --bootloader-id=Artix",
    )?;

    // Generate config
    cmd.run_in_chroot(install_root, "grub-mkconfig -o /boot/grub/grub.cfg")?;

    info!("GRUB installation complete");
    Ok(())
}
```

---

## 11. Phase 9: Service Configuration

### 11.1 Create Greetd Configuration Module

**File:** `src/configure/greetd.rs` (NEW FILE)

```rust
//! greetd display manager configuration

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// Configure greetd for auto-login to Plasma
pub fn configure_greetd(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    info!("Configuring greetd");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would configure /etc/greetd/config.toml");
        return Ok(());
    }

    let username = &config.user.name;

    // Determine session command based on desktop environment
    let session_cmd = match &config.desktop.environment {
        crate::config::DesktopEnvironment::Kde => "startplasma-wayland",
        crate::config::DesktopEnvironment::Gnome => "gnome-session",
        crate::config::DesktopEnvironment::Xfce => "startxfce4",
        crate::config::DesktopEnvironment::None => return Ok(()), // No greetd for headless
    };

    let config_content = format!(
        r#"[terminal]
vt = 1

[default_session]
command = "{session}"
user = "{user}"
"#,
        session = session_cmd,
        user = username,
    );

    let greetd_dir = format!("{}/etc/greetd", install_root);
    fs::create_dir_all(&greetd_dir)?;
    fs::write(format!("{}/config.toml", greetd_dir), config_content)?;

    info!("greetd configured for user: {}", username);
    Ok(())
}
```

### 11.2 NetworkManager Build Module (for iwd backend)

**File:** `src/configure/networkmanager.rs` (NEW FILE)

When NetworkManager + iwd is selected, the installer must build NetworkManager from source with iwd support:

```rust
//! NetworkManager build-from-source module
//! Required when using NetworkManager with iwd as wifi backend

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use tracing::info;

const NM_GIT_REPO: &str = "https://github.com/NetworkManager/NetworkManager";
const NM_VERSION: &str = "1.48.0";  // Pin to stable version

/// Build and install NetworkManager with iwd support
pub fn build_networkmanager_with_iwd(
    cmd: &CommandRunner,
    install_root: &str,
) -> Result<()> {
    info!("Building NetworkManager with iwd backend support");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would clone and build NetworkManager with -Diwd=true");
        return Ok(());
    }

    let build_dir = "/tmp/deploytix-nm-build";

    // Clone NetworkManager
    cmd.run("git", &[
        "clone",
        "--depth", "1",
        "--branch", NM_VERSION,
        NM_GIT_REPO,
        build_dir,
    ])?;

    // Configure with meson (see ref/meson_options.txt for options)
    // Key options:
    //   -Diwd=true                          (line 33: enable iwd support)
    //   -Dconfig_wifi_backend_default=iwd   (line 26: set iwd as default)
    //   -Dsession_tracking=elogind          (for non-systemd systems)
    //   -Dsystemd_journal=false             (disable systemd journal)
    cmd.run_in_dir(build_dir, "meson", &[
        "setup",
        "build",
        "-Diwd=true",
        "-Dconfig_wifi_backend_default=iwd",
        "-Dsession_tracking=elogind",
        "-Dsystemd_journal=false",
        "-Dsuspend_resume=elogind",
        "-Dpolkit=true",
        "-Dmodify_system=true",
        "--prefix=/usr",
        "--sysconfdir=/etc",
        "--localstatedir=/var",
    ])?;

    // Build
    cmd.run_in_dir(&format!("{}/build", build_dir), "ninja", &[])?;

    // Install to target root
    cmd.run_in_dir(
        &format!("{}/build", build_dir),
        "meson",
        &["install", "--destdir", install_root],
    )?;

    // Cleanup
    std::fs::remove_dir_all(build_dir)?;

    info!("NetworkManager built and installed with iwd support");
    Ok(())
}

/// Check if NetworkManager needs to be built from source
pub fn needs_source_build(config: &DeploymentConfig) -> bool {
    config.network.backend == NetworkBackend::NetworkManager
}
```

### 11.3 Update Services Module

**File:** `src/configure/services.rs`

Add seatd and greetd enabling:

```rust
/// Enable services for the installed system
pub fn enable_services(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    info!("Enabling services");

    let mut services = Vec::new();

    // seatd is required for wayland compositors
    if config.desktop.environment != DesktopEnvironment::None {
        services.push("seatd");
    }

    // Display manager (only with desktop)
    if config.desktop.environment != DesktopEnvironment::None {
        services.push("greetd");
    }

    // Network services
    match config.network.backend {
        NetworkBackend::Iwd => services.push("iwd"),
        NetworkBackend::NetworkManager => {
            // NetworkManager is built from source with iwd support
            // (see networkmanager.rs and ref/meson_options.txt)
            services.push("NetworkManager");
            services.push("iwd");  // iwd as backend for NetworkManager
        }
        NetworkBackend::Connman => services.push("connmand"),
    }

    // DNS
    if config.network.dns == DnsProvider::DnscryptProxy {
        services.push("dnscrypt-proxy");
    }

    // Enable services based on init system
    match config.system.init {
        InitSystem::Runit => enable_runit_services(cmd, &services, install_root)?,
        _ => {
            return Err(DeploytixError::NotImplemented(
                "Only runit service enabling is implemented".to_string()
            ));
        }
    }

    Ok(())
}

/// Enable services for runit
fn enable_runit_services(
    cmd: &CommandRunner,
    services: &[&str],
    install_root: &str,
) -> Result<()> {
    let sv_dir = format!("{}/etc/runit/sv", install_root);
    let enabled_dir = format!("{}/run/runit/service", install_root);

    if cmd.is_dry_run() {
        for service in services {
            println!("  [dry-run] ln -s {}/{} {}/{}", sv_dir, service, enabled_dir, service);
        }
        return Ok(());
    }

    fs::create_dir_all(&enabled_dir)?;

    for service in services {
        let src = format!("{}/{}", sv_dir, service);
        let dst = format!("{}/{}", enabled_dir, service);

        if std::path::Path::new(&src).exists() {
            // Use symlink
            std::os::unix::fs::symlink(&src, &dst)?;
            info!("Enabled service: {}", service);
        } else {
            tracing::warn!("Service not found: {}", service);
        }
    }

    Ok(())
}
```

---

## 12. Phase 10: Installer Workflow Integration

### 12.1 Update Installer for Crypto-Subvolume

**File:** `src/install/installer.rs`

```rust
impl Installer {
    /// Run the full installation process
    pub fn run(mut self) -> Result<()> {
        info!("Starting Deploytix installation");

        // Phase 1: Preparation
        self.prepare()?;

        // Phase 2: Disk operations
        self.partition_disk()?;

        // Phase 2.5: LUKS setup (for CryptoSubvolume layout)
        if self.config.disk.layout == PartitionLayout::CryptoSubvolume {
            self.setup_encryption()?;
            self.create_btrfs_subvolumes()?;
            self.mount_crypto_subvolumes()?;
        } else {
            self.format_partitions()?;
            self.mount_partitions()?;
        }

        // Phase 3: Base system
        self.install_base_system()?;
        self.generate_fstab()?;

        // Phase 3.5: Crypttab (for encrypted systems)
        if self.config.disk.encryption {
            self.generate_crypttab()?;
        }

        // Phase 4: System configuration
        self.configure_system()?;

        // Phase 4.5: Custom hooks (for encrypted systems)
        if self.config.disk.encryption {
            self.install_custom_hooks()?;
        }

        // Phase 4.6: Build NetworkManager from source (if NetworkManager + iwd selected)
        if crate::configure::networkmanager::needs_source_build(&self.config) {
            self.build_networkmanager()?;
        }

        // Phase 5: Desktop environment (optional)
        self.install_desktop()?;

        // Phase 6: Finalization
        self.finalize()?;

        info!("Installation complete!");
        println!("\n✓ Installation completed successfully!");
        println!("  You can now reboot into your new Artix Linux system.");

        Ok(())
    }

    /// Setup LUKS encryption
    fn setup_encryption(&mut self) -> Result<()> {
        info!("Setting up LUKS encryption");

        let layout = self.layout.as_ref().unwrap();
        let luks_part = layout
            .partitions
            .iter()
            .find(|p| p.is_luks)
            .ok_or_else(|| DeploytixError::ConfigError("No LUKS partition found".to_string()))?;

        let container = crate::configure::encryption::setup_encryption(
            &self.cmd,
            &self.config,
            &self.config.disk.device,
            luks_part.number,
        )?;

        self.luks_container = Some(container);
        Ok(())
    }

    /// Create btrfs subvolumes inside LUKS container
    fn create_btrfs_subvolumes(&self) -> Result<()> {
        info!("Creating btrfs filesystem and subvolumes");

        let container = self.luks_container.as_ref().unwrap();
        let layout = self.layout.as_ref().unwrap();

        // Create btrfs filesystem
        crate::disk::formatting::create_btrfs_filesystem(
            &self.cmd,
            &container.mapped_path,
            "ROOT",
        )?;

        // Create subvolumes
        if let Some(ref subvolumes) = layout.subvolumes {
            crate::disk::formatting::create_btrfs_subvolumes(
                &self.cmd,
                &container.mapped_path,
                subvolumes,
                "/tmp/deploytix-btrfs",
            )?;
        }

        Ok(())
    }

    /// Mount btrfs subvolumes
    fn mount_crypto_subvolumes(&self) -> Result<()> {
        info!("Mounting btrfs subvolumes");

        let container = self.luks_container.as_ref().unwrap();
        let layout = self.layout.as_ref().unwrap();

        if let Some(ref subvolumes) = layout.subvolumes {
            crate::disk::formatting::mount_btrfs_subvolumes(
                &self.cmd,
                &container.mapped_path,
                subvolumes,
                INSTALL_ROOT,
            )?;
        }

        // Mount EFI partition
        let efi_part = layout
            .partitions
            .iter()
            .find(|p| p.is_efi)
            .ok_or_else(|| DeploytixError::ConfigError("No EFI partition found".to_string()))?;

        let efi_device = partition_path(&self.config.disk.device, efi_part.number);
        let efi_mount = format!("{}/boot/efi", INSTALL_ROOT);
        std::fs::create_dir_all(&efi_mount)?;
        self.cmd.run("mount", &[&efi_device, &efi_mount])?;

        Ok(())
    }

    /// Generate crypttab
    fn generate_crypttab(&self) -> Result<()> {
        let layout = self.layout.as_ref().unwrap();
        let luks_part = layout
            .partitions
            .iter()
            .find(|p| p.is_luks)
            .ok_or_else(|| DeploytixError::ConfigError("No LUKS partition found".to_string()))?;

        crate::install::crypttab::generate_crypttab(
            &self.cmd,
            &self.config,
            &self.config.disk.device,
            luks_part.number,
            INSTALL_ROOT,
        )
    }

    /// Install custom mkinitcpio hooks
    fn install_custom_hooks(&self) -> Result<()> {
        let layout = self.layout.as_ref().unwrap();

        crate::configure::hooks::install_custom_hooks(
            &self.cmd,
            &self.config,
            layout,
            INSTALL_ROOT,
        )
    }

    /// Build NetworkManager from source with iwd support
    fn build_networkmanager(&self) -> Result<()> {
        crate::configure::networkmanager::build_networkmanager_with_iwd(
            &self.cmd,
            INSTALL_ROOT,
        )
    }
}
```

---

## 13. Phase 11: Testing & Validation

### 13.1 End-State Validation Checklist

After installation, verify:

- [ ] GRUB loads kernel + initramfs from encrypted `/boot`
- [ ] initramfs parses `/etc/crypttab`
- [ ] LUKS container unlocks to `/dev/mapper/Crypt-Root`
- [ ] Btrfs subvolumes mount correctly:
  - [ ] `@` → `/`
  - [ ] `@usr` → `/usr`
  - [ ] `@var` → `/var`
  - [ ] `@home` → `/home`
  - [ ] `@boot` → `/boot`
- [ ] EFI mounts to `/boot/efi`
- [ ] `switch_root` succeeds
- [ ] runit reaches stage 2
- [ ] seatd starts before greetd (if desktop selected)
- [ ] greetd launches desktop session (if desktop selected)
- [ ] NetworkManager built with iwd backend (if selected)

### 13.2 Testing Commands

```bash
# Verify LUKS UUID in GRUB config
grep -E "cryptdevice|GRUB_ENABLE_CRYPTODISK" /boot/grub/grub.cfg

# Verify crypttab
cat /etc/crypttab

# Verify fstab
cat /etc/fstab

# Verify mkinitcpio hooks
grep HOOKS /etc/mkinitcpio.conf

# Check custom hooks exist
ls -la /usr/lib/initcpio/hooks/crypttab-unlock
ls -la /usr/lib/initcpio/hooks/mountcrypt

# Verify services
ls -la /run/runit/service/

# Test initramfs generation
mkinitcpio -P
```

### 13.3 Integration Test (Dry-Run)

```bash
# Test full workflow in dry-run mode
sudo ./target/release/deploytix -n install -c test-crypto.toml
```

Example `test-crypto.toml`:
```toml
[disk]
device = "/dev/sda"
layout = "cryptosubvolume"
filesystem = "btrfs"
encryption = true
encryption_password = "testpassword"
luks_mapper_name = "Crypt-Root"

[system]
init = "runit"
bootloader = "grub"
timezone = "UTC"
locale = "en_US.UTF-8"
keymap = "us"
hostname = "artix-test"

[user]
name = "testuser"
password = "testpassword"

[network]
backend = "iwd"
dns = "dnscrypt-proxy"

[desktop]
environment = "kde"
```

---

## Appendix: File Change Summary

### Modified Files

| File | Changes |
|------|---------|
| `src/config/deployment.rs` | Add `CryptoSubvolume` layout, `luks_mapper_name` field, validation |
| `src/disk/layouts.rs` | Add `SubvolumeDef`, `compute_crypto_subvolume_layout()`, `is_luks` field |
| `src/disk/formatting.rs` | Add `create_btrfs_filesystem()`, `create_btrfs_subvolumes()`, `mount_btrfs_subvolumes()` |
| `src/configure/encryption.rs` | Full implementation of `setup_encryption()`, `luks_format()`, `luks_open()` |
| `src/configure/mkinitcpio.rs` | Update `construct_hooks()` for custom hooks |
| `src/configure/bootloader.rs` | Add `configure_grub_defaults()` for LUKS, `install_grub_encrypted()` |
| `src/configure/services.rs` | Add seatd/greetd enabling, `enable_runit_services()` |
| `src/install/fstab.rs` | Add `generate_fstab_crypto_subvolume()` |
| `src/install/installer.rs` | Add encrypted workflow phases |
| `src/configure/mod.rs` | Export new modules |
| `src/install/mod.rs` | Export crypttab module |

### New Files

| File | Purpose |
|------|---------|
| `src/configure/hooks.rs` | Custom mkinitcpio hook generation |
| `src/install/crypttab.rs` | `/etc/crypttab` generation |
| `src/configure/greetd.rs` | greetd configuration |
| `src/configure/networkmanager.rs` | Build NetworkManager from source with iwd support |

### Module Exports

**File:** `src/configure/mod.rs`
```rust
pub mod hooks;
pub mod greetd;
pub mod networkmanager;
```

**File:** `src/install/mod.rs`
```rust
pub mod crypttab;
pub use crypttab::generate_crypttab;
```

---

*Documentation generated: 2026-01-30*
*Based on: `docs/artix-runit-crypto-install-spec.md`*
