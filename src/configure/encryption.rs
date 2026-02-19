//! LUKS encryption setup

use crate::config::DeploymentConfig;
use crate::disk::detection::partition_path;
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::io::Write;
use std::process::{Command, Stdio};
use tracing::info;

/// Convert string to title case (e.g., "ROOT" -> "Root", "USR" -> "Usr")
fn to_title_case(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut chars = lower.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

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

/// Setup LUKS encryption for the specified partition (legacy single-volume)
#[allow(dead_code)]
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

    let password = config.disk.encryption_password.as_ref().ok_or_else(|| {
        DeploytixError::ValidationError("Encryption password required".to_string())
    })?;

    let luks_device = partition_path(device, luks_partition);
    let mapper_name = config.disk.luks_mapper_name.clone();
    let mapped_path = format!("/dev/mapper/{}", mapper_name);

    info!(
        "Setting up LUKS2 encryption on {} (mapper: {})",
        luks_device, mapper_name
    );

    let integrity = config.disk.integrity;

    if cmd.is_dry_run() {
        let integrity_flag = if integrity {
            " --integrity hmac-sha256"
        } else {
            ""
        };
        println!(
            "  [dry-run] cryptsetup luksFormat --type luks2{} {}",
            integrity_flag, luks_device
        );
        println!(
            "  [dry-run] cryptsetup open {} {}",
            luks_device, mapper_name
        );
        return Ok(LuksContainer {
            device: luks_device,
            mapper_name,
            mapped_path,
        });
    }

    // Format LUKS container (with or without integrity)
    if integrity {
        luks_format_integrity(&luks_device, password)?;
    } else {
        luks_format(&luks_device, password)?;
    }

    // Open LUKS container
    luks_open(&luks_device, &mapper_name, password)?;

    info!(
        "LUKS encryption setup complete: {} -> {}",
        luks_device, mapped_path
    );

    Ok(LuksContainer {
        device: luks_device,
        mapper_name,
        mapped_path,
    })
}

/// Format a device as LUKS2
fn luks_format(device: &str, password: &str) -> Result<()> {
    luks_format_inner(device, password, false)
}

/// Format a device as LUKS2 with dm-integrity (HMAC-SHA256 per-sector integrity)
fn luks_format_integrity(device: &str, password: &str) -> Result<()> {
    luks_format_inner(device, password, true)
}

/// Internal LUKS2 format implementation
fn luks_format_inner(device: &str, password: &str, integrity: bool) -> Result<()> {
    if integrity {
        info!(
            "Formatting {} as LUKS2 container with dm-integrity (aes-xts-plain64, argon2id, hmac-sha256)",
            device
        );
    } else {
        info!(
            "Formatting {} as LUKS2 container (aes-xts-plain64, argon2id)",
            device
        );
    }

    let mut args = vec![
        "luksFormat",
        "--type",
        "luks2",
        "--cipher",
        "aes-xts-plain64",
        "--key-size",
        "512",
        "--hash",
        "sha512",
        "--pbkdf",
        "argon2id",
        "--batch-mode",
    ];

    // Add integrity flag for dm-integrity support
    if integrity {
        args.push("--integrity");
        args.push("hmac-sha256");
        // Use 4096 sector size for optimal performance with integrity
        args.push("--sector-size");
        args.push("4096");
    }

    args.push(device);

    // Use stdin to pass password securely (fixes command injection vulnerability)
    let mut child = Command::new("cryptsetup")
        .args(&args)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DeploytixError::CommandFailed {
            command: "cryptsetup luksFormat".to_string(),
            stderr: e.to_string(),
        })?;

    // Write password to stdin with newline - required by cryptsetup
    if let Some(ref mut stdin) = child.stdin {
        writeln!(stdin, "{}", password)?;
    }
    drop(child.stdin.take()); // Close stdin to signal EOF

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DeploytixError::CommandFailed {
            command: "cryptsetup luksFormat".to_string(),
            stderr: format!("Failed to format LUKS container: {}", stderr),
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
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DeploytixError::CommandFailed {
            command: "cryptsetup open".to_string(),
            stderr: e.to_string(),
        })?;

    if let Some(ref mut stdin) = child.stdin {
        // Write password with newline - required by cryptsetup when reading from stdin
        writeln!(stdin, "{}", password)?;
    }
    drop(child.stdin.take()); // Close stdin to signal EOF

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DeploytixError::CommandFailed {
            command: "cryptsetup open".to_string(),
            stderr: format!("Failed to open LUKS container: {}", stderr),
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

    let password = config.disk.encryption_password.as_ref().ok_or_else(|| {
        DeploytixError::ValidationError("Encryption password required".to_string())
    })?;

    let boot_device = partition_path(device, boot_partition);
    let mapper_name = config.disk.luks_boot_mapper_name.clone();
    let mapped_path = format!("/dev/mapper/{}", mapper_name);

    info!(
        "Setting up LUKS1 encryption on {} for /boot (mapper: {})",
        boot_device, mapper_name
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] cryptsetup luksFormat --type luks1 {}",
            boot_device
        );
        println!(
            "  [dry-run] cryptsetup open {} {}",
            boot_device, mapper_name
        );
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

    info!(
        "LUKS1 boot encryption setup complete: {} -> {}",
        boot_device, mapped_path
    );

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
    info!(
        "Formatting {} as LUKS1 container (aes-xts-plain64, pbkdf2)",
        device
    );

    let mut child = Command::new("cryptsetup")
        .args([
            "luksFormat",
            "--type",
            "luks1",
            "--cipher",
            "aes-xts-plain64",
            "--key-size",
            "512",
            "--hash",
            "sha512",
            "--batch-mode",
            device,
        ])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DeploytixError::CommandFailed {
            command: "cryptsetup luksFormat (LUKS1)".to_string(),
            stderr: e.to_string(),
        })?;

    // Write password with newline - required by cryptsetup
    if let Some(ref mut stdin) = child.stdin {
        writeln!(stdin, "{}", password)?;
    }
    drop(child.stdin.take());

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DeploytixError::CommandFailed {
            command: "cryptsetup luksFormat (LUKS1)".to_string(),
            stderr: format!("Failed to format LUKS1 container for /boot: {}", stderr),
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

/// Setup LUKS2 encryption for a single partition
///
/// Creates and opens a LUKS2 container on the specified device.
/// Used for LVM thin provisioning layout where a single LUKS container holds the LVM PV.
pub fn setup_single_luks(
    cmd: &CommandRunner,
    device: &str,
    password: &str,
    mapper_name: &str,
) -> Result<LuksContainer> {
    setup_single_luks_inner(cmd, device, password, mapper_name, false)
}

/// Setup LUKS2 encryption with dm-integrity for a single partition
///
/// Same as `setup_single_luks` but adds per-sector HMAC-SHA256 integrity protection.
pub fn setup_single_luks_with_integrity(
    cmd: &CommandRunner,
    device: &str,
    password: &str,
    mapper_name: &str,
) -> Result<LuksContainer> {
    setup_single_luks_inner(cmd, device, password, mapper_name, true)
}

fn setup_single_luks_inner(
    cmd: &CommandRunner,
    device: &str,
    password: &str,
    mapper_name: &str,
    integrity: bool,
) -> Result<LuksContainer> {
    let mapped_path = format!("/dev/mapper/{}", mapper_name);

    if integrity {
        info!(
            "Setting up LUKS2 encryption with dm-integrity on {} (mapper: {})",
            device, mapper_name
        );
    } else {
        info!(
            "Setting up LUKS2 encryption on {} (mapper: {})",
            device, mapper_name
        );
    }

    if cmd.is_dry_run() {
        let integrity_flag = if integrity {
            " --integrity hmac-sha256"
        } else {
            ""
        };
        println!(
            "  [dry-run] cryptsetup luksFormat --type luks2{} {}",
            integrity_flag, device
        );
        println!("  [dry-run] cryptsetup open {} {}", device, mapper_name);
        return Ok(LuksContainer {
            device: device.to_string(),
            mapper_name: mapper_name.to_string(),
            mapped_path,
        });
    }

    // Format LUKS container (with or without integrity)
    if integrity {
        luks_format_integrity(device, password)?;
    } else {
        luks_format(device, password)?;
    }

    // Open LUKS container
    luks_open(device, mapper_name, password)?;

    info!(
        "LUKS2 encryption setup complete: {} -> {}",
        device, mapped_path
    );

    Ok(LuksContainer {
        device: device.to_string(),
        mapper_name: mapper_name.to_string(),
        mapped_path,
    })
}

/// Setup LUKS2 encryption for multiple partitions (multi-volume encryption)
///
/// Creates and opens LUKS containers for ROOT, USR, VAR, and HOME partitions.
/// Each container gets a unique mapper name (e.g., Crypt-Root, Crypt-Usr, etc.).
pub fn setup_multi_volume_encryption(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    device: &str,
    luks_partitions: &[(u32, &str)], // (partition_number, name)
) -> Result<Vec<LuksContainer>> {
    if !config.disk.encryption {
        return Err(DeploytixError::ConfigError(
            "Encryption not enabled in configuration".to_string(),
        ));
    }

    let password = config.disk.encryption_password.as_ref().ok_or_else(|| {
        DeploytixError::ValidationError("Encryption password required".to_string())
    })?;

    let integrity = config.disk.integrity;
    let mut containers = Vec::new();

    for (part_num, name) in luks_partitions {
        let luks_device = partition_path(device, *part_num);
        // Convert partition name to title case (e.g., "ROOT" -> "Root")
        let title_case_name = to_title_case(name);
        let mapper_name = format!("Crypt-{}", title_case_name);
        let mapped_path = format!("/dev/mapper/{}", mapper_name);

        if integrity {
            info!(
                "Setting up LUKS2 encryption with dm-integrity on {} (mapper: {})",
                luks_device, mapper_name
            );
        } else {
            info!(
                "Setting up LUKS2 encryption on {} (mapper: {})",
                luks_device, mapper_name
            );
        }

        if cmd.is_dry_run() {
            let integrity_flag = if integrity {
                " --integrity hmac-sha256"
            } else {
                ""
            };
            println!(
                "  [dry-run] cryptsetup luksFormat --type luks2{} {}",
                integrity_flag, luks_device
            );
            println!(
                "  [dry-run] cryptsetup open {} {}",
                luks_device, mapper_name
            );
        } else {
            // Format LUKS container (with or without integrity)
            if integrity {
                luks_format_integrity(&luks_device, password)?;
            } else {
                luks_format(&luks_device, password)?;
            }

            // Open LUKS container
            luks_open(&luks_device, &mapper_name, password)?;
        }

        info!(
            "LUKS encryption setup complete: {} -> {}",
            luks_device, mapped_path
        );

        containers.push(LuksContainer {
            device: luks_device,
            mapper_name,
            mapped_path,
        });
    }

    info!(
        "Multi-volume encryption setup complete: {} containers created",
        containers.len()
    );
    Ok(containers)
}

/// Close multiple LUKS containers
pub fn close_multi_luks(cmd: &CommandRunner, containers: &[LuksContainer]) -> Result<()> {
    info!("Closing {} LUKS containers", containers.len());

    // Close in reverse order (home, var, usr, root)
    for container in containers.iter().rev() {
        close_luks(cmd, &container.mapper_name)?;
    }

    Ok(())
}
