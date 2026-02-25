# Plan: Custom Partition Layout

---

> **Previous plan (SecureBoot) follows below this section.**

---

## Objective

Implement the `Custom` partition layout variant (`PartitionLayout::Custom`), which lets users declare an arbitrary list of data partitions — one per top-level directory on the filesystem — each with a user-specified size.  The system always prepends EFI and Boot partitions and appends a Swap partition when configured.  A partition with `size_mib = 0` consumes the remaining disk space.

---

## Background and Current State

| Location | Situation |
|----------|-----------|
| `src/config/deployment.rs:172` | `PartitionLayout::Custom` variant exists |
| `src/disk/layouts.rs:539` | Matched in `compute_layout()` but immediately returns `Err("Custom layouts not yet implemented")` |
| `src/disk/layouts.rs:68–91` | `PartitionDef` is fully generic; no changes needed |
| `src/disk/layouts.rs:94–107` | `ComputedLayout` is fully generic; no changes needed |
| `src/config/deployment.rs:21–88` | `DiskConfig` has no field for user-provided partition specs |
| `src/disk/partitioning.rs` | `generate_sfdisk_script()` already handles `size_mib = 0` as "remainder" |
| `src/install/installer.rs` | Calls `compute_layout()` — needs updated call site |

---

## Step 1 — Add `CustomPartitionEntry` struct (`src/config/deployment.rs`)

Add a new serde-serializable struct **above** the `DiskConfig` definition:

```rust
/// One user-defined data partition for PartitionLayout::Custom.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomPartitionEntry {
    /// Root-relative mount point, e.g. "/", "/home", "/var", "/data".
    pub mount_point: String,

    /// Partition label (e.g. "ROOT", "HOME").
    /// If omitted, derived from the last path component, uppercased.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Size in MiB.  Set to 0 to consume all remaining disk space.
    /// Exactly one entry in the list may be 0.
    pub size_mib: u64,

    /// Per-partition encryption override.  Inherits `disk.encryption` when None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encryption: Option<bool>,
}
```

---

## Step 2 — Add `custom_partitions` field to `DiskConfig` (`src/config/deployment.rs`)

Insert after `use_subvolumes`:

```rust
/// Partition list for PartitionLayout::Custom.
/// EFI, Boot, and Swap are always prepended by the system.
#[serde(skip_serializing_if = "Option::is_none")]
pub custom_partitions: Option<Vec<CustomPartitionEntry>>,
```

Propagate `custom_partitions: None` through every `DiskConfig` literal in `sample()` and `from_wizard()` (non-custom paths).

---

## Step 3 — Implement `compute_custom_layout()` (`src/disk/layouts.rs`)

New private function:

```rust
fn compute_custom_layout(
    disk_mib: u64,
    encryption: bool,
    use_swap_partition: bool,
    entries: &[CustomPartitionEntry],
) -> Result<ComputedLayout>
```

**Algorithm:**

1. **System partitions** (always first):
   - Partition 1 — EFI: 512 MiB, `/boot/efi`, `is_efi = true`
   - Partition 2 — Boot: 2048 MiB, `/boot`, `is_boot_fs = true`, `LegacyBIOSBootable`
   - Partition 3 (conditional on `use_swap_partition`) — Swap: `calculate_swap_mib(get_ram_mib())`, `is_swap = true`

2. **Reserved MiB** = `EFI_MIB + BOOT_MIB + swap_mib`

3. **Validate fixed-size total**: sum all `entry.size_mib > 0`, add `reserved_mib`, assert < `disk_mib`.  Return `DeploytixError::DiskTooSmall` if not.

4. **Remainder check**: count entries where `size_mib == 0`.  If > 1, return `DeploytixError::ConfigError("Only one custom partition may specify size_mib = 0 (remainder)")`.

5. **Assign GPT type GUIDs** by `mount_point`:

   | Mount point | GUID constant |
   |-------------|---------------|
   | `/` | `LINUX_ROOT_X86_64` |
   | `/usr` | `LINUX_USR_X86_64` |
   | `/var` | `LINUX_VAR` |
   | `/home` | `LINUX_HOME` |
   | anything else | `LINUX_FILESYSTEM` |

6. **Derive label**: if `entry.label.is_none()`, uppercase the last non-empty path segment (`/` → `"ROOT"`, `/home` → `"HOME"`, `/var/log` → `"LOG"`).

7. **Assign partition numbers** starting from `next_part_num` (3 if no swap, 4 if swap).

8. **Set `is_luks`** = `entry.encryption.unwrap_or(encryption)`.

9. **Align sizes**: `floor_align(size_mib, ALIGN_MIB)` for each fixed entry; remainder entry stays `0`.

10. Return `ComputedLayout { partitions, total_mib: disk_mib, subvolumes: None }`.

---

## Step 4 — Update `compute_layout()` signature (`src/disk/layouts.rs`)

Add a fourth parameter and route the Custom arm:

```rust
pub fn compute_layout(
    layout: &PartitionLayout,
    disk_mib: u64,
    encryption: bool,
    custom_partitions: Option<&[CustomPartitionEntry]>,   // NEW
) -> Result<ComputedLayout> {
    match layout {
        PartitionLayout::Standard => compute_standard_layout(disk_mib, encryption),
        PartitionLayout::Minimal  => compute_minimal_layout(disk_mib),
        PartitionLayout::LvmThin  => compute_lvm_thin_layout(disk_mib, true),
        PartitionLayout::Custom   => {
            let entries = custom_partitions.ok_or_else(|| {
                DeploytixError::ConfigError("Custom layout requires custom_partitions".into())
            })?;
            compute_custom_layout(disk_mib, encryption, true, entries)
        }
    }
}
```

`use_swap_partition` for the Custom arm should reflect the caller's swap type.  The cleanest approach is to also pass `use_swap_partition: bool` to `compute_layout()` (making it a fifth parameter, matching the existing `compute_lvm_thin_layout_with_swap` pattern) so the Custom and LvmThin branches both use it consistently.

---

## Step 5 — Validation rules (`src/config/deployment.rs`)

Append to `DeploymentConfig::validate()` when `self.disk.layout == PartitionLayout::Custom`:

```
a. custom_partitions must be Some(_) and non-empty.
b. Exactly one entry must have mount_point == "/".
c. Every mount_point must start with '/'.
d. mount_point must not be "/boot" or "/boot/efi" (reserved for system partitions).
e. No duplicate mount_points across the list.
f. At most one entry may have size_mib == 0.
g. A per-entry encryption = true requires disk.encryption = true.
```

---

## Step 6 — Wizard integration (`src/config/deployment.rs`)

In `from_wizard()`, after the user selects `PartitionLayout::Custom`, insert an interactive loop:

```
Info: EFI (512 MiB), Boot (2 GiB), and Swap partitions are prepended automatically.

? Mount point (e.g. /, /home, /var): /
? Size in MiB (0 = remaining space): 30720
? Partition label [ROOT]: (enter to accept)
? Add another partition? [Y/n]: y

? Mount point: /home
? Size in MiB (0 = remaining space): 0
? Partition label [HOME]: (enter to accept)
? Add another partition? [Y/n]: n
```

Before breaking the loop, validate that at least one entry with `mount_point == "/"` exists; if not, warn and require the user to add one.

Assign the collected `Vec<CustomPartitionEntry>` to `disk.custom_partitions`.

---

## Step 7 — Installer call site (`src/install/installer.rs`)

In the partition layout computation phase, update the call to pass the new parameters:

```rust
let layout = compute_layout(
    &self.config.disk.layout,
    disk_mib,
    self.config.disk.encryption,
    self.config.disk.custom_partitions.as_deref(),   // NEW
)?;
```

---

## Step 8 — TOML documentation (`README.md`)

Add an example block to the existing configuration snippet in the README showing a custom layout:

```toml
[disk]
device   = "/dev/sda"
layout   = "custom"
filesystem = "ext4"
encryption = false

[[disk.custom_partitions]]
mount_point = "/"
size_mib    = 30720      # 30 GiB

[[disk.custom_partitions]]
mount_point = "/var"
size_mib    = 10240      # 10 GiB

[[disk.custom_partitions]]
mount_point = "/home"
size_mib    = 0          # consumes all remaining space
```

---

## File Change Summary

| File | Change |
|------|--------|
| `src/config/deployment.rs` | Add `CustomPartitionEntry`; add `custom_partitions` field to `DiskConfig`; extend `validate()`; extend `from_wizard()` |
| `src/disk/layouts.rs` | Add `compute_custom_layout()`; update `compute_layout()` signature (add `custom_partitions` and `use_swap_partition` params) |
| `src/install/installer.rs` | Update `compute_layout()` call site |
| `README.md` | Document custom layout TOML syntax |

No changes required in `partitioning.rs`, `formatting.rs`, `fstab.rs`, `chroot.rs`, or any encryption/LVM module — all downstream code operates on the generic `ComputedLayout`/`PartitionDef` types.

---

## Design Decisions

- **EFI and Boot are always system-managed.** Users cannot override or remove them via `custom_partitions`. This preserves bootloader compatibility guarantees.
- **`size_mib = 0` means "remainder"**, consistent with the existing convention in `PartitionDef` and `generate_sfdisk_script()`.
- **Exactly one remainder partition.** More than one would make sizing indeterminate; zero remainder partitions are allowed (all partitions fixed-size) but a warning should be surfaced if no `/` partition fills the disk.
- **Per-partition encryption override** follows the existing `is_luks` flag pattern and is compatible with `setup_multi_volume_encryption()`.
- **Btrfs subvolumes** are orthogonal: `use_subvolumes = true` still works with a Custom layout's `/` partition because subvolume creation is driven by `disk.use_subvolumes` in `formatting.rs`, independently of how the `ComputedLayout` was produced.
- **4 MiB alignment** is preserved via `floor_align()`, consistent with all other layout functions.

---

# Plan: SecureBoot Setup + Key Generation

## Background

Deploytix currently has zero SecureBoot infrastructure. The project already has strong patterns
for configuration modules, key generation (LUKS keyfiles), bootloader setup, and dry-run support.
This plan adds a new `SecureBoot` configuration option with automatic key generation and enrollment.

---

## Overview of SecureBoot Key Hierarchy

UEFI SecureBoot uses a chain of trust with four key databases:

| Key | Role | Typical Owner |
|-----|------|---------------|
| **PK** (Platform Key) | Root of trust, controls who can modify KEK | Machine owner |
| **KEK** (Key Exchange Key) | Authorizes changes to db/dbx | Machine owner / OS vendor |
| **db** (Signature Database) | Contains trusted signing keys/certs | OS vendor / user |
| **dbx** (Forbidden Signatures) | Revocation list | OS vendor / user |

For a self-enrolled custom SecureBoot setup, Deploytix generates **PK**, **KEK**, and **db** keys,
signs the bootloader and kernel with the **db** key, and enrolls all keys into UEFI firmware.

---

## Implementation Plan

### Step 1: Add `SecureBootConfig` to Configuration (`src/config/deployment.rs`)

Add a new `SecureBootConfig` struct and `SecureBootMode` enum to the configuration:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecureBootConfig {
    /// Enable SecureBoot setup
    #[serde(default)]
    pub enabled: bool,
    /// SecureBoot mode
    #[serde(default)]
    pub mode: SecureBootMode,
    /// Custom common name for generated certificates (default: "Deploytix SecureBoot")
    #[serde(default = "default_secureboot_cn")]
    pub common_name: String,
    /// Key size in bits (default: 4096)
    #[serde(default = "default_secureboot_key_size")]
    pub key_size: u32,
    /// Validity period in days (default: 3650 = ~10 years)
    #[serde(default = "default_secureboot_validity_days")]
    pub validity_days: u32,
}
```

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SecureBootMode {
    /// Generate custom PK/KEK/db keys, sign bootloader+kernel, enroll keys via efi-updatevar
    #[default]
    Custom,
    /// Use shim-signed with MOK (Machine Owner Key) — enroll via mokutil
    Shim,
}
```

Add `secure_boot: SecureBootConfig` field to `DeploymentConfig`.

**Files modified:** `src/config/deployment.rs`

---

### Step 2: Create `SecureBootKeys` Constructor (`src/configure/secureboot.rs`)

Create a new module with a `SecureBootKeys` struct that encapsulates key generation:

```rust
/// Represents a complete set of SecureBoot signing keys
pub struct SecureBootKeys {
    /// Directory where keys are stored
    pub key_dir: String,
    /// Platform Key paths (PK.key, PK.crt, PK.esl, PK.auth)
    pub pk: SecureBootKeyPair,
    /// Key Exchange Key paths (KEK.key, KEK.crt, KEK.esl, KEK.auth)
    pub kek: SecureBootKeyPair,
    /// Signature Database key paths (db.key, db.crt, db.esl, db.auth)
    pub db: SecureBootKeyPair,
}

pub struct SecureBootKeyPair {
    pub name: String,        // "PK", "KEK", or "db"
    pub key_path: String,    // RSA private key (.key)
    pub cert_path: String,   // X.509 certificate (.crt)
    pub esl_path: String,    // EFI Signature List (.esl)
    pub auth_path: String,   // Signed update payload (.auth)
}
```

The constructor `SecureBootKeys::generate()` will:

1. Create the key directory: `{install_root}/etc/secureboot/keys/`
2. For each key (PK, KEK, db):
   - Generate RSA private key via `openssl genrsa -out {name}.key {key_size}`
   - Generate self-signed X.509 certificate via `openssl req -new -x509 -key {name}.key -out {name}.crt -days {validity} -subj "/CN={common_name} {name}"`
   - Convert cert to EFI Signature List via `cert-to-efi-sig-list -g {guid} {name}.crt {name}.esl`
   - Sign the ESL into an authenticated variable via `sign-efi-sig-list` (PK signs itself, PK signs KEK, KEK signs db)
3. Set restrictive permissions (0o600 for keys, 0o644 for certs)
4. Return `SecureBootKeys` struct with all paths

**Signing chain:**
- `sign-efi-sig-list -g {guid} -k PK.key -c PK.crt PK PK.esl PK.auth` (PK self-signs)
- `sign-efi-sig-list -g {guid} -k PK.key -c PK.crt KEK KEK.esl KEK.auth` (PK signs KEK)
- `sign-efi-sig-list -g {guid} -k KEK.key -c KEK.crt db db.esl db.auth` (KEK signs db)

**Files created:** `src/configure/secureboot.rs`

---

### Step 3: Add Signing and Enrollment Functions (`src/configure/secureboot.rs`)

Add functions for signing EFI binaries and enrolling keys:

#### `sign_efi_binary()`
Signs a single EFI binary (bootloader or kernel) with the db key:
```
sbsign --key db.key --cert db.crt --output {binary} {binary}
```

Targets to sign:
- `/boot/efi/EFI/BOOT/BOOTX64.EFI` (GRUB EFI binary)
- `/boot/efi/EFI/BOOT/grubx64.efi` (if present)
- `/boot/vmlinuz-linux-zen` (kernel, if direct-boot)

#### `sign_bootloader_and_kernel()`
Orchestrator that finds and signs all relevant EFI binaries.

#### `enroll_keys()`
Enrolls the generated keys into UEFI firmware:
- **Custom mode**: Uses `efi-updatevar` to write PK, KEK, db variables
  ```
  efi-updatevar -e -f db.esl db
  efi-updatevar -e -f KEK.esl KEK
  efi-updatevar -f PK.auth PK
  ```
  (PK must be enrolled last as it locks down the other variables)

- **Shim mode**: Uses `mokutil --import db.crt` to enroll the signing key as a MOK
  (User must confirm enrollment on next boot via MokManager)

#### `setup_secure_boot()` — Main entry point
Orchestrates the full SecureBoot setup:
1. Generate keys (`SecureBootKeys::generate()`)
2. Sign bootloader and kernel (`sign_bootloader_and_kernel()`)
3. Enroll keys (`enroll_keys()`)

**Files modified:** `src/configure/secureboot.rs`

---

### Step 4: Register Module and Wire Into Installer

#### 4a. Register the module
Add `pub mod secureboot;` to `src/configure/mod.rs`.

#### 4b. Add required packages to basestrap
In `src/install/basestrap.rs`, conditionally add SecureBoot packages when enabled:
- `efitools` — provides `cert-to-efi-sig-list`, `sign-efi-sig-list`, `efi-updatevar`
- `sbsigntools` — provides `sbsign`, `sbverify`
- `openssl` — for key/certificate generation

#### 4c. Wire into `Installer::run()` in `src/install/installer.rs`
Add SecureBoot setup after bootloader installation (Phase 4), before finalization:

```rust
// Phase 4.7: SecureBoot setup (after bootloader, before desktop)
if self.config.secure_boot.enabled {
    self.report_progress(0.78, "Setting up SecureBoot...");
    configure::secureboot::setup_secure_boot(
        &self.cmd,
        &self.config,
        INSTALL_ROOT,
    )?;
}
```

**Files modified:**
- `src/configure/mod.rs`
- `src/install/basestrap.rs`
- `src/install/installer.rs`

---

### Step 5: Add Wizard Prompts and Validation

#### 5a. Interactive wizard (`src/config/deployment.rs`)
After the bootloader selection in `from_wizard()`, add:
```
? Enable UEFI SecureBoot? [y/N]
? SecureBoot mode: [Custom keys / Shim+MOK]
```

#### 5b. TOML config support
Example config section:
```toml
[secure_boot]
enabled = true
mode = "custom"
common_name = "Deploytix SecureBoot"
key_size = 4096
validity_days = 3650
```

#### 5c. Validation in `validate()`
- SecureBoot requires UEFI (EFI partition must exist — always true for current layouts)
- Shim mode is incompatible with CryptoSubvolume+boot_encryption (shim needs unencrypted ESP)

**Files modified:** `src/config/deployment.rs`

---

### Step 6: Add GUI Panel Support (optional, if GUI feature enabled)

Add SecureBoot toggle and mode selector to the GUI wizard panels in `src/gui/panels.rs`.

**Files modified:** `src/gui/panels.rs`, `src/gui/app.rs`

---

## File Change Summary

| File | Action | Description |
|------|--------|-------------|
| `src/config/deployment.rs` | Modify | Add `SecureBootConfig`, `SecureBootMode`, wizard prompts, validation |
| `src/configure/secureboot.rs` | **Create** | Key generation constructor, signing, enrollment, orchestrator |
| `src/configure/mod.rs` | Modify | Register `secureboot` module |
| `src/install/installer.rs` | Modify | Wire SecureBoot into installation phases |
| `src/install/basestrap.rs` | Modify | Add conditional SecureBoot package dependencies |
| `src/gui/panels.rs` | Modify | (Optional) Add SecureBoot UI controls |
| `src/gui/app.rs` | Modify | (Optional) Add SecureBoot state |

## External Tool Dependencies

The following must be available in the target system (installed via basestrap):
- `openssl` — RSA key generation and X.509 certificate creation
- `efitools` — EFI signature list conversion and variable enrollment
- `sbsigntools` — EFI binary signing and verification

## Key Design Decisions

1. **Keys generated at install time** — Each installation gets unique keys. No shared/pre-built keys.
2. **Keys stored in `/etc/secureboot/keys/`** — Allows re-signing after kernel updates.
3. **All operations go through `CommandRunner`** — Full dry-run support preserved.
4. **Custom mode is default** — Full control without third-party shim dependency.
5. **GUID generated per-installation** — Uses `uuid` crate (already a dependency) for unique owner GUID.
