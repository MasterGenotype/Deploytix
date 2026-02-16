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
- Warn if systemd-boot is selected (limited SecureBoot support on Artix)

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
