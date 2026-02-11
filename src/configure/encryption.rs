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
    /// Source device (e.g., /dev/sda3)
    pub device: String,
    /// Mapper name (e.g., Crypt-Root)
    pub mapper_name: String,
    /// Mapped device path (e.g., /dev/mapper/Crypt-Root)
    pub mapped_path: String,
}

/// Setup LUKS encryption for the specified partition
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
    let mapper_name = config.disk.luks_mapper_name.clone();
    let mapped_path = format!("/dev/mapper/{}", mapper_name);

    info!("Setting up LUKS2 encryption on {} (mapper: {})", luks_device, mapper_name);

    if cmd.is_dry_run() {
        println!("  [dry-run] cryptsetup luksFormat --type luks2 {}", luks_device);
        println!("  [dry-run] cryptsetup open {} {}", luks_device, mapper_name);
        return Ok(LuksContainer {
            device: luks_device,
            mapper_name,
            mapped_path,
        });
    }

    // Format LUKS container
    luks_format(&luks_device, password)?;

    // Open LUKS container
    luks_open(&luks_device, &mapper_name, password)?;

    info!("LUKS encryption setup complete: {} -> {}", luks_device, mapped_path);

    Ok(LuksContainer {
        device: luks_device,
        mapper_name,
        mapped_path,
    })
}

/// Format a device as LUKS2
fn luks_format(device: &str, password: &str) -> Result<()> {
    info!("Formatting {} as LUKS2 container (aes-xts-plain64, argon2id)", device);

    // Use stdin to pass password securely (fixes command injection vulnerability)
    let mut child = Command::new("cryptsetup")
        .args([
            "luksFormat",
            "--type", "luks2",
            "--cipher", "aes-xts-plain64",
            "--key-size", "512",
            "--hash", "sha512",
            "--pbkdf", "argon2id",
            "--batch-mode",
            device,
        ])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| DeploytixError::CommandFailed {
            command: "cryptsetup luksFormat".to_string(),
            stderr: e.to_string(),
        })?;

    // Write password to stdin
    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(password.as_bytes())?;
    }
    drop(child.stdin.take()); // Close stdin to signal EOF

    let status = child.wait()?;
    if !status.success() {
        return Err(DeploytixError::CommandFailed {
            command: "cryptsetup luksFormat".to_string(),
            stderr: "Failed to format LUKS container".to_string(),
        });
    }

    Ok(())
}

/// Open a LUKS container
fn luks_open(device: &str, mapper_name: &str, password: &str) -> Result<()> {
    info!("Opening LUKS container {} as {}", device, mapper_name);

    let mut child = Command::new("cryptsetup")
        .args(["open", device, mapper_name])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| DeploytixError::CommandFailed {
            command: "cryptsetup open".to_string(),
            stderr: e.to_string(),
        })?;

    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(password.as_bytes())?;
    }
    drop(child.stdin.take()); // Close stdin to signal EOF

    let status = child.wait()?;
    if !status.success() {
        return Err(DeploytixError::CommandFailed {
            command: "cryptsetup open".to_string(),
            stderr: "Failed to open LUKS container".to_string(),
        });
    }

    // Wait for device to appear
    std::thread::sleep(std::time::Duration::from_millis(500));

    Ok(())
}

/// Setup LUKS1 encryption for the /boot partition
///
/// LUKS1 is required because GRUB's cryptodisk module does not support LUKS2.
/// Uses pbkdf2 as the KDF since GRUB cannot handle argon2id.
pub fn setup_boot_encryption(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    boot_partition: u32,
) -> Result<LuksContainer> {
    if !config.disk.boot_encryption {
        return Err(DeploytixError::ConfigError(
            "Boot encryption not enabled in configuration".to_string(),
        ));
    }

    let password = config
        .disk
        .encryption_password
        .as_ref()
        .ok_or_else(|| DeploytixError::ValidationError(
            "Encryption password required".to_string()
        ))?;

    let boot_device = partition_path(device, boot_partition);
    let mapper_name = config.disk.luks_boot_mapper_name.clone();
    let mapped_path = format!("/dev/mapper/{}", mapper_name);

    info!("Setting up LUKS1 encryption on {} for /boot (mapper: {})", boot_device, mapper_name);

    if cmd.is_dry_run() {
        println!("  [dry-run] cryptsetup luksFormat --type luks1 {}", boot_device);
        println!("  [dry-run] cryptsetup open {} {}", boot_device, mapper_name);
        return Ok(LuksContainer {
            device: boot_device,
            mapper_name,
            mapped_path,
        });
    }

    // Format as LUKS1
    luks_format_v1(&boot_device, password)?;

    // Open LUKS container
    luks_open(&boot_device, &mapper_name, password)?;

    info!("LUKS1 boot encryption setup complete: {} -> {}", boot_device, mapped_path);

    Ok(LuksContainer {
        device: boot_device,
        mapper_name,
        mapped_path,
    })
}

/// Format a device as LUKS1 (required for GRUB-accessible encrypted /boot)
///
/// Uses pbkdf2 instead of argon2id because GRUB's cryptodisk module only
/// supports pbkdf2 for LUKS1 containers.
fn luks_format_v1(device: &str, password: &str) -> Result<()> {
    info!("Formatting {} as LUKS1 container (aes-xts-plain64, pbkdf2)", device);

    let mut child = Command::new("cryptsetup")
        .args([
            "luksFormat",
            "--type", "luks1",
            "--cipher", "aes-xts-plain64",
            "--key-size", "512",
            "--hash", "sha512",
            "--batch-mode",
            device,
        ])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| DeploytixError::CommandFailed {
            command: "cryptsetup luksFormat (LUKS1)".to_string(),
            stderr: e.to_string(),
        })?;

    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(password.as_bytes())?;
    }
    drop(child.stdin.take());

    let status = child.wait()?;
    if !status.success() {
        return Err(DeploytixError::CommandFailed {
            command: "cryptsetup luksFormat (LUKS1)".to_string(),
            stderr: "Failed to format LUKS1 container for /boot".to_string(),
        });
    }

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
        .map_err(|e| DeploytixError::CommandFailed {
            command: "cryptsetup luksUUID".to_string(),
            stderr: e.to_string(),
        })?;

    if !output.status.success() {
        return Err(DeploytixError::CommandFailed {
            command: "cryptsetup luksUUID".to_string(),
            stderr: format!("Failed to get LUKS UUID for {}", device),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
