# Test Coverage Analysis & Improvement Proposal

## Current State

The codebase has **68 unit tests** spread across **6 files** out of 47 total Rust source files
(~13% file coverage). The tests that exist are high-quality and focused on critical boot-time
logic (initramfs hooks, mkinitcpio configuration, LUKS volume management). But large portions of
the installer are completely untested.

### Files with tests today

| File | Tests | What is covered |
|------|------:|-----------------|
| `configure/hooks.rs` | 12 | Custom initramfs hook generation |
| `configure/mkinitcpio.rs` | 14 | mkinitcpio MODULES/HOOKS/FILES arrays |
| `disk/volumes.rs` | 6 | VolumeSet construction, encryption, LVM thin |
| `configure/keyfiles.rs` | 1 | Keyfile path derivation |
| `configure/encryption.rs` | 5 | `to_title_case` helper *(added in this PR)* |
| `configure/swap.rs` | 3 | `swap_file_fstab_entry` *(added in this PR)* |
| `install/crypttab.rs` | 2 | `crypttab_options` *(added in this PR)* |
| `config/deployment.rs` | 8 | `CustomPartitionEntry`, `InitSystem` *(added in this PR)* |
| `disk/layouts.rs` | 9 | Math helpers, layout queries *(added in this PR)* |
| `disk/detection.rs` | 8 | `partition_prefix`, `partition_path` *(added in this PR)* |

---

## Areas to Improve

The sections below are ordered by risk/impact. Each includes the rationale, the specific
functions that should be tested, and the main challenge to overcome.

---

### 1. `config/deployment.rs` — `DeploymentConfig::validate()`

**Risk: HIGH** — The validation function contains ~15 distinct business rules that prevent
nonsensical configurations (e.g. boot encryption without base encryption, integrity without
encryption, duplicate custom mount points). None of these rules have automated tests.

**Problem:** `validate()` checks block device existence as its very first step, which makes
every subsequent rule unreachable in a unit-test environment that has no real disk.

**Recommended fix:** Extract the pure logical checks into a private `validate_rules()` helper
that takes only `&self` and returns `Result<()>`. Call it from `validate()` after the device
check. This allows every business rule to be unit-tested without hardware:

```rust
// Example: rules that should each have their own test
fn validate_rules(&self) -> Result<()> {
    if self.user.name.is_empty() { /* ... */ }
    if self.disk.integrity && !self.disk.encryption { /* ... */ }
    if self.disk.boot_encryption && !self.disk.encryption { /* ... */ }
    if self.disk.lvm_thin_pool_percent == 0 || self.disk.lvm_thin_pool_percent > 100 { /* ... */ }
    // SecureBoot ManualKeys requires keys_path
    // Custom layout: root required, no reserved mounts, no duplicates, ≤1 remainder
    // Subvolumes require btrfs
    // SwapType::FileZram requires btrfs or ext4
    Ok(())
}
```

---

### 2. `install/fstab.rs` — fstab content generation

**Risk: HIGH** — Incorrect fstab entries cause an unbootable system. The module has five public
entry-point functions covering regular, subvolume, multi-volume (LUKS), LVM thin, and swap-file
layouts. None are tested.

**Problem:** All functions call `get_partition_uuid()` which reads from the real filesystem, and
they write files to disk.

**Recommended fix:** Use the existing `CommandRunner::new(true)` (dry-run mode). In dry-run the
functions print what they would do but skip I/O. Alternatively, split the string-building logic
from the I/O so the core formatting can be tested with a mock UUID provider. Key scenarios to
cover:

- Correct fstab options per partition type (EFI=vfat, root=btrfs pass=1, others pass=2)
- Swap partition vs. swap file entries
- Btrfs subvolume `subvol=` option presence
- Multi-volume LUKS: entries use mapper paths, not raw device paths
- LVM thin: entries use `/dev/vg/lv` paths

---

### 3. `disk/layouts.rs` — `compute_layout` / `compute_layout_from_config`

**Risk: HIGH** — These functions produce the partition table plan that drives the entire
installation. Calculation errors lead to mis-sized or missing partitions.

**Problem:** `compute_standard_layout`, `compute_minimal_layout`, and `compute_lvm_thin_layout`
internally call `get_ram_mib()` which reads `/proc/meminfo`. The public wrappers
`compute_layout` / `compute_layout_from_config` also accept a `DeploymentConfig` that requires
device validation.

**Recommended fix:** Export `compute_lvm_thin_layout_with_swap` (already public) and add a
thin wrapper `compute_layout_for_disk(disk_mib: u64, layout: PartitionLayout)` that can be
called with synthetic disk sizes. Specific scenarios to verify:

- Standard layout: correct partition count, EFI=512 MiB, BOOT=2048 MiB
- Minimum disk size enforcement (error on disk smaller than required)
- `compute_standard_layout` on 256 GiB disk produces swap capped at 20 GiB
- LVM layout without swap: no SWAP partition, LVM PV is remainder
- Custom layout: remainder partition gets all space; two remainder partitions → error

---

### 4. `install/crypttab.rs` — `generate_crypttab_multi_volume`

**Risk: HIGH** — crypttab drives automatic volume unlocking at boot. A wrong volume name, UUID
reference, or option string leaves the system locked. The `generate_crypttab_multi_volume`
function already has testable string-building logic: it iterates containers and keyfiles to
assemble lines in a fixed format.

**Problem:** It calls `get_luks_uuid()` per container, which reads real LUKS metadata.

**Recommended fix:** Add a thin abstraction (`UuidProvider` trait or a closure parameter) so
tests can inject known UUID strings. Key assertions:

- Each container generates exactly one `name UUID=<uuid> keyfile options` line
- Integrity flag changes options from `luks,discard` to `luks`
- Boot container (LUKS1) always uses `luks,discard` regardless of integrity flag
- Missing keyfile falls back to `keyfile_path(volume_name)` derivation

---

### 5. `configure/bootloader.rs` — GRUB config generation

**Risk: MEDIUM-HIGH** — `configure_grub_defaults` and `configure_grub_defaults_lvm_thin` write
`/etc/default/grub`. A wrong kernel command line (e.g. missing `cryptdevice=` or `rd.luks.name=`)
prevents unlocking the encrypted root at boot.

**Problem:** Functions write directly to files under `install_root`.

**Recommended fix:** Use `tempfile::tempdir()` as `install_root`. The generated file content can
then be read back and asserted. Scenarios:

- Encrypted standard: `cryptdevice=` present in `GRUB_CMDLINE_LINUX`
- Encrypted LVM thin: `rd.luks.name=` or `cryptdevice=` for the LVM PV
- SecureBoot enabled: correct signing command in post-generation hook
- BIOS/legacy boot: `GRUB_TERMINAL=console` set, no EFI-specific options

---

### 6. `configure/users.rs` — User creation

**Risk: MEDIUM** — User account setup involves `useradd`, `passwd`, `usermod`, and group
membership. Errors lock users out of the installed system.

**Problem:** All functions shell out to external commands.

**Recommended fix:** Test via `CommandRunner::new(true)` (dry-run). The dry-run runner logs
commands without executing them; capturing output with `assert!(...)` on the logged strings
confirms the right commands are built. Alternatively, add a mock `CommandRunner` variant.
Scenarios to cover:

- `useradd` invoked with the correct username and home directory
- Password is set via `chpasswd` (not `passwd`) for non-interactive use
- Configured groups are all passed to `usermod -aG`
- Root password set when `config.user.root_password` is provided

---

### 7. `disk/formatting.rs` — Filesystem creation

**Risk: MEDIUM** — Wrong filesystem type or missing formatting step leads to an unbootable
partition. The module currently has no tests and wraps `mkfs.*` commands.

**Problem:** Requires real block devices.

**Recommended fix:** Integration tests in `tests/` using `losetup` with a sparse image file, or
dry-run mode tests that assert the correct `mkfs.*` variant is invoked for each filesystem enum
value (Btrfs, Ext4, Xfs, F2fs). Key assertions:

- `Filesystem::Btrfs` → `mkfs.btrfs`
- `Filesystem::Ext4` → `mkfs.ext4`
- EFI partition always uses `mkfs.fat -F32`
- Swap partition always uses `mkswap`

---

### 8. `configure/secureboot.rs` — Key enrollment & signing

**Risk: MEDIUM** — SecureBoot mis-configuration (wrong key paths, missing signing step) causes
firmware to refuse to boot the signed bootloader.

**Problem:** Most operations shell out to `sbctl`, `openssl`, and `sbsign`.

**Recommended fix:** Extract pure helpers that can be tested in isolation:

- `get_signing_key_paths(config, install_root)` — already pure, should have a test asserting
  that the paths differ between `Sbctl` and `ManualKeys` methods
- `print_enrollment_instructions(config)` — pure output, can be tested by capturing stdout
- `sign_efi_binary` command construction — testable via dry-run

---

### 9. `configure/swap.rs` — ZRAM service file generation

**Risk: LOW-MEDIUM** — The four `setup_zram_*` helpers (`runit`, `openrc`, `s6`, `dinit`) write
init-specific service files. Wrong paths or missing `ExecStart` lines cause ZRAM swap never to
activate.

**Recommended fix:** Use `tempfile::tempdir()` as `install_root` and assert:

- Service file written to the correct directory for each init system
- File contains the configured `zram_percent` and `algorithm`
- `mkswap` and `swapon` are referenced in all variants

---

### 10. `utils/deps.rs` — Dependency checking

**Risk: LOW** — `check_dependencies` and `ensure_dependencies` determine which tools must be
present before installation proceeds. A wrong package name mapping causes a confusing error
message.

**Recommended fix:** The `binary_to_package()` mapping table is a pure `HashMap`; test that:

- Every key maps to a non-empty package name
- Known critical binaries (`cryptsetup`, `grub-install`, `mkfs.btrfs`) are in the map
- `required_binaries(config)` returns the expected set for a given layout/feature combination

---

## Summary Table

| Priority | Module | Tests to add | Blocker to unblock |
|----------|--------|--------------|--------------------|
| High | `config/deployment.rs` | ~12 (validate rules) | Extract `validate_rules()` |
| High | `install/fstab.rs` | ~10 (content format) | Injectable UUID provider |
| High | `disk/layouts.rs` | ~8 (layout computation) | Expose `compute_*` with disk_mib arg |
| High | `install/crypttab.rs` | ~5 (multi-volume format) | Injectable UUID provider |
| Med-High | `configure/bootloader.rs` | ~6 (GRUB config) | `tempdir` as install_root |
| Medium | `configure/users.rs` | ~4 (command construction) | dry-run mode assertions |
| Medium | `disk/formatting.rs` | ~5 (mkfs dispatch) | dry-run mode assertions |
| Medium | `configure/secureboot.rs` | ~4 (pure helpers) | Expose `get_signing_key_paths` |
| Low-Med | `configure/swap.rs` | ~4 (service files) | `tempdir` as install_root |
| Low | `utils/deps.rs` | ~3 (mapping table) | None (pure functions) |
