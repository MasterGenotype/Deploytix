//! Cleanup and uninstall functionality (Undeploytix)

use crate::disk::detection::{list_block_devices, target_swap_devices};
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

        // Resolve the wipe target up front. It has to happen before the unmount
        // rather than inside the `if wipe` below, because unmount_all needs the
        // disk to tell the install's swap apart from the host's.
        let wipe_target: Option<String> = if wipe {
            Some(match device {
                Some(d) => d.to_string(),
                None => self.prompt_for_device()?,
            })
        } else {
            None
        };

        // A `--device` given without `--wipe` still scopes the swap sweep.
        let swap_device = wipe_target.as_deref().or(device);

        // Unmount all filesystems
        self.unmount_all(swap_device)?;

        // Close any LUKS containers
        self.close_encrypted_volumes()?;

        // Wipe if requested
        if let Some(target) = wipe_target.as_deref() {
            self.wipe_device(target)?;
        }

        info!("Cleanup complete (all resources released)");
        Ok(())
    }

    /// Unmount all filesystems under install root.
    ///
    /// `device` is the target disk when known. Without it the swap sweep can
    /// only match staged swapfiles and `Crypt-` mappers — which is why the
    /// caller resolves the device first: the installer enables swap on a raw
    /// partition (`/dev/sda3`), so a filter that looks only at those two would
    /// leave the target's swap on and block the wipe that follows.
    fn unmount_all(&self, device: Option<&str>) -> Result<()> {
        info!("Unmounting all filesystems under {}", INSTALL_ROOT);

        // Disable the install's swap devices — never `-a`, which would take the
        // live session's zram and any host swap down with them.
        let swaps = fs::read_to_string("/proc/swaps").unwrap_or_default();
        for dev in target_swap_devices(&swaps, INSTALL_ROOT, device) {
            info!("Disabling swap on {}", dev);
            let _ = self.cmd.run("swapoff", &[&dev]);
        }

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
            if let Err(e) = self.cmd.run("umount", &[mp]) {
                tracing::warn!("Failed to unmount {}: {} (trying lazy unmount)", mp, e);
                if let Err(e2) = self.cmd.run("umount", &["-l", mp]) {
                    tracing::warn!("Lazy unmount of {} also failed: {}", mp, e2);
                }
            }
        }

        Ok(())
    }

    /// Close any open LUKS encrypted volumes
    ///
    /// Dynamically enumerates `/dev/mapper/Crypt-*` and
    /// `/dev/mapper/temporary-cryptsetup-*` entries so that both
    /// canonical names (e.g. `Crypt-Root`) and disambiguated names
    /// (e.g. `Crypt-Root-1`) are closed, as well as any temporary
    /// dm mappings left behind by interrupted `cryptsetup luksFormat`
    /// operations.
    fn close_encrypted_volumes(&self) -> Result<()> {
        info!("Closing any open LUKS encrypted volumes");

        // Kill orphaned cryptsetup processes first (they hold dm mappings open)
        self.kill_orphaned_cryptsetup();

        let mapper_dir = std::path::Path::new("/dev/mapper");
        if let Ok(entries) = fs::read_dir(mapper_dir) {
            // Collect and sort in reverse so deeper volumes close before root
            let mut names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with("Crypt-") || name.starts_with("temporary-cryptsetup-") {
                        Some(name)
                    } else {
                        None
                    }
                })
                .collect();
            names.sort();
            names.reverse();

            for name in names {
                info!("Closing {}", name);
                if let Err(e) = self.cmd.run("cryptsetup", &["close", &name]) {
                    tracing::warn!("Failed to close LUKS volume {}: {}", name, e);
                }
            }
        }

        Ok(())
    }

    /// Kill orphaned `cryptsetup` processes (PPID == 1) that may be holding
    /// dm mappings open (e.g. integrity wipe from an interrupted luksFormat).
    fn kill_orphaned_cryptsetup(&self) {
        use tracing::warn;

        let Ok(proc_entries) = fs::read_dir("/proc") else {
            return;
        };

        for entry in proc_entries.filter_map(|e| e.ok()) {
            let pid_str = entry.file_name().to_string_lossy().to_string();
            let Ok(pid) = pid_str.parse::<u32>() else {
                continue;
            };

            let cmdline_path = format!("/proc/{}/cmdline", pid);
            let Ok(cmdline) = fs::read_to_string(&cmdline_path) else {
                continue;
            };

            if !cmdline.starts_with("cryptsetup\0") && !cmdline.starts_with("cryptsetup ") {
                continue;
            }

            // Check if orphaned (PPID == 1)
            let stat_path = format!("/proc/{}/stat", pid);
            let Ok(stat) = fs::read_to_string(&stat_path) else {
                continue;
            };
            if let Some(after_comm) = stat.rfind(')') {
                let fields: Vec<&str> = stat[after_comm + 1..].split_whitespace().collect();
                if fields.len() >= 2 && fields[1] == "1" {
                    info!("Killing orphaned cryptsetup process (PID {})", pid);
                    if self.cmd.is_dry_run() {
                        println!("  [dry-run] Would kill PID {}", pid);
                        continue;
                    }
                    unsafe {
                        libc::kill(pid as i32, libc::SIGTERM);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    if std::path::Path::new(&format!("/proc/{}", pid)).exists() {
                        warn!("SIGTERM failed, sending SIGKILL to PID {}", pid);
                        unsafe {
                            libc::kill(pid as i32, libc::SIGKILL);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                }
            }
        }
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
                // Fall back to fdisk with piped stdin (no shell interpolation)
                let _ = std::process::Command::new("fdisk")
                    .arg(device)
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .and_then(|mut child| {
                        if let Some(ref mut stdin) = child.stdin {
                            use std::io::Write;
                            let _ = stdin.write_all(b"g\nw\n");
                        }
                        child.wait()
                    });
            }
        }

        info!("Partition table wiped and blank GPT created on {}", device);
        Ok(())
    }
}
