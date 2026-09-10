//! Swap configuration: ZRAM and swap file support
//!
//! Provides alternatives to traditional swap partitions:
//! - ZRAM: Compressed RAM-based swap with higher priority
//! - Swap file: File-based swap on btrfs or ext4

use crate::config::{DeploymentConfig, InitSystem, SwapType};
use crate::disk::detection::get_ram_mib;
use crate::disk::detection::partition_path;
use crate::disk::layouts::ComputedLayout;
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::{info, warn};

/// Default swap file path, for a writable root.
pub const SWAP_FILE_PATH: &str = "/swap/swapfile";

/// Swap file path on an immutable root.
///
/// `/swap` lives inside `@` (btrfs) or inside the dm-verity slot (LVM A/B).
/// Both are mounted read-only at boot, so `swapon` on a file there fails
/// outright; and on btrfs `@` is snapshotted, so the file's physical extents --
/// which is exactly what `resume_offset=` names -- stop meaning anything the
/// first time a set is taken. `/var` is writable and shared across sets on both
/// backends, so a swap file there survives an update and keeps its offset.
pub const IMMUTABLE_SWAP_FILE_PATH: &str = "/var/swap/swapfile";

/// Where the swap file lives for this config.
pub fn swap_file_path(config: &DeploymentConfig) -> &'static str {
    if config.packages.immutable_root {
        IMMUTABLE_SWAP_FILE_PATH
    } else {
        SWAP_FILE_PATH
    }
}

/// What the kernel needs in order to resume from hibernation.
///
/// `device` is a `resume=` value: a `UUID=…` spec where the backing device has
/// one, else a device path. `offset` is set only for a swap *file*, where the
/// kernel needs the image's physical offset within that device as well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeParams {
    pub device: String,
    pub offset: Option<u64>,
}

impl ResumeParams {
    /// The cmdline fragments, in the order the kernel documents them.
    pub fn cmdline_parts(&self) -> Vec<String> {
        let mut parts = vec![format!("resume={}", self.device)];
        if let Some(offset) = self.offset {
            parts.push(format!("resume_offset={}", offset));
        }
        parts
    }
}

/// Fixed ZRAM size: 4 GiB in bytes.
const ZRAM_SIZE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Setup ZRAM swap device
///
/// Creates a init service that configures ZRAM at boot with a fixed 4 GiB device.
/// ZRAM provides compressed in-memory swap with configurable compression algorithm.
pub fn setup_zram(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let algorithm = &config.disk.zram_algorithm;

    info!("Setting up ZRAM: 4 GiB fixed, compression: {}", algorithm);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would create ZRAM service: 4 GiB, {} compression",
            algorithm
        );
        return Ok(());
    }

    match config.system.init {
        InitSystem::Runit => setup_zram_runit(install_root, algorithm)?,
        InitSystem::OpenRC => setup_zram_openrc(install_root, algorithm)?,
        InitSystem::S6 => setup_zram_s6(cmd, install_root, algorithm)?,
        InitSystem::Dinit => setup_zram_dinit(install_root, algorithm)?,
    }

    info!("ZRAM service configured successfully");
    Ok(())
}

/// Create ZRAM runit service
fn setup_zram_runit(install_root: &str, algorithm: &str) -> Result<()> {
    let sv_dir = format!("{}/etc/runit/sv/zram", install_root);
    fs::create_dir_all(&sv_dir)?;

    // Create run script
    let run_script = format!(
        r#"#!/bin/sh
exec 2>&1

# Load zram module
modprobe zram num_devices=1

# Configure zram0 with fixed 4 GiB size
echo {algorithm} > /sys/block/zram0/comp_algorithm
echo {size} > /sys/block/zram0/disksize

# Setup swap
mkswap /dev/zram0
swapon -p 100 /dev/zram0

# Keep service running
exec pause
"#,
        algorithm = algorithm,
        size = ZRAM_SIZE_BYTES
    );

    let run_path = format!("{}/run", sv_dir);
    fs::write(&run_path, run_script)?;

    // Make executable
    let mut perms = fs::metadata(&run_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&run_path, perms)?;

    // Create finish script for cleanup
    let finish_script = r#"#!/bin/sh
swapoff /dev/zram0 2>/dev/null
echo 1 > /sys/block/zram0/reset 2>/dev/null
"#;

    let finish_path = format!("{}/finish", sv_dir);
    fs::write(&finish_path, finish_script)?;
    let mut perms = fs::metadata(&finish_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&finish_path, perms)?;

    // Enable the service by creating a symlink in the default runsvdir
    let link_dir = format!("{}/etc/runit/runsvdir/default", install_root);
    fs::create_dir_all(&link_dir)?;
    std::os::unix::fs::symlink("/etc/runit/sv/zram", format!("{}/zram", link_dir))?;

    info!("Created and enabled runit ZRAM service at {}", sv_dir);
    Ok(())
}

/// Create ZRAM OpenRC service
fn setup_zram_openrc(install_root: &str, algorithm: &str) -> Result<()> {
    let init_dir = format!("{}/etc/init.d", install_root);
    fs::create_dir_all(&init_dir)?;

    let init_script = format!(
        r#"#!/sbin/openrc-run

description="ZRAM swap device"

depend() {{
    need localmount
    before swap
}}

start() {{
    ebegin "Starting ZRAM swap"
    
    modprobe zram num_devices=1
    echo {algorithm} > /sys/block/zram0/comp_algorithm
    echo {size} > /sys/block/zram0/disksize
    mkswap /dev/zram0
    swapon -p 100 /dev/zram0
    
    eend $?
}}

stop() {{
    ebegin "Stopping ZRAM swap"
    swapoff /dev/zram0 2>/dev/null
    echo 1 > /sys/block/zram0/reset 2>/dev/null
    eend $?
}}
"#,
        algorithm = algorithm,
        size = ZRAM_SIZE_BYTES
    );

    let script_path = format!("{}/zram", init_dir);
    fs::write(&script_path, init_script)?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    // Enable the service in the default runlevel
    let runlevel_dir = format!("{}/etc/runlevels/default", install_root);
    fs::create_dir_all(&runlevel_dir)?;
    std::os::unix::fs::symlink("/etc/init.d/zram", format!("{}/zram", runlevel_dir))?;

    info!("Created and enabled OpenRC ZRAM service at {}", script_path);
    Ok(())
}

/// Create ZRAM s6 service
///
/// Follows the structure used by the `zram-s6` AUR package
/// (upstream: github.com/Senderman/s6-services).  The service is an s6-rc
/// oneshot, so the startup script lives in `up` (not `run`, which is for
/// longruns).  It is written to `/etc/s6/adminsv`, the directory Artix
/// reserves for admin-defined s6-rc services.  Configuration is stored in
/// `/etc/s6/config/zram.conf` and read at boot via `envfile`.
///
/// No `dependencies.d` entries are declared — like the runit/dinit
/// variants, modprobe and /sys are available by the time the default
/// bundle starts (s6-linux-init mounts the core pseudo-filesystems in
/// stage 1), and the in-house Artix oneshot names previously referenced
/// here are not stable under the upstream-matching s6-scripts.
fn setup_zram_s6(cmd: &CommandRunner, install_root: &str, algorithm: &str) -> Result<()> {
    let sv_dir = format!("{}/etc/s6/adminsv/zram", install_root);
    fs::create_dir_all(&sv_dir)?;

    // /etc/s6/config/zram.conf — configuration read by the up script at boot
    let config_dir = format!("{}/etc/s6/config", install_root);
    fs::create_dir_all(&config_dir)?;

    let config_content = format!(
        "COMP_ALGORITHM={}\nZRAM_SIZE={}\n",
        algorithm, ZRAM_SIZE_BYTES
    );
    fs::write(format!("{}/zram.conf", config_dir), config_content)?;

    // up — execlineb oneshot startup script (mirrors the AUR package)
    let up_script = r#"#!/usr/bin/execlineb -P
fdmove -c 2 1
envfile /etc/s6/config/zram.conf
importas comp_algorithm COMP_ALGORITHM
importas zram_size ZRAM_SIZE

foreground { modprobe zram }
foreground { redirfd -w 1 /sys/block/zram0/comp_algorithm echo $comp_algorithm }
foreground { redirfd -w 1 /sys/block/zram0/disksize echo $zram_size }
foreground { mkswap --label zram0 /dev/zram0 }
swapon --priority 100 /dev/zram0
"#;

    let up_path = format!("{}/up", sv_dir);
    fs::write(&up_path, up_script)?;
    let mut perms = fs::metadata(&up_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&up_path, perms)?;

    // down — teardown script run when the service is stopped
    let down_script = r#"#!/usr/bin/execlineb -P
fdmove -c 2 1
foreground { swapoff /dev/zram0 }
redirfd -w 1 /sys/block/zram0/reset echo 1
"#;

    let down_path = format!("{}/down", sv_dir);
    fs::write(&down_path, down_script)?;
    let mut perms = fs::metadata(&down_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&down_path, perms)?;

    // type — declares this as an s6-rc oneshot (runs once, not supervised)
    fs::write(format!("{}/type", sv_dir), "oneshot\n")?;

    // The zram definition was just written into /etc/s6/adminsv — rebuild
    // the reference database so `s6 set enable` can see it, then add the
    // service to the default bundle.  The staged change is compiled and
    // installed as the boot database (`s6 set commit` + `s6 live install
    // --init`) in the finalize phase.
    crate::configure::services::sync_service_repository(
        cmd,
        &crate::config::InitSystem::S6,
        install_root,
    )?;
    cmd.run_in_chroot(install_root, "s6 set enable zram")?;

    info!("Created and enabled s6 ZRAM service at {}", sv_dir);
    Ok(())
}

/// Create ZRAM dinit service
fn setup_zram_dinit(install_root: &str, algorithm: &str) -> Result<()> {
    let dinit_dir = format!("{}/etc/dinit.d", install_root);
    fs::create_dir_all(&dinit_dir)?;

    // Create setup script
    let script_dir = format!("{}/usr/local/bin", install_root);
    fs::create_dir_all(&script_dir)?;

    let setup_script = format!(
        r#"#!/bin/sh
# Fixed 4 GiB ZRAM swap device
modprobe zram num_devices=1
echo {algorithm} > /sys/block/zram0/comp_algorithm
echo {size} > /sys/block/zram0/disksize
mkswap /dev/zram0
swapon -p 100 /dev/zram0
"#,
        algorithm = algorithm,
        size = ZRAM_SIZE_BYTES
    );

    let script_path = format!("{}/zram-setup", script_dir);
    fs::write(&script_path, setup_script)?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    // Create dinit service file.
    // Note: no dependency declared — modprobe and /sys are available
    // early in the boot sequence and do not require a mount service.
    let service_content = r#"type = scripted
command = /usr/local/bin/zram-setup
"#;

    let service_path = format!("{}/zram", dinit_dir);
    fs::write(&service_path, service_content)?;

    // Enable the service by creating a symlink in boot.d
    let boot_d = format!("{}/etc/dinit.d/boot.d", install_root);
    fs::create_dir_all(&boot_d)?;
    std::os::unix::fs::symlink("/etc/dinit.d/zram", format!("{}/zram", boot_d))?;

    info!("Created and enabled dinit ZRAM service at {}", service_path);
    Ok(())
}

/// Create a swap file
///
/// For btrfs: Uses `btrfs filesystem mkswapfile` (kernel 6.1+) or fallback method.
/// For ext4: Uses fallocate + mkswap.
pub fn create_swap_file(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let size_mib = if config.disk.swap_file_size_mib > 0 {
        config.disk.swap_file_size_mib
    } else {
        // Auto-calculate: 2x RAM, capped at 16 GiB
        let ram_mib = get_ram_mib();
        std::cmp::min(ram_mib * 2, 16384)
    };

    let swap_file = format!("{}{}", install_root, swap_file_path(config));
    let swap_dir = swap_file
        .rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_else(|| format!("{}/swap", install_root));

    info!("Creating {} MiB swap file at {}", size_mib, swap_file);

    if cmd.is_dry_run() {
        println!("  [dry-run] mkdir -p {}", swap_dir);
        println!(
            "  [dry-run] Create {} MiB swap file at {}",
            size_mib, swap_file
        );
        return Ok(());
    }

    // Create swap directory
    fs::create_dir_all(&swap_dir)?;

    // Allocation strategy is per-filesystem: see `create_regular_swap_file`.
    let fs_type = fs_type_of(&swap_dir);

    if fs_type == "btrfs" {
        create_btrfs_swap_file(cmd, &swap_file, size_mib)?;
    } else {
        create_regular_swap_file(cmd, &swap_file, size_mib, &fs_type)?;
    }

    // Set permissions
    let mut perms = fs::metadata(&swap_file)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&swap_file, perms)?;

    // Format as swap
    cmd.run("mkswap", &[&swap_file])
        .map_err(|e| DeploytixError::CommandFailed {
            command: "mkswap".to_string(),
            stderr: e.to_string(),
        })?;

    info!("Swap file created successfully");
    Ok(())
}

/// The filesystem type holding `path`, as `stat -f` names it.
///
/// Note the names are `stat`'s, not mkfs's: every ext filesystem — ext4
/// included — reports as `ext2/ext3`. Returns an empty string when `stat`
/// cannot be run, which callers treat as "unknown" and handle conservatively.
fn fs_type_of(path: &str) -> String {
    use std::process::Command;

    Command::new("stat")
        .args(["-f", "-c", "%T", path])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Whether `fs_type` is one where `fallocate` produces a swappable file.
///
/// `swapon` refuses a file that contains holes or unwritten extents, and it
/// resolves blocks with `bmap` rather than going through the filesystem. On the
/// ext family a fallocated file is fully mapped and works. On XFS `fallocate`
/// leaves *unwritten* extents, so `swapon` fails with "skipping - it appears to
/// have holes" — XFS swap files have to be written out, which is why the
/// upstream advice for XFS is `dd`, not `fallocate`. F2FS is stricter still
/// (a swap file must be contiguous and pinned), so it takes the same path.
/// An unknown type is treated as not-fallocatable: writing the file out is
/// slower but always correct.
fn fallocate_is_swappable(fs_type: &str) -> bool {
    matches!(fs_type, "ext2/ext3" | "ext4")
}

/// Create swap file on btrfs
///
/// Uses `btrfs filesystem mkswapfile` if available (kernel 6.1+),
/// otherwise falls back to chattr + truncate method.
fn create_btrfs_swap_file(cmd: &CommandRunner, path: &str, size_mib: u64) -> Result<()> {
    info!("Creating btrfs swap file at {}", path);

    // Try the modern mkswapfile command first (kernel 6.1+)
    let result = cmd.run(
        "btrfs",
        &[
            "filesystem",
            "mkswapfile",
            "--size",
            &format!("{}m", size_mib),
            path,
        ],
    );

    if result.is_ok() {
        return Ok(());
    }

    // Fallback: manual creation
    info!("Falling back to manual btrfs swap file creation");

    // Create empty file
    std::fs::File::create(path)?;

    // Disable COW
    cmd.run("chattr", &["+C", path])
        .map_err(|e| DeploytixError::CommandFailed {
            command: "chattr +C".to_string(),
            stderr: e.to_string(),
        })?;

    // Allocate space
    cmd.run("fallocate", &["-l", &format!("{}M", size_mib), path])
        .map_err(|e| DeploytixError::CommandFailed {
            command: "fallocate".to_string(),
            stderr: e.to_string(),
        })?;

    Ok(())
}

/// Create a swap file on a non-btrfs filesystem.
///
/// `fallocate` on the ext family, `dd` everywhere else — see
/// [`fallocate_is_swappable`] for why a fallocated file is not swappable on XFS
/// or F2FS. `dd` is slower (it writes the whole file) but produces real,
/// written-out extents on every filesystem.
fn create_regular_swap_file(
    cmd: &CommandRunner,
    path: &str,
    size_mib: u64,
    fs_type: &str,
) -> Result<()> {
    if fallocate_is_swappable(fs_type) {
        info!("Creating swap file at {} via fallocate ({})", path, fs_type);
        cmd.run("fallocate", &["-l", &format!("{}M", size_mib), path])
            .map_err(|e| DeploytixError::CommandFailed {
                command: "fallocate".to_string(),
                stderr: e.to_string(),
            })?;
        return Ok(());
    }

    info!(
        "Creating swap file at {} via dd ({} needs written-out extents; \
         a fallocated file is not swappable there)",
        path,
        if fs_type.is_empty() {
            "unknown filesystem"
        } else {
            fs_type
        }
    );
    cmd.run(
        "dd",
        &[
            "if=/dev/zero",
            &format!("of={}", path),
            "bs=1M",
            &format!("count={}", size_mib),
        ],
    )
    .map_err(|e| DeploytixError::CommandFailed {
        command: "dd".to_string(),
        stderr: e.to_string(),
    })?;

    Ok(())
}

/// Get swap file physical offset for hibernation resume
///
/// Required for hibernation with swap file on btrfs.
/// Returns the physical offset that should be used in `resume_offset=` kernel parameter.
pub fn get_swap_file_offset(swap_file: &str) -> Result<u64> {
    use std::process::Command;

    info!("Getting swap file offset for {}", swap_file);

    // Try btrfs-specific command first
    let output = Command::new("btrfs")
        .args(["inspect-internal", "map-swapfile", "-r", swap_file])
        .output();

    if let Ok(out) = output {
        if out.status.success() {
            let offset_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Ok(offset) = offset_str.parse::<u64>() {
                return Ok(offset);
            }
        }
    }

    // Fallback: use filefrag
    let output = Command::new("filefrag")
        .args(["-v", swap_file])
        .output()
        .map_err(|e| DeploytixError::CommandFailed {
            command: "filefrag".to_string(),
            stderr: e.to_string(),
        })?;

    if !output.status.success() {
        return Err(DeploytixError::CommandFailed {
            command: "filefrag".to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    // Parse filefrag output to get physical offset
    // Format: "   0:        0..    8191:     123456..    131647: ..."
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("0:") && line.contains("..") {
            // Find the physical start offset
            let parts: Vec<&str> = line.split_whitespace().collect();
            for (i, part) in parts.iter().enumerate() {
                if part.contains("..") && i > 0 {
                    // Previous part should be the logical extent, next should be physical
                    if let Some(phys_part) = parts.get(i + 1) {
                        let phys_str = phys_part.split("..").next().unwrap_or("0");
                        if let Ok(offset) = phys_str.parse::<u64>() {
                            return Ok(offset);
                        }
                    }
                }
            }
        }
    }

    Err(DeploytixError::CommandFailed {
        command: "get_swap_file_offset".to_string(),
        stderr: "Could not determine swap file offset".to_string(),
    })
}

/// The block device backing whatever filesystem holds `path`, as a `resume=`
/// spec.
///
/// Asked of the kernel rather than derived from the layout: `findmnt --target`
/// answers for every backend the installer supports -- a plain partition, a
/// LUKS mapper, an LVM LV -- and stays right when a layout gains a case nobody
/// updated this function for. The `[/@subvol]` suffix btrfs sources carry is
/// stripped; a UUID is preferred over the path because device names are not
/// stable across boots, and the initramfs resolves `UUID=` either way.
fn backing_device_spec(path: &str) -> Result<String> {
    let out = std::process::Command::new("findmnt")
        .args(["-no", "SOURCE", "--target", path])
        .output()
        .map_err(|e| DeploytixError::CommandFailed {
            command: "findmnt".to_string(),
            stderr: e.to_string(),
        })?;
    if !out.status.success() {
        return Err(DeploytixError::CommandFailed {
            command: "findmnt".to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        });
    }
    let source = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // btrfs reports `/dev/mapper/Crypt-Var[/@var]`; the device is the head.
    let device = source
        .split('[')
        .next()
        .unwrap_or(&source)
        .trim()
        .to_string();
    if device.is_empty() {
        return Err(DeploytixError::CommandFailed {
            command: "findmnt".to_string(),
            stderr: format!("no source device for {path}"),
        });
    }

    match crate::disk::formatting::get_partition_uuid(&device) {
        Ok(uuid) => Ok(format!("UUID={uuid}")),
        // No UUID (or blkid could not read it): the path still resolves, and
        // the initramfs waits on it the same way.
        Err(e) => {
            info!("No UUID for {device} ({e}); using the device path for resume=");
            Ok(device)
        }
    }
}

/// Resolve `resume=` / `resume_offset=` for this config, or `None` when
/// hibernation is off.
///
/// Called with the target still mounted at `install_root`, because the swap
/// file's offset can only be read from the real file and the backing device
/// only from the live mount table.
///
/// Both immutable backends keep swap off the read-only root -- a swap
/// partition is never brought into LUKS or LVM (`mark_data_partitions_as_luks`
/// and `apply_lvm_thin_to_layout` both exclude `is_swap`), and a swap file goes
/// to `/var` -- so the device named here is always one the initramfs can reach
/// by the time the resume attempt runs.
pub fn resume_params(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    layout: &ComputedLayout,
    device: &str,
    install_root: &str,
) -> Result<Option<ResumeParams>> {
    if !config.system.hibernation {
        return Ok(None);
    }

    if cmd.is_dry_run() {
        return Ok(Some(ResumeParams {
            device: "UUID=XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX".to_string(),
            offset: match config.disk.swap_type {
                SwapType::FileZram => Some(0),
                _ => None,
            },
        }));
    }

    match config.disk.swap_type {
        SwapType::Partition => {
            let Some(swap) = layout.partitions.iter().find(|p| p.is_swap) else {
                warn!("hibernation is on but the layout has no swap partition; no resume=");
                return Ok(None);
            };
            let swap_device = partition_path(device, swap.number);
            let uuid = crate::disk::formatting::get_partition_uuid(&swap_device)?;
            info!("Hibernation resumes from swap partition {swap_device}");
            Ok(Some(ResumeParams {
                device: format!("UUID={uuid}"),
                offset: None,
            }))
        }
        SwapType::FileZram => {
            let swap_file = format!("{}{}", install_root, swap_file_path(config));
            let spec = backing_device_spec(&swap_file)?;
            let offset = get_swap_file_offset(&swap_file)?;
            info!("Hibernation resumes from swap file {swap_file} (offset {offset} on {spec})");
            Ok(Some(ResumeParams {
                device: spec,
                offset: Some(offset),
            }))
        }
        // Rejected by validation -- there is no persistent device to write the
        // image to, so there is nothing to name here either.
        SwapType::ZramOnly => Ok(None),
    }
}

/// Configure swap based on SwapType
pub fn configure_swap(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    match config.disk.swap_type {
        SwapType::Partition => {
            // Swap partition is handled by layout and fstab
            info!("Using swap partition (configured via layout)");
            Ok(())
        }
        SwapType::FileZram => {
            // Setup both ZRAM and swap file
            setup_zram(cmd, config, install_root)?;
            create_swap_file(cmd, config, install_root)?;

            // The offset itself is read (and written into the cmdline) by
            // `resume_params` during the bootloader phase, once the file is in
            // its final place. Reported here only so the log shows it early.
            if config.system.hibernation {
                let swap_file = format!("{}{}", install_root, swap_file_path(config));
                match get_swap_file_offset(&swap_file) {
                    Ok(offset) => info!("Swap file offset for hibernation: {}", offset),
                    Err(e) => warn!(
                        "Could not determine swap file offset: {} (hibernation may not work)",
                        e
                    ),
                }
            }
            Ok(())
        }
        SwapType::ZramOnly => {
            // Setup ZRAM only
            setup_zram(cmd, config, install_root)?;
            Ok(())
        }
    }
}

/// Generate fstab entry for swap file
pub fn swap_file_fstab_entry(config: &DeploymentConfig) -> String {
    format!(
        "{}    none    swap    defaults    0    0\n",
        swap_file_path(config)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutable() -> DeploymentConfig {
        let mut c = DeploymentConfig::sample();
        c.packages.immutable_root = false;
        c
    }

    fn immutable() -> DeploymentConfig {
        let mut c = DeploymentConfig::sample();
        c.packages.immutable_root = true;
        c
    }

    // ── swap_file_fstab_entry ────────────────────────────────────────────────

    // ── swap file allocation strategy ────────────────────────────────────────

    /// `swapon` rejects a file with unwritten extents. fallocate produces those
    /// on XFS (and F2FS wants a pinned, contiguous file), so only the ext family
    /// may take the fast path; everything else, unknown included, must be
    /// written out with dd.
    #[test]
    fn only_ext_filesystems_may_use_fallocate_for_swap() {
        assert!(fallocate_is_swappable("ext2/ext3"));
        assert!(fallocate_is_swappable("ext4"));

        assert!(!fallocate_is_swappable("xfs"));
        assert!(!fallocate_is_swappable("f2fs"));
        assert!(!fallocate_is_swappable("zfs"));
        // stat could not be run; writing the file out is always correct.
        assert!(!fallocate_is_swappable(""));
    }

    /// btrfs never reaches `create_regular_swap_file` at all -- it has its own
    /// path, because a swap file there additionally needs COW off.
    #[test]
    fn btrfs_is_not_handled_by_the_regular_allocator() {
        assert!(!fallocate_is_swappable("btrfs"));
    }

    #[test]
    fn swap_file_fstab_entry_uses_correct_swap_file_path() {
        let entry = swap_file_fstab_entry(&mutable());
        assert!(
            entry.contains(SWAP_FILE_PATH),
            "fstab entry must reference SWAP_FILE_PATH, got: {}",
            entry
        );
    }

    #[test]
    fn swap_file_fstab_entry_has_swap_type_and_defaults() {
        let entry = swap_file_fstab_entry(&mutable());
        assert!(entry.contains("swap"), "fstab entry must specify type=swap");
        assert!(
            entry.contains("none"),
            "fstab entry mount point must be 'none'"
        );
        assert!(
            entry.contains("defaults"),
            "fstab entry must include 'defaults' options"
        );
    }

    #[test]
    fn swap_file_fstab_entry_ends_with_newline() {
        let entry = swap_file_fstab_entry(&mutable());
        assert!(entry.ends_with('\n'), "fstab entry must end with newline");
    }

    // ── swap file placement ──────────────────────────────────────────────────

    /// The whole reason the path is config-dependent: `/swap` is inside the
    /// root, which an immutable install mounts read-only and (on btrfs)
    /// snapshots. A swap file there cannot be swapped on, and its
    /// `resume_offset` stops being true the first time a set is taken.
    #[test]
    fn the_immutable_swap_file_lives_on_var() {
        assert_eq!(swap_file_path(&immutable()), IMMUTABLE_SWAP_FILE_PATH);
        assert!(
            swap_file_path(&immutable()).starts_with("/var/"),
            "an immutable root's swap file must be on the writable, \
             non-snapshotted /var"
        );
        assert_eq!(swap_file_path(&mutable()), SWAP_FILE_PATH);
    }

    #[test]
    fn the_immutable_fstab_entry_follows_the_swap_file() {
        let entry = swap_file_fstab_entry(&immutable());
        assert!(
            entry.contains(IMMUTABLE_SWAP_FILE_PATH),
            "fstab must point at the /var swap file, got: {entry}"
        );
    }

    // ── ResumeParams ─────────────────────────────────────────────────────────

    /// A swap partition resumes from the device alone. Emitting a bare
    /// `resume_offset=0` would be wrong, not merely redundant: the kernel would
    /// read the image from the start of the partition's *filesystem* rather
    /// than treat the partition as the swap device.
    #[test]
    fn a_swap_partition_gets_no_offset() {
        let p = ResumeParams {
            device: "UUID=dead-beef".to_string(),
            offset: None,
        };
        assert_eq!(p.cmdline_parts(), vec!["resume=UUID=dead-beef"]);
    }

    /// A swap file cannot resume without one.
    #[test]
    fn a_swap_file_carries_its_offset_after_the_device() {
        let p = ResumeParams {
            device: "UUID=dead-beef".to_string(),
            offset: Some(272384),
        };
        assert_eq!(
            p.cmdline_parts(),
            vec!["resume=UUID=dead-beef", "resume_offset=272384"]
        );
    }

    #[test]
    fn hibernation_off_means_no_resume_params() {
        let cmd = CommandRunner::new(true);
        let mut config = mutable();
        config.system.hibernation = false;
        let layout = ComputedLayout {
            partitions: Vec::new(),
            total_mib: 0,
            subvolumes: None,
            planned_thin_volumes: None,
        };
        assert_eq!(
            resume_params(&cmd, &config, &layout, "/dev/null", "/mnt").unwrap(),
            None
        );
    }
}
