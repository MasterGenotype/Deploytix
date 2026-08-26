//! Cleanup and uninstall functionality (Undeploytix)

use crate::disk::detection::list_block_devices;
use crate::disk::mapping;
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use crate::utils::prompt::{prompt_confirm, prompt_select};
use std::collections::BTreeSet;
use std::fs;
use tracing::{info, warn};

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

        // Resolve the target device up front: when a wipe is requested without
        // one, the answer also scopes the teardown below.
        let target_device: Option<String> = match device {
            Some(d) => Some(d.to_string()),
            None if wipe => Some(self.prompt_for_device()?),
            None => None,
        };

        // Establish which physical disks this run is allowed to touch, before
        // unmounting removes the evidence.  The installation being cleaned up
        // is identified by what it has mounted under INSTALL_ROOT, plus the
        // explicitly named target if there is one.  Anything backed by another
        // disk belongs to the running system: on a deploytix-deployed host the
        // live volumes carry the same Crypt-* names this installer uses.
        let mut targets = mapping::disks_mounted_under(INSTALL_ROOT);
        if let Some(d) = &target_device {
            targets.extend(mapping::backing_disks(d));
        }

        if targets.is_empty() {
            warn!(
                "Nothing is mounted under {} and no device was named; skipping \
                 unmount and LUKS teardown rather than guessing which disk to \
                 act on. Re-run with --device <disk> to scope it.",
                INSTALL_ROOT
            );
        } else {
            info!(
                "Cleanup scoped to disk(s): {}",
                targets
                    .iter()
                    .map(|d| format!("/dev/{}", d))
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            // Unmount all filesystems
            self.unmount_all(&targets)?;

            // Close any LUKS containers
            self.close_encrypted_volumes(&targets)?;
        }

        // Wipe if requested
        if let (true, Some(d)) = (wipe, target_device.as_deref()) {
            self.wipe_device(d)?;
        }

        info!("Cleanup complete (all resources released)");
        Ok(())
    }

    /// Unmount all filesystems under install root
    fn unmount_all(&self, targets: &BTreeSet<String>) -> Result<()> {
        info!("Unmounting all filesystems under {}", INSTALL_ROOT);

        // Disable swap devices that were set up for the installation
        // (avoid disabling all host swap with -a)
        let mounts = fs::read_to_string("/proc/swaps").unwrap_or_default();
        for line in mounts.lines().skip(1) {
            if let Some(device) = line.split_whitespace().next() {
                if mapping::is_under(device, INSTALL_ROOT)
                    || mapping::backed_only_by(device, targets)
                {
                    let _ = self.cmd.run("swapoff", &[device]);
                }
            }
        }

        // Get mount points under install root
        let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
        let mut mount_points: Vec<&str> = mounts
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 && mapping::is_under(parts[1], INSTALL_ROOT) {
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

    /// Close the LUKS mappings belonging to the installation being cleaned up.
    ///
    /// Candidates are the `Crypt-*` and `temporary-cryptsetup-*` dm nodes, but
    /// a name match only makes a node a candidate — it is closed solely when
    /// every disk backing it is one of `targets`, and when it is not serving a
    /// mount outside INSTALL_ROOT.  Both guards matter on a host that was
    /// itself deployed by deploytix, where the live root, usr and home
    /// containers answer to the same names as the ones being installed.
    fn close_encrypted_volumes(&self, targets: &BTreeSet<String>) -> Result<()> {
        info!("Closing LUKS volumes belonging to the installation");

        // Kill orphaned cryptsetup processes first (they hold dm mappings open)
        self.kill_orphaned_cryptsetup();

        let mut names = mapping::mapper_nodes(|name| {
            name.starts_with("Crypt-") || name.starts_with("temporary-cryptsetup-")
        });
        // Reverse order so deeper volumes close before the ones they sit on
        // (e.g. Crypt-LVM_dif before Crypt-LVM).
        names.reverse();

        for name in names {
            let path = format!("/dev/mapper/{}", name);

            if !mapping::backed_only_by(&path, targets) {
                info!(
                    "Leaving {} alone — not backed by the disk being cleaned up",
                    name
                );
                continue;
            }

            let outside = mapping::mounts_outside(&path, INSTALL_ROOT);
            if !outside.is_empty() {
                warn!(
                    "Refusing to close {} — still mounted at {}",
                    name,
                    outside.join(", ")
                );
                continue;
            }

            info!("Closing {}", name);
            if let Err(e) = self.cmd.run("cryptsetup", &["close", &name]) {
                warn!("Failed to close LUKS volume {}: {}", name, e);
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
