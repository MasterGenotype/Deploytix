//! Swap configuration: ZRAM and swap file support
//!
//! Provides alternatives to traditional swap partitions:
//! - ZRAM: Compressed RAM-based swap with higher priority
//! - Swap file: File-based swap on btrfs or ext4

use crate::config::{DeploymentConfig, InitSystem, SwapType};
use crate::disk::detection::get_ram_mib;
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::info;

/// Default swap file path
pub const SWAP_FILE_PATH: &str = "/swap/swapfile";

/// Setup ZRAM swap device
///
/// Creates a runit service that configures ZRAM at boot.
/// ZRAM provides compressed in-memory swap with configurable compression algorithm.
pub fn setup_zram(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let percent = config.disk.zram_percent;
    let algorithm = &config.disk.zram_algorithm;

    info!(
        "Setting up ZRAM with {}% of RAM, compression: {}",
        percent, algorithm
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would create ZRAM service with {}% RAM, {} compression",
            percent, algorithm
        );
        return Ok(());
    }

    match config.system.init {
        InitSystem::Runit => setup_zram_runit(install_root, percent, algorithm)?,
        InitSystem::OpenRC => setup_zram_openrc(install_root, percent, algorithm)?,
        InitSystem::S6 => setup_zram_s6(install_root, percent, algorithm)?,
        InitSystem::Dinit => setup_zram_dinit(install_root, percent, algorithm)?,
    }

    info!("ZRAM service configured successfully");
    Ok(())
}

/// Create ZRAM runit service
fn setup_zram_runit(install_root: &str, percent: u8, algorithm: &str) -> Result<()> {
    let sv_dir = format!("{}/etc/runit/sv/zram", install_root);
    fs::create_dir_all(&sv_dir)?;

    // Create run script
    let run_script = format!(
        r#"#!/bin/sh
exec 2>&1

# Calculate ZRAM size based on RAM
RAM_KB=$(grep MemTotal /proc/meminfo | awk '{{print $2}}')
ZRAM_SIZE=$((RAM_KB * {percent} / 100 * 1024))

# Load zram module
modprobe zram num_devices=1

# Configure zram0
echo {algorithm} > /sys/block/zram0/comp_algorithm
echo $ZRAM_SIZE > /sys/block/zram0/disksize

# Setup swap
mkswap /dev/zram0
swapon -p 100 /dev/zram0

# Keep service running
exec pause
"#,
        percent = percent,
        algorithm = algorithm
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

    info!("Created runit ZRAM service at {}", sv_dir);
    Ok(())
}

/// Create ZRAM OpenRC service
fn setup_zram_openrc(install_root: &str, percent: u8, algorithm: &str) -> Result<()> {
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
    
    RAM_KB=$(grep MemTotal /proc/meminfo | awk '{{print $2}}')
    ZRAM_SIZE=$((RAM_KB * {percent} / 100 * 1024))
    
    modprobe zram num_devices=1
    echo {algorithm} > /sys/block/zram0/comp_algorithm
    echo $ZRAM_SIZE > /sys/block/zram0/disksize
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
        percent = percent,
        algorithm = algorithm
    );

    let script_path = format!("{}/zram", init_dir);
    fs::write(&script_path, init_script)?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    info!("Created OpenRC ZRAM service at {}", script_path);
    Ok(())
}

/// Create ZRAM s6 service
fn setup_zram_s6(install_root: &str, percent: u8, algorithm: &str) -> Result<()> {
    let sv_dir = format!("{}/etc/s6/sv/zram", install_root);
    fs::create_dir_all(&sv_dir)?;

    // Create run script
    let run_script = format!(
        r#"#!/bin/execlineb -P
foreground {{
    backtick -n RAM_KB {{ pipeline {{ redirfd -r 0 /proc/meminfo }} grep MemTotal pipeline {{ awk "{{print $2}}" }} }}
    importas -u RAM_KB RAM_KB
    define ZRAM_SIZE ${{RAM_KB * {percent} / 100 * 1024}}
    foreground {{ modprobe zram num_devices=1 }}
    foreground {{ redirfd -w 1 /sys/block/zram0/comp_algorithm echo {algorithm} }}
    foreground {{ redirfd -w 1 /sys/block/zram0/disksize echo $ZRAM_SIZE }}
    foreground {{ mkswap /dev/zram0 }}
    swapon -p 100 /dev/zram0
}}
s6-pause
"#,
        percent = percent,
        algorithm = algorithm
    );

    let run_path = format!("{}/run", sv_dir);
    fs::write(&run_path, run_script)?;
    let mut perms = fs::metadata(&run_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&run_path, perms)?;

    // Create type file
    fs::write(format!("{}/type", sv_dir), "oneshot\n")?;

    info!("Created s6 ZRAM service at {}", sv_dir);
    Ok(())
}

/// Create ZRAM dinit service
fn setup_zram_dinit(install_root: &str, percent: u8, algorithm: &str) -> Result<()> {
    let dinit_dir = format!("{}/etc/dinit.d", install_root);
    fs::create_dir_all(&dinit_dir)?;

    // Create setup script
    let script_dir = format!("{}/usr/local/bin", install_root);
    fs::create_dir_all(&script_dir)?;

    let setup_script = format!(
        r#"#!/bin/sh
RAM_KB=$(grep MemTotal /proc/meminfo | awk '{{print $2}}')
ZRAM_SIZE=$((RAM_KB * {percent} / 100 * 1024))

modprobe zram num_devices=1
echo {algorithm} > /sys/block/zram0/comp_algorithm
echo $ZRAM_SIZE > /sys/block/zram0/disksize
mkswap /dev/zram0
swapon -p 100 /dev/zram0
"#,
        percent = percent,
        algorithm = algorithm
    );

    let script_path = format!("{}/zram-setup", script_dir);
    fs::write(&script_path, setup_script)?;
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    // Create dinit service file
    let service_content = r#"type = scripted
command = /usr/local/bin/zram-setup
depends-on = mount.local
"#;

    let service_path = format!("{}/zram", dinit_dir);
    fs::write(&service_path, service_content)?;

    info!("Created dinit ZRAM service at {}", service_path);
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

    let swap_dir = format!("{}/swap", install_root);
    let swap_file = format!("{}/swapfile", swap_dir);

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

    // Check if btrfs
    let is_btrfs = check_is_btrfs(&swap_dir);

    if is_btrfs {
        create_btrfs_swap_file(cmd, &swap_file, size_mib)?;
    } else {
        create_regular_swap_file(cmd, &swap_file, size_mib)?;
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

/// Check if a path is on a btrfs filesystem
fn check_is_btrfs(path: &str) -> bool {
    use std::process::Command;

    let output = Command::new("stat").args(["-f", "-c", "%T", path]).output();

    match output {
        Ok(out) => {
            let fs_type = String::from_utf8_lossy(&out.stdout).trim().to_string();
            fs_type == "btrfs"
        }
        Err(_) => false,
    }
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

/// Create regular swap file (ext4, xfs, etc.)
fn create_regular_swap_file(cmd: &CommandRunner, path: &str, size_mib: u64) -> Result<()> {
    info!("Creating regular swap file at {}", path);

    cmd.run("fallocate", &["-l", &format!("{}M", size_mib), path])
        .map_err(|e| DeploytixError::CommandFailed {
            command: "fallocate".to_string(),
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

            // If hibernation is enabled, get the swap file offset for resume
            if config.system.hibernation {
                let swap_file = format!("{}{}", install_root, SWAP_FILE_PATH);
                match get_swap_file_offset(&swap_file) {
                    Ok(offset) => {
                        info!("Swap file offset for hibernation: {}", offset);
                        info!(
                            "Add 'resume_offset={}' to kernel parameters for hibernation",
                            offset
                        );
                    }
                    Err(e) => {
                        info!(
                            "Could not determine swap file offset: {} (hibernation may not work)",
                            e
                        );
                    }
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
pub fn swap_file_fstab_entry() -> String {
    format!("{}    none    swap    defaults    0    0\n", SWAP_FILE_PATH)
}
