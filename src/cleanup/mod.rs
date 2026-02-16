//! Cleanup and uninstall functionality (Undeploytix)

use crate::disk::detection::list_block_devices;
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use crate::utils::prompt::{prompt_confirm, prompt_select};
use std::fs;
use tracing::info;

/// Install root path
const INSTALL_ROOT: &str = "/install";

/// Cleanup utility
pub struct Cleaner {
    cmd: CommandRunner,
}

impl Cleaner {
    pub fn new(dry_run: bool) -> Self {
        Self {
            cmd: CommandRunner::new(dry_run),
        }
    }

    /// Perform cleanup operations
    pub fn cleanup(&self, device: Option<&str>, wipe: bool) -> Result<()> {
        info!(
            "Starting cleanup (unmount, close LUKS{})",
            if wipe { ", wipe" } else { "" }
        );

        // Unmount all filesystems
        self.unmount_all()?;

        // Close any LUKS containers
        self.close_encrypted_volumes()?;

        // Wipe if requested
        if wipe {
            let device = if let Some(d) = device {
                d.to_string()
            } else {
                self.prompt_for_device()?
            };

            self.wipe_device(&device)?;
        }

        info!("Cleanup complete (all resources released)");
        Ok(())
    }

    /// Unmount all filesystems under install root
    fn unmount_all(&self) -> Result<()> {
        info!("Unmounting all filesystems under {}", INSTALL_ROOT);

        // Disable swap first
        let _ = self.cmd.run("swapoff", &["-a"]);

        // Get mount points under install root
        let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
        let mut mount_points: Vec<&str> = mounts
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 && parts[1].starts_with(INSTALL_ROOT) {
                    Some(parts[1])
                } else {
                    None
                }
            })
            .collect();

        // Sort by depth (deepest first)
        mount_points.sort_by_key(|b| std::cmp::Reverse(b.matches('/').count()));

        // Unmount each
        for mp in mount_points {
            info!("Unmounting {}", mp);
            let _ = self.cmd.run("umount", &[mp]);
        }

        Ok(())
    }

    /// Close any open LUKS encrypted volumes
    fn close_encrypted_volumes(&self) -> Result<()> {
        info!("Closing any open LUKS encrypted volumes");

        // Common mapper names used in the original script
        let mapper_names = [
            "Crypt-Boot",
            "Crypt-Swap",
            "Crypt-Root",
            "Crypt-Usr",
            "Crypt-Var",
            "Crypt-Home",
        ];

        for name in mapper_names {
            let mapper_path = format!("/dev/mapper/{}", name);
            if std::path::Path::new(&mapper_path).exists() {
                info!("Closing {}", name);
                let _ = self.cmd.run("cryptsetup", &["close", name]);
            }
        }

        Ok(())
    }

    /// Prompt user for device to wipe
    fn prompt_for_device(&self) -> Result<String> {
        let devices = list_block_devices(true)?;

        if devices.is_empty() {
            return Err(DeploytixError::ConfigError(
                "No block devices found".to_string(),
            ));
        }

        let items: Vec<String> = devices
            .iter()
            .map(|d| {
                format!(
                    "{} - {} {}",
                    d.path,
                    d.size_human(),
                    d.model.as_deref().unwrap_or("")
                )
            })
            .collect();

        let idx = prompt_select("Select device to wipe", &items, 0)?;
        Ok(devices[idx].path.clone())
    }

    /// Wipe partition table from device
    fn wipe_device(&self, device: &str) -> Result<()> {
        // Confirm
        let warning = format!(
            "This will WIPE the partition table on {}. This cannot be undone!",
            device
        );
        println!("\n⚠️  WARNING: {}\n", warning);

        if !prompt_confirm("Are you sure you want to continue?", false)? {
            return Err(DeploytixError::UserCancelled);
        }

        info!(
            "Wiping partition table and filesystem signatures on {}",
            device
        );

        if self.cmd.is_dry_run() {
            println!("  [dry-run] Would wipe partition table on {}", device);
            return Ok(());
        }

        // Wipe filesystem signatures
        self.cmd.run("wipefs", &["-a", device])?;

        // Create blank GPT
        // Using sfdisk to write empty GPT
        let script = "label: gpt\n";
        let script_path = "/tmp/deploytix_wipe";
        fs::write(script_path, script)?;

        let result = std::process::Command::new("sfdisk")
            .arg(device)
            .stdin(fs::File::open(script_path)?)
            .output();

        let _ = fs::remove_file(script_path);

        if let Ok(output) = result {
            if !output.status.success() {
                // Fall back to fdisk
                let _ = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(format!("echo -e 'g\\nw' | fdisk {}", device))
                    .output();
            }
        }

        info!("Partition table wiped and blank GPT created on {}", device);
        Ok(())
    }
}
