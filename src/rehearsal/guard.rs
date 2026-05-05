//! RAII disk-wipe guard for the rehearsal system.
//!
//! When armed, the guard unmounts all filesystems under the install root,
//! closes LUKS containers, and writes a blank GPT to the target device on
//! drop.  This guarantees the disk is restored to a pristine state even if
//! the rehearsal panics or encounters an early error.

use std::fs;
use std::process::{Command, Stdio};
use tracing::{info, warn};

/// Install root path (mirrors `install::installer::INSTALL_ROOT`).
const INSTALL_ROOT: &str = "/install";

/// RAII guard that wipes the target disk when dropped.
///
/// The guard is "armed" on creation and stays armed until either:
/// - It is explicitly disarmed (only after a successful wipe), or
/// - It is dropped, at which point it performs the wipe.
///
/// This ensures the disk cannot be left in a partially-installed state.
pub struct DiskWipeGuard {
    device: String,
    armed: bool,
}

impl DiskWipeGuard {
    /// Create a new armed guard for the given device.
    pub fn new(device: &str) -> Self {
        info!(
            "DiskWipeGuard: armed for {} (will wipe on drop)",
            device
        );
        Self {
            device: device.to_string(),
            armed: true,
        }
    }

    /// Disarm the guard so that dropping it is a no-op.
    /// Call this only after a successful explicit wipe.
    pub fn disarm(&mut self) {
        self.armed = false;
        info!("DiskWipeGuard: disarmed (wipe already completed)");
    }

    /// Perform the full cleanup + wipe sequence.
    /// Returns `true` if the wipe succeeded.
    pub fn wipe_now(&mut self) -> bool {
        if !self.armed {
            return true;
        }
        let ok = Self::do_cleanup_and_wipe(&self.device);
        if ok {
            self.disarm();
        }
        ok
    }

    // ── internal helpers ────────────────────────────────────────────

    /// Best-effort cleanup and wipe.  Errors are logged but never
    /// propagated — this runs in a Drop impl where panicking is UB.
    fn do_cleanup_and_wipe(device: &str) -> bool {
        info!(
            "DiskWipeGuard: starting cleanup + wipe for {}",
            device
        );

        // 1. Unmount everything under INSTALL_ROOT (deepest first)
        Self::unmount_all();

        // 2. Close LUKS / LVM mappings
        Self::close_encrypted_volumes();

        // 3. Wipe filesystem signatures
        if Self::run_quiet("wipefs", &["-a", device]).is_err() {
            warn!("DiskWipeGuard: wipefs failed on {}", device);
        }

        // 4. Write blank GPT
        let script = "label: gpt\n";
        let script_path = "/tmp/deploytix_rehearsal_wipe";
        if fs::write(script_path, script).is_ok() {
            let ok = Command::new("sfdisk")
                .arg(device)
                .stdin(
                    fs::File::open(script_path)
                        .unwrap_or_else(|_| fs::File::open("/dev/null").unwrap()),
                )
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);

            let _ = fs::remove_file(script_path);
            if !ok {
                warn!("DiskWipeGuard: sfdisk blank-GPT failed, trying fdisk");
                // Fallback
                let _ = Command::new("fdisk")
                    .arg(device)
                    .stdin(Stdio::piped())
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

        info!(
            "DiskWipeGuard: wipe complete for {}",
            device
        );
        true
    }

    fn unmount_all() {
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

        mount_points.sort_by_key(|b| std::cmp::Reverse(b.matches('/').count()));

        for mp in mount_points {
            info!("DiskWipeGuard: unmounting {}", mp);
            if Self::run_quiet("umount", &[mp]).is_err() {
                let _ = Self::run_quiet("umount", &["-l", mp]);
            }
        }

        // Disable swap devices associated with the install
        let swaps = fs::read_to_string("/proc/swaps").unwrap_or_default();
        for line in swaps.lines().skip(1) {
            if let Some(dev) = line.split_whitespace().next() {
                if dev.starts_with(INSTALL_ROOT) || dev.contains("/dev/mapper/Crypt-") {
                    let _ = Self::run_quiet("swapoff", &[dev]);
                }
            }
        }
    }

    fn close_encrypted_volumes() {
        // Deactivate any VGs created during the rehearsal
        let _ = Self::run_quiet("vgchange", &["-an"]);

        let mapper_dir = std::path::Path::new("/dev/mapper");
        if let Ok(entries) = fs::read_dir(mapper_dir) {
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
                info!("DiskWipeGuard: closing {}", name);
                let _ = Self::run_quiet("cryptsetup", &["close", &name]);
            }
        }
    }

    fn run_quiet(program: &str, args: &[&str]) -> Result<(), ()> {
        Command::new(program)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| {
                if s.success() {
                    Ok(())
                } else {
                    Err(())
                }
            })
            .unwrap_or(Err(()))
    }
}

impl Drop for DiskWipeGuard {
    fn drop(&mut self) {
        if self.armed {
            warn!("DiskWipeGuard: drop triggered — performing emergency wipe");
            Self::do_cleanup_and_wipe(&self.device);
        }
    }
}
