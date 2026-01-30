# Deploytix Fix Implementation Task List

This document provides step-by-step instructions for an AI coding agent to implement the required fixes identified in the codebase review.

---

## Prerequisites

Before starting, familiarize yourself with:
- Rust language and Cargo build system
- The project structure in `/home/user/Deploytix/src/`
- The existing error types in `src/utils/error.rs`
- The command execution pattern in `src/utils/command.rs`

---

## Phase 1: Critical Security Fixes (P0)

### Task 1.1: Fix Command Injection in User Creation

**File:** `src/configure/users.rs`

**Problem:** Lines 30-38 interpolate user input directly into shell commands, allowing command injection.

**Instructions:**

1. Read `src/configure/users.rs` to understand the current implementation
2. Read `src/utils/command.rs` to understand the `CommandRunner` API

3. Modify `CommandRunner` in `src/utils/command.rs` to add a new method:
   ```rust
   /// Run command in chroot with stdin input
   pub fn run_in_chroot_with_stdin(
       &self,
       chroot_path: &str,
       command: &str,
       stdin_data: &str,
   ) -> Result<()>
   ```
   - Use `std::process::Command` with `.stdin(Stdio::piped())`
   - Write `stdin_data` to the child's stdin
   - Handle dry-run mode appropriately

4. In `src/configure/users.rs`, update `create_user()`:
   - Replace the shell interpolation for `useradd` with proper argument array:
     ```rust
     cmd.run_in_chroot(install_root, "useradd", &["-m", "-G", &groups_str, "-s", "/bin/bash", username])?;
     ```
   - Replace `chpasswd` to use stdin:
     ```rust
     cmd.run_in_chroot_with_stdin(install_root, "chpasswd", &format!("{}:{}", username, password))?;
     ```

5. Apply the same fix to `set_root_password()` function (lines 86-102)

6. Add input validation for username in `src/config/deployment.rs`:
   ```rust
   fn validate_username(name: &str) -> Result<()> {
       // Must start with lowercase letter
       // Only contain [a-z0-9_-]
       // Length 1-32 characters
   }
   ```

**Verification:** Run `cargo build` and ensure no compilation errors.

---

### Task 1.2: Fix Hardcoded Root Partition Number

**File:** `src/configure/bootloader.rs`

**Problem:** Line 34 hardcodes `partition_path(device, 4)` assuming root is always partition 4.

**Instructions:**

1. Read `src/disk/layouts.rs` to understand the `ComputedLayout` and `PartitionInfo` structures

2. Add a method to `ComputedLayout` in `src/disk/layouts.rs`:
   ```rust
   impl ComputedLayout {
       /// Get the partition number for the root filesystem
       pub fn root_partition_number(&self) -> Option<u32> {
           self.partitions
               .iter()
               .find(|p| p.mount_point.as_deref() == Some("/"))
               .map(|p| p.number)
       }
   }
   ```

3. Update `src/configure/bootloader.rs`:
   - Modify `install_grub()` function signature to accept `&ComputedLayout`
   - Replace line 34:
     ```rust
     // Before:
     let root_part = partition_path(device, 4);

     // After:
     let root_num = layout.root_partition_number()
         .ok_or_else(|| DeploytixError::ConfigError("No root partition found".into()))?;
     let root_part = partition_path(device, root_num);
     ```

4. Update the call site in `src/install/installer.rs` to pass the layout to `install_grub()`

5. Apply the same fix to `install_systemd_boot()` if it references partition numbers

**Verification:** Test with both `standard` and `minimal` layouts in dry-run mode.

---

### Task 1.3: Fix systemd-boot UUID Placeholder

**File:** `src/configure/bootloader.rs`

**Problem:** Lines 163-168 contain literal `<ROOT_UUID>` that is never replaced.

**Instructions:**

1. Add a utility function to get partition UUID in `src/utils/command.rs` or `src/disk/detection.rs`:
   ```rust
   /// Get the UUID of a partition
   pub fn get_partition_uuid(partition_path: &str) -> Result<String> {
       // Read from /dev/disk/by-uuid/ or use blkid command
       // blkid -s UUID -o value /dev/sdX1
   }
   ```

2. In `src/configure/bootloader.rs`, update `install_systemd_boot()`:
   - Get the root partition path (using the fix from Task 1.2)
   - Call `get_partition_uuid()` to get the actual UUID
   - Replace the template:
     ```rust
     let root_uuid = get_partition_uuid(&root_part)?;
     let entry_content = format!(
         "title   Artix Linux\n\
          linux   /vmlinuz-linux-zen\n\
          initrd  /initramfs-linux-zen.img\n\
          options root=UUID={} rw\n",
         root_uuid
     );
     ```

3. Handle dry-run mode by using a placeholder UUID with a log message

**Verification:** Run in dry-run mode and verify the generated entry content is logged with proper UUID format.

---

### Task 1.4: Disable Encryption Until Implemented

**File:** `src/configure/encryption.rs` and `src/config/deployment.rs`

**Problem:** Encryption can be enabled but does nothing, giving false sense of security.

**Instructions:**

1. Add a new error variant in `src/utils/error.rs`:
   ```rust
   #[error("Feature not implemented: {0}")]
   NotImplemented(String),
   ```

2. In `src/config/deployment.rs`, update the `validate()` method to reject encryption:
   ```rust
   if self.disk.encryption {
       return Err(DeploytixError::NotImplemented(
           "LUKS encryption is not yet implemented. Set encryption = false in config.".into()
       ));
   }
   ```

3. Add a comment in `src/configure/encryption.rs` documenting what needs to be implemented:
   ```rust
   //! # TODO: Full LUKS Implementation
   //!
   //! Required steps:
   //! 1. Create LUKS container with cryptsetup luksFormat
   //! 2. Open container with cryptsetup open
   //! 3. Create filesystem inside container
   //! 4. Update mkinitcpio HOOKS to include 'encrypt'
   //! 5. Update bootloader cmdline with cryptdevice parameter
   //! 6. Generate /etc/crypttab
   ```

**Verification:** Create a config with `encryption = true` and verify it returns an error.

---

## Phase 2: Important Bug Fixes (P1)

### Task 2.1: Fix Sudoers Modification Safety

**File:** `src/configure/users.rs`

**Problem:** Direct modification of `/etc/sudoers` can corrupt the file and lock out sudo access.

**Instructions:**

1. Replace the direct file modification (lines 60-79) with drop-in file approach:
   ```rust
   fn configure_sudoers(cmd: &CommandRunner, install_root: &str) -> Result<()> {
       info!("Configuring sudoers for wheel group");

       let sudoers_d = format!("{}/etc/sudoers.d", install_root);
       let wheel_file = format!("{}/wheel", sudoers_d);

       if cmd.is_dry_run() {
           println!("  [dry-run] Would create {}", wheel_file);
           return Ok(());
       }

       // Ensure directory exists
       std::fs::create_dir_all(&sudoers_d)?;

       // Write drop-in file (safer than modifying main sudoers)
       std::fs::write(&wheel_file, "%wheel ALL=(ALL:ALL) NOPASSWD: ALL\n")?;

       // Set correct permissions (0440)
       use std::os::unix::fs::PermissionsExt;
       std::fs::set_permissions(&wheel_file, std::fs::Permissions::from_mode(0o440))?;

       Ok(())
   }
   ```

2. Ensure the drop-in file has correct ownership (root:root) - this happens automatically when running as root

**Verification:** Check that `/etc/sudoers.d/wheel` is created with correct permissions (0440).

---

### Task 2.2: Read Sector Size from Device

**File:** `src/disk/partitioning.rs`

**Problem:** Sector size is hardcoded to 512, but NVMe drives often use 4096.

**Instructions:**

1. Add a function in `src/disk/detection.rs`:
   ```rust
   /// Read the logical sector size of a block device
   pub fn get_sector_size(device: &str) -> Result<u64> {
       // Extract device name (e.g., "sda" from "/dev/sda")
       let dev_name = device.trim_start_matches("/dev/");
       let path = format!("/sys/block/{}/queue/logical_block_size", dev_name);

       let content = std::fs::read_to_string(&path)
           .map_err(|_| DeploytixError::DeviceNotFound(device.to_string()))?;

       content.trim().parse::<u64>()
           .map_err(|_| DeploytixError::DeviceNotFound(format!("Invalid sector size for {}", device)))
   }
   ```

2. Update `src/disk/partitioning.rs` line 15:
   ```rust
   // Before:
   let sector_size = 512u64;

   // After:
   let sector_size = get_sector_size(device).unwrap_or(512);
   ```

3. Also update any other hardcoded 512 values in `src/disk/layouts.rs` if present

**Verification:** Test detection on both SATA and NVMe devices if available.

---

### Task 2.3: Add Proper Error Handling for Disk Operations

**File:** `src/disk/partitioning.rs`

**Problem:** Lines 102, 125-126 ignore errors from `wipefs`, `partprobe`, and `udevadm`.

**Instructions:**

1. Replace silent error ignoring with logged warnings:
   ```rust
   // Before:
   let _ = cmd.run("wipefs", &["-a", device]);

   // After:
   if let Err(e) = cmd.run("wipefs", &["-a", device]) {
       tracing::warn!("wipefs failed (continuing anyway): {}", e);
   }
   ```

2. Apply to all instances:
   - Line 102: `wipefs`
   - Line 125: `partprobe`
   - Line 126: `udevadm settle`

3. For `partprobe`, add a retry mechanism since it can fail transiently:
   ```rust
   for attempt in 1..=3 {
       match cmd.run("partprobe", &[device]) {
           Ok(_) => break,
           Err(e) if attempt < 3 => {
               tracing::warn!("partprobe attempt {} failed, retrying: {}", attempt, e);
               std::thread::sleep(std::time::Duration::from_secs(1));
           }
           Err(e) => {
               tracing::warn!("partprobe failed after 3 attempts: {}", e);
           }
       }
   }
   ```

**Verification:** Ensure warnings appear in logs when operations fail.

---

### Task 2.4: Add Basic Input Validation

**File:** `src/config/deployment.rs`

**Problem:** Timezone, locale, keymap, and hostname are not validated.

**Instructions:**

1. Add validation functions:
   ```rust
   fn validate_timezone(tz: &str) -> Result<()> {
       let tz_path = format!("/usr/share/zoneinfo/{}", tz);
       if !std::path::Path::new(&tz_path).exists() {
           return Err(DeploytixError::ValidationError(
               format!("Invalid timezone: {}. Check /usr/share/zoneinfo/", tz)
           ));
       }
       Ok(())
   }

   fn validate_hostname(hostname: &str) -> Result<()> {
       // RFC 1123: alphanumeric and hyphens, 1-63 chars, no leading/trailing hyphen
       let re = regex::Regex::new(r"^[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$").unwrap();
       if !re.is_match(hostname) {
           return Err(DeploytixError::ValidationError(
               format!("Invalid hostname: {}. Must be RFC 1123 compliant.", hostname)
           ));
       }
       Ok(())
   }

   fn validate_locale(locale: &str) -> Result<()> {
       // Basic format check: xx_XX.UTF-8
       let re = regex::Regex::new(r"^[a-z]{2}_[A-Z]{2}(\.[A-Za-z0-9-]+)?$").unwrap();
       if !re.is_match(locale) {
           return Err(DeploytixError::ValidationError(
               format!("Invalid locale format: {}. Expected format: en_US.UTF-8", locale)
           ));
       }
       Ok(())
   }
   ```

2. Call these in the `validate()` method of `DeploymentConfig`

3. Ensure `regex` is in `Cargo.toml` dependencies (it already is)

**Verification:** Test with invalid timezone like "Invalid/Zone" and verify error is returned.

---

## Phase 3: Code Quality Improvements (P2)

### Task 3.1: Extract Shared Unmount Logic

**Files:** `src/cleanup/mod.rs` and `src/install/chroot.rs`

**Problem:** Unmount logic is duplicated.

**Instructions:**

1. Create a new shared function in `src/utils/mount.rs`:
   ```rust
   //! Mount/unmount utilities

   use crate::utils::command::CommandRunner;
   use crate::utils::error::Result;
   use tracing::info;

   /// Unmount all filesystems under a given path
   pub fn unmount_recursive(cmd: &CommandRunner, base_path: &str) -> Result<()> {
       info!("Unmounting all partitions from {}", base_path);

       // Disable swap first
       if let Err(e) = cmd.run("swapoff", &["-a"]) {
           tracing::warn!("swapoff failed: {}", e);
       }

       // Get and sort mount points
       let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
       let mut mount_points: Vec<&str> = mounts
           .lines()
           .filter_map(|line| {
               let parts: Vec<&str> = line.split_whitespace().collect();
               if parts.len() >= 2 && parts[1].starts_with(base_path) {
                   Some(parts[1])
               } else {
                   None
               }
           })
           .collect();

       // Sort deepest first
       mount_points.sort_by_key(|b| std::cmp::Reverse(b.matches('/').count()));

       // Unmount each
       for mp in mount_points {
           info!("Unmounting {}", mp);
           if let Err(e) = cmd.run("umount", &[mp]) {
               tracing::warn!("Failed to unmount {}: {}", mp, e);
           }
       }

       Ok(())
   }
   ```

2. Update `src/utils/mod.rs` to export the new module:
   ```rust
   pub mod mount;
   ```

3. Update `src/cleanup/mod.rs` to use the shared function:
   ```rust
   use crate::utils::mount::unmount_recursive;
   // Replace the duplicate code with:
   unmount_recursive(cmd, install_root)?;
   ```

4. Update `src/install/chroot.rs` similarly

**Verification:** Both cleanup and chroot unmounting should work identically.

---

### Task 3.2: Add Progress Indicators

**File:** `src/install/installer.rs` and `src/install/basestrap.rs`

**Problem:** Long operations provide no progress feedback.

**Instructions:**

1. The `indicatif` crate is already a dependency. Add progress bars for long operations:

2. In `src/install/installer.rs`, wrap the installation phases:
   ```rust
   use indicatif::{ProgressBar, ProgressStyle};

   let phases = [
       "Preparing", "Partitioning", "Formatting",
       "Mounting", "Installing base", "Configuring",
       "Installing desktop", "Finalizing"
   ];

   let pb = ProgressBar::new(phases.len() as u64);
   pb.set_style(ProgressStyle::default_bar()
       .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
       .unwrap()
       .progress_chars("#>-"));

   for (i, phase) in phases.iter().enumerate() {
       pb.set_message(phase.to_string());
       // ... do phase work ...
       pb.set_position((i + 1) as u64);
   }
   pb.finish_with_message("Installation complete!");
   ```

3. For `basestrap` (package installation), show a spinner:
   ```rust
   let spinner = ProgressBar::new_spinner();
   spinner.set_message("Installing base packages (this may take a while)...");
   spinner.enable_steady_tick(std::time::Duration::from_millis(100));
   // ... run basestrap ...
   spinner.finish_with_message("Base packages installed");
   ```

**Verification:** Run in dry-run mode and observe progress indicators.

---

### Task 3.3: Add File Logging

**File:** `src/main.rs`

**Problem:** No persistent logs for debugging failed installations.

**Instructions:**

1. Update the logging setup in `src/main.rs`:
   ```rust
   use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
   use std::fs::File;

   fn setup_logging(verbose: bool) -> Result<()> {
       let filter = if verbose {
           EnvFilter::new("debug")
       } else {
           EnvFilter::new("info")
       };

       // Create log file
       let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
       let log_path = format!("/tmp/deploytix_{}.log", timestamp);
       let log_file = File::create(&log_path)?;

       // Console layer
       let console_layer = fmt::layer()
           .with_target(false)
           .with_ansi(true);

       // File layer
       let file_layer = fmt::layer()
           .with_target(true)
           .with_ansi(false)
           .with_writer(log_file);

       tracing_subscriber::registry()
           .with(filter)
           .with(console_layer)
           .with(file_layer)
           .init();

       info!("Logging to: {}", log_path);
       Ok(())
   }
   ```

2. Add `chrono` to `Cargo.toml` if not present:
   ```toml
   chrono = "0.4"
   ```

3. Print the log file path at the end of installation (success or failure)

**Verification:** After running, check that `/tmp/deploytix_*.log` exists with proper content.

---

## Phase 4: Feature Completion (P3)

### Task 4.1: Implement Custom Partition Layout

**File:** `src/disk/layouts.rs`

**Problem:** `PartitionLayout::Custom` is defined but not implemented.

**Instructions:**

1. Add custom partition configuration to `src/config/deployment.rs`:
   ```rust
   #[derive(Debug, Clone, Serialize, Deserialize)]
   pub struct CustomPartition {
       pub size_mib: Option<u64>,  // None = use remaining space
       pub filesystem: Filesystem,
       pub mount_point: Option<String>,
       pub label: String,
   }

   #[derive(Debug, Clone, Serialize, Deserialize)]
   pub struct DiskConfig {
       // ... existing fields ...
       #[serde(default)]
       pub custom_partitions: Vec<CustomPartition>,
   }
   ```

2. In `src/disk/layouts.rs`, implement the custom layout calculation:
   ```rust
   PartitionLayout::Custom => {
       if config.disk.custom_partitions.is_empty() {
           return Err(DeploytixError::ConfigError(
               "Custom layout requires custom_partitions to be defined".into()
           ));
       }

       let mut partitions = Vec::new();
       let mut current_start = FIRST_USABLE_SECTOR;

       // Always add EFI partition first
       partitions.push(PartitionInfo { /* EFI config */ });

       for (i, custom) in config.disk.custom_partitions.iter().enumerate() {
           // Calculate size and create PartitionInfo
       }

       ComputedLayout { partitions, device_size_mib }
   }
   ```

3. Update documentation/example config to show custom layout usage

**Verification:** Test with a custom layout config in dry-run mode.

---

### Task 4.2: Add BIOS Boot Support

**File:** `src/configure/bootloader.rs`

**Problem:** Only UEFI boot is supported.

**Instructions:**

1. Add boot mode detection in `src/utils/` or `src/disk/detection.rs`:
   ```rust
   pub fn is_uefi_boot() -> bool {
       std::path::Path::new("/sys/firmware/efi").exists()
   }
   ```

2. Update GRUB installation to handle both modes:
   ```rust
   pub fn install_grub(/* params */) -> Result<()> {
       if is_uefi_boot() {
           // Existing EFI installation
           cmd.run_in_chroot(install_root, "grub-install", &[
               "--target=x86_64-efi",
               "--boot-directory=/boot",
               "--efi-directory=/boot/efi",
               "--removable",
               device,
           ])?;
       } else {
           // BIOS installation
           cmd.run_in_chroot(install_root, "grub-install", &[
               "--target=i386-pc",
               "--boot-directory=/boot",
               device,
           ])?;
       }
       // ... continue with grub-mkconfig ...
   }
   ```

3. Update partition layouts to include BIOS boot partition when in BIOS mode:
   - BIOS boot partition: 1 MiB, no filesystem, type `21686148-6449-6E6F-744E-656564454649`

**Verification:** Test detection logic on both UEFI and legacy systems if available.

---

## Testing Instructions

After implementing fixes, run these verification steps:

```bash
# 1. Build the project
cargo build --release

# 2. Run clippy for lint checks
cargo clippy -- -D warnings

# 3. Check formatting
cargo fmt -- --check

# 4. Test config validation
./target/release/deploytix validate --config test-config.toml

# 5. Test dry-run with standard layout
./target/release/deploytix install --config test-config.toml --dry-run

# 6. Test dry-run with minimal layout
# (modify test-config.toml to use layout = "minimal")
./target/release/deploytix install --config test-config.toml --dry-run

# 7. Verify error handling
# (create config with encryption = true and verify error)
```

---

## Completion Checklist

- [ ] Task 1.1: Command injection fix
- [ ] Task 1.2: Hardcoded root partition fix
- [ ] Task 1.3: systemd-boot UUID fix
- [ ] Task 1.4: Encryption disabled
- [ ] Task 2.1: Sudoers safety fix
- [ ] Task 2.2: Sector size detection
- [ ] Task 2.3: Error handling improvements
- [ ] Task 2.4: Input validation
- [ ] Task 3.1: Shared unmount logic
- [ ] Task 3.2: Progress indicators
- [ ] Task 3.3: File logging
- [ ] Task 4.1: Custom partition layout
- [ ] Task 4.2: BIOS boot support
- [ ] All tests pass
- [ ] No clippy warnings
- [ ] Code formatted with rustfmt

---

*Task list generated: 2026-01-30*
