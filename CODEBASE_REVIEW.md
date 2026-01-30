# Deploytix Codebase Review

## Executive Summary

Deploytix is a well-structured Rust CLI for automated Artix Linux deployment. The modular architecture and configuration-driven design are solid foundations. However, there are several bugs, security concerns, and opportunities for improvement that should be addressed before production use.

---

## 1. Critical Bugs & Security Issues

### 1.1 Command Injection Vulnerabilities (HIGH PRIORITY)

**Location:** `src/configure/users.rs:30-38`

```rust
let useradd_cmd = format!(
    "useradd -m -G {} -s /bin/bash {}",
    groups_str, username
);
cmd.run_in_chroot(install_root, &useradd_cmd)?;

let chpasswd_cmd = format!("echo '{}:{}' | chpasswd", username, password);
cmd.run_in_chroot(install_root, &chpasswd_cmd)?;
```

**Problem:** User-provided username and password are interpolated directly into shell commands. A malicious or accidental input like `user'; rm -rf /; '` could execute arbitrary commands.

**Fix:** Use proper argument escaping or pass credentials via stdin/file descriptor rather than shell interpolation:
```rust
// Use stdin to pass password securely
cmd.run_in_chroot_with_stdin(install_root, "chpasswd", &format!("{}:{}", username, password))?;
```

### 1.2 Hardcoded Root Partition Number (HIGH)

**Location:** `src/configure/bootloader.rs:34`

```rust
let root_part = partition_path(device, 4); // Root is partition 4 in standard layout
```

**Problem:** This assumes the standard layout. With the minimal layout, root is partition 3. This will cause GRUB to point to the wrong partition, resulting in an unbootable system.

**Fix:** Store root partition info in `ComputedLayout` and reference it dynamically:
```rust
let root_part = layout.get_root_partition_path(device);
```

### 1.3 systemd-boot Entry Has Literal Placeholder (HIGH)

**Location:** `src/configure/bootloader.rs:163-168`

```rust
let entry_content = r#"title   Artix Linux
linux   /vmlinuz-linux-zen
initrd  /initramfs-linux-zen.img
options root=UUID=<ROOT_UUID> rw
"#;
```

**Problem:** The `<ROOT_UUID>` placeholder is never replaced with the actual UUID. Systems installed with systemd-boot will not boot.

**Fix:** Compute and substitute the actual UUID:
```rust
let root_uuid = get_partition_uuid(&root_part)?;
let entry_content = format!(
    "title   Artix Linux\nlinux   /vmlinuz-linux-zen\ninitrd  /initramfs-linux-zen.img\noptions root=UUID={} rw\n",
    root_uuid
);
```

### 1.4 Encryption Feature is a No-Op (HIGH)

**Location:** `src/configure/encryption.rs`

**Problem:** Users can enable encryption in config, validation passes, but `setup_encryption()` does nothing (just has TODO comments). The system will install without encryption despite the user's expectation, creating a false sense of security.

**Fix:** Either:
1. Implement full LUKS encryption support, OR
2. Return an error if encryption is enabled: `return Err(DeploytixError::NotImplemented("LUKS encryption"))`

### 1.5 Unsafe Sudoers Modification (MEDIUM)

**Location:** `src/configure/users.rs:61-79`

**Problem:** Direct file manipulation of `/etc/sudoers` without `visudo` validation. A malformed sudoers file will lock the user out of sudo entirely.

**Fix:** Write to `/etc/sudoers.d/wheel` instead (drop-in directory) or use `visudo -c` to validate before committing changes.

### 1.6 Sector Size Hardcoded (MEDIUM)

**Location:** `src/disk/partitioning.rs:15`

```rust
let sector_size = 512u64; // Default, could be read from sysfs
```

**Problem:** Modern NVMe drives often use 4096-byte sectors. Incorrect sector size causes partition misalignment and potential boot failures.

**Fix:** Read actual sector size from sysfs:
```rust
let sector_size = read_sector_size(device).unwrap_or(512);
```

---

## 2. Medium Priority Issues

### 2.1 Error Handling Gaps

| Location | Issue |
|----------|-------|
| `partitioning.rs:102` | `let _ = cmd.run("wipefs"...)` silently ignores failure |
| `partitioning.rs:125-126` | `partprobe` and `udevadm` failures ignored |
| `chroot.rs:90` | Unmount failures silently ignored |
| `cleanup/mod.rs:77` | Unmount failures silently ignored |

**Recommendation:** At minimum, log warnings when these operations fail. Consider implementing retry logic for transient failures.

### 2.2 GRUB Assumes UEFI Only

**Location:** `src/configure/bootloader.rs:51-53`

```rust
"grub-install --target=x86_64-efi --boot-directory=/boot --efi-directory=/boot/efi --removable {}"
```

**Problem:** No BIOS/legacy boot support. Systems without UEFI cannot be installed.

**Recommendation:** Detect boot mode and support both:
```rust
if is_uefi_boot() {
    // EFI installation
} else {
    // BIOS installation with --target=i386-pc
}
```

### 2.3 Custom Partition Layout Not Implemented

**Location:** `src/config/deployment.rs:108`

`PartitionLayout::Custom` is defined but not handled in `src/disk/layouts.rs`. Selecting it will likely cause a panic or error.

### 2.4 Hibernation Support Incomplete

The `hibernation` config option exists but:
- Resume parameter not added to GRUB cmdline (commented out in `bootloader.rs:89-92`)
- Swap UUID not passed to mkinitcpio hooks

---

## 3. Code Quality Issues

### 3.1 Duplicate Code

Unmount logic is duplicated in:
- `src/cleanup/mod.rs:51-81`
- `src/install/chroot.rs:64-93`

**Recommendation:** Extract to a shared utility function.

### 3.2 Dead Code

Multiple `#[allow(dead_code)]` annotations indicate unused functions. Consider either using them or removing them to reduce maintenance burden.

### 3.3 Missing Input Validation

| Field | Issue |
|-------|-------|
| `timezone` | Not validated against available timezones |
| `locale` | Not validated against available locales |
| `keymap` | Not validated against available keymaps |
| `hostname` | RFC 1123 hostname validation missing |
| `username` | Only checks for empty/spaces, not full POSIX compliance |

### 3.4 Inconsistent Error Types

Some functions return `Result<()>` with `DeploytixError`, others use `anyhow::Result`. Standardize on one approach.

---

## 4. Missing Features & Workflow Improvements

### 4.1 No Tests

The codebase has zero unit or integration tests. This is problematic for a tool that performs destructive disk operations.

**Recommended test coverage:**
```
tests/
├── unit/
│   ├── config_parsing.rs      # Config validation
│   ├── layout_calculation.rs  # Partition sizing logic
│   └── sfdisk_generation.rs   # Partition script generation
├── integration/
│   ├── dry_run.rs             # Full workflow in dry-run mode
│   └── validation.rs          # Config validation flows
```

### 4.2 No CI/CD Pipeline

**Recommended `.github/workflows/ci.yml`:**
```yaml
name: CI
on: [push, pull_request]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo build --release
      - run: cargo test
      - run: cargo clippy -- -D warnings
      - run: cargo fmt -- --check
```

### 4.3 No Progress Indicators

The `indicatif` crate is included as a dependency but never used. Long operations like `basestrap` provide no feedback.

**Recommendation:** Add progress bars for:
- Partition creation
- Filesystem formatting
- Package installation
- System configuration

### 4.4 No Installation Logging

Failed installations are difficult to debug without logs.

**Recommendation:** Add file logging:
```rust
// Log to /tmp/deploytix-{timestamp}.log
tracing_subscriber::fmt()
    .with_writer(File::create(log_path)?)
    .init();
```

### 4.5 No Recovery/Rollback

If installation fails midway, the system is left in an inconsistent state.

**Recommendation:** Implement checkpoint/rollback:
```rust
impl Installer {
    fn checkpoint(&mut self, phase: Phase) { ... }
    fn rollback(&self) -> Result<()> { ... }
}
```

---

## 5. Expansion Opportunities

### 5.1 Additional Desktop Environments

Currently supported: KDE, GNOME, XFCE, None

Consider adding:
- **Cinnamon** - Popular Linux Mint DE
- **MATE** - GNOME 2 fork
- **LXQt** - Lightweight Qt-based
- **Sway/i3** - Tiling window managers
- **Budgie** - Modern, simple DE

### 5.2 BTRFS Subvolume Support

Modern BTRFS installations use subvolumes for better snapshot management:
```
@ -> /
@home -> /home
@snapshots -> /.snapshots
@var_log -> /var/log
```

This enables:
- Snapper/Timeshift integration
- Bootable snapshots
- Efficient rollbacks

### 5.3 Installation Profiles

Pre-configured profiles for common use cases:
```rust
pub enum InstallProfile {
    Minimal,      // Base system only
    Desktop,      // Standard desktop
    Gaming,       // Gaming-optimized (Steam, Proton, drivers)
    Development,  // Dev tools, containers, editors
    Server,       // Headless, SSH, minimal
    Workstation,  // Professional tools
}
```

### 5.4 Post-Install Hooks

Allow users to run custom scripts:
```toml
[hooks]
post_basestrap = "/path/to/script.sh"
post_configure = "/path/to/another.sh"
```

### 5.5 Package Customization

Allow adding/removing packages from the base installation:
```toml
[packages]
additional = ["neovim", "htop", "git"]
exclude = ["nano"]
```

### 5.6 Mirror Selection

Auto-detect fastest mirrors or allow manual selection:
```toml
[mirrors]
country = "US"
# or
custom = ["https://mirror.example.com/artix/$repo/os/$arch"]
```

### 5.7 Network Configuration During Install

Support configuring WiFi during installation for network-dependent operations:
```toml
[network.wifi]
ssid = "MyNetwork"
password = "secret"
```

### 5.8 Multiboot Support

Detect and preserve existing operating systems:
- Windows dual-boot support
- Other Linux distributions
- os-prober integration

### 5.9 Advanced Filesystem Options

- **ZFS support** (via ZFSBootMenu)
- **LVM support** for flexible volume management
- **RAID support** (mdadm)
- **dm-crypt/LUKS2** full implementation

### 5.10 Remote/Automated Deployment

Support for unattended installations:
- Preseed/kickstart-style configuration
- Network boot (PXE) integration
- Cloud-init compatibility

---

## 6. Priority Matrix

| Priority | Item | Effort |
|----------|------|--------|
| P0 | Fix command injection in users.rs | Low |
| P0 | Fix hardcoded root partition | Low |
| P0 | Fix systemd-boot UUID placeholder | Low |
| P0 | Disable or implement encryption | Medium |
| P1 | Add basic unit tests | Medium |
| P1 | Fix sudoers modification | Low |
| P1 | Read sector size from device | Low |
| P1 | Add CI/CD pipeline | Low |
| P2 | Add progress indicators | Medium |
| P2 | Add file logging | Low |
| P2 | Validate timezone/locale/keymap | Medium |
| P2 | BIOS boot support | Medium |
| P3 | BTRFS subvolumes | High |
| P3 | Additional desktop environments | Medium |
| P3 | Installation profiles | Medium |
| P3 | Post-install hooks | Low |

---

## 7. Conclusion

Deploytix has a solid foundation with clean architecture and good separation of concerns. The main concerns are:

1. **Security vulnerabilities** in user input handling that need immediate attention
2. **Silent failures** that could result in unbootable systems
3. **Missing tests** making the codebase risky to modify
4. **Incomplete features** (encryption, custom layouts) that are exposed to users

Addressing the P0 items is essential before any production use. The codebase is well-positioned for expansion once the core issues are resolved.

---

*Review generated: 2026-01-30*
