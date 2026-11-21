//! Cleanup and uninstall functionality (Undeploytix)

pub mod guards;

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

        // Unmount all filesystems, then close any LUKS containers. Both carry
        // on past a failure so as much as possible is released, then report
        // what is still held.
        let mut held = self.unmount_all();
        held.extend(self.close_encrypted_volumes());

        if !held.is_empty() {
            // A busy filesystem or an open container on the target: wiping
            // now would rewrite the partition table under live mappings.
            return Err(DeploytixError::CommandFailed {
                command: "cleanup".to_string(),
                stderr: format!(
                    "still in use after cleanup: {}{}",
                    held.join(", "),
                    if wipe { "; not wiping the device" } else { "" }
                ),
            });
        }

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

    /// Unmount all filesystems under install root.
    ///
    /// Returns the mount points still mounted afterwards (empty in dry-run).
    fn unmount_all(&self) -> Vec<String> {
        info!("Unmounting all filesystems under {}", INSTALL_ROOT);

        // Disable swap devices that were set up for the installation
        // (avoid disabling all host swap with -a)
        let mounts = fs::read_to_string("/proc/swaps").unwrap_or_default();
        for line in mounts.lines().skip(1) {
            if let Some(device) = line.split_whitespace().next() {
                if device.starts_with(INSTALL_ROOT) || device.contains("/dev/mapper/Crypt-") {
                    let _ = self.cmd.run("swapoff", &[device]);
                }
            }
        }

        // Get mount points under install root
        let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
        let mut mount_points = mounts_under(&mounts, INSTALL_ROOT);

        // Sort by depth (deepest first)
        mount_points.sort_by_key(|b| std::cmp::Reverse(b.matches('/').count()));

        // Unmount each
        for mp in &mount_points {
            let mp = mp.as_str();
            info!("Unmounting {}", mp);
            if let Err(e) = self.cmd.run("umount", &[mp]) {
                tracing::warn!("Failed to unmount {}: {} (trying lazy unmount)", mp, e);
                if let Err(e2) = self.cmd.run("umount", &["-l", mp]) {
                    tracing::warn!("Lazy unmount of {} also failed: {}", mp, e2);
                }
            }
        }

        if self.cmd.is_dry_run() {
            return Vec::new();
        }
        // A lazy unmount detaches the mount even while it is busy; whatever
        // still holds it then keeps its LUKS container open, which
        // close_encrypted_volumes reports.
        mounts_under(
            &fs::read_to_string("/proc/mounts").unwrap_or_default(),
            INSTALL_ROOT,
        )
    }

    /// Close any open LUKS encrypted volumes
    ///
    /// Dynamically enumerates `/dev/mapper/Crypt-*` and
    /// `/dev/mapper/temporary-cryptsetup-*` entries so that both
    /// canonical names (e.g. `Crypt-Root`) and disambiguated names
    /// (e.g. `Crypt-Root-1`) are closed, as well as any temporary
    /// dm mappings left behind by interrupted `cryptsetup luksFormat`
    /// operations.
    ///
    /// Returns the mappings still open afterwards (empty in dry-run).
    fn close_encrypted_volumes(&self) -> Vec<String> {
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
                    if is_cleanup_mapping(&name) {
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

        if self.cmd.is_dry_run() {
            return Vec::new();
        }
        open_cleanup_mappings(mapper_dir)
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
        let script_path = crate::utils::paths::runtime_path("wipe_script");
        let script_path = script_path.as_str();
        fs::write(script_path, script)?;

        let sfdisk = std::process::Command::new("sfdisk")
            .arg(device)
            .stdin(fs::File::open(script_path)?)
            .output();

        let _ = fs::remove_file(script_path);

        let sfdisk_err = match sfdisk {
            Ok(out) if out.status.success() => None,
            Ok(out) => Some(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            Err(e) => Some(e.to_string()),
        };
        if let Some(sfdisk_err) = sfdisk_err {
            tracing::warn!(
                "sfdisk could not write a blank GPT ({}); trying fdisk",
                sfdisk_err
            );
            // Piped stdin, no shell interpolation.
            let fdisk = std::process::Command::new("fdisk")
                .arg(device)
                .stdin(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    if let Some(mut stdin) = child.stdin.take() {
                        use std::io::Write;
                        stdin.write_all(b"g\nw\n")?;
                    }
                    child.wait_with_output()
                });
            match fdisk {
                Ok(out) if out.status.success() => {}
                Ok(out) => {
                    return Err(DeploytixError::PartitionError(format!(
                        "could not write a blank GPT to {device}: sfdisk: {sfdisk_err}; fdisk: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    )))
                }
                Err(e) => {
                    return Err(DeploytixError::PartitionError(format!(
                        "could not write a blank GPT to {device}: sfdisk: {sfdisk_err}; fdisk: {e}"
                    )))
                }
            }
        }

        info!("Partition table wiped and blank GPT created on {}", device);
        Ok(())
    }
}

/// Mount points in `mounts` (the text of `/proc/mounts`) at or under `root`.
fn mounts_under(mounts: &str, root: &str) -> Vec<String> {
    mounts
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter(|mp| *mp == root || mp.starts_with(&format!("{root}/")))
        .map(str::to_string)
        .collect()
}

/// Whether a `/dev/mapper` name is one cleanup is responsible for closing.
fn is_cleanup_mapping(name: &str) -> bool {
    name.starts_with("Crypt-") || name.starts_with("temporary-cryptsetup-")
}

/// The cleanup-owned mappings present in `mapper_dir`.
fn open_cleanup_mappings(mapper_dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(mapper_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| is_cleanup_mapping(n))
        .map(|n| format!("/dev/mapper/{n}"))
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mounts_under_takes_the_root_and_below_only() {
        let mounts = "/dev/sda2 / ext4 rw 0 0\n\
                      /dev/mapper/Crypt-Root /install btrfs rw 0 0\n\
                      /dev/sdb1 /install/boot/efi vfat rw 0 0\n\
                      /dev/sdc1 /installer ext4 rw 0 0\n";
        assert_eq!(
            mounts_under(mounts, "/install"),
            vec!["/install", "/install/boot/efi"]
        );
    }

    #[test]
    fn only_deploytix_mappings_are_cleanups_business() {
        assert!(is_cleanup_mapping("Crypt-Root"));
        assert!(is_cleanup_mapping("Crypt-Root-1"));
        assert!(is_cleanup_mapping("temporary-cryptsetup-1234"));
        assert!(!is_cleanup_mapping("control"));
        assert!(!is_cleanup_mapping("luks-1234"));
    }

    #[test]
    fn open_mappings_lists_only_cleanup_owned_names() {
        let dir = std::env::temp_dir().join(format!("deploytix-mapper-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for n in ["control", "Crypt-Root", "Crypt-Home", "luks-x"] {
            fs::write(dir.join(n), "").unwrap();
        }
        let open = open_cleanup_mappings(&dir);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            open,
            vec!["/dev/mapper/Crypt-Home", "/dev/mapper/Crypt-Root"]
        );
    }

    #[test]
    fn dry_run_cleanup_reports_nothing_held() {
        let c = Cleaner::new(true);
        assert!(c.close_encrypted_volumes().is_empty());
    }
}
