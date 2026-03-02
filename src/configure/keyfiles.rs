//! LUKS keyfile generation and management

use crate::configure::encryption::LuksContainer;
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use tracing::info;

/// Keyfile directory path (inside installed system)
pub const KEYFILE_DIR: &str = "/etc/cryptsetup-keys.d";

/// Keyfile size in bytes (512 bytes = 4096 bits)
const KEYFILE_SIZE: usize = 512;

/// Generate a keyfile path for a given volume name
pub fn keyfile_path(volume_name: &str) -> String {
    format!("{}/crypt{}.key", KEYFILE_DIR, volume_name.to_lowercase())
}

/// Generate a secure random keyfile
pub fn generate_keyfile(cmd: &CommandRunner, path: &str) -> Result<()> {
    info!("Generating keyfile: {}", path);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] dd if=/dev/random of={} bs={} count=1 iflag=fullblock",
            path, KEYFILE_SIZE
        );
        println!("  [dry-run] chmod 000 {}", path);
        return Ok(());
    }

    // Create parent directory
    if let Some(parent) = std::path::Path::new(path).parent() {
        fs::create_dir_all(parent)?;
    }

    // Generate random data using /dev/random
    cmd.run(
        "dd",
        &[
            "if=/dev/random",
            &format!("of={}", path),
            &format!("bs={}", KEYFILE_SIZE),
            "count=1",
            "iflag=fullblock",
        ],
    )?;

    // Set restrictive permissions (mode 000 - only root can read via capabilities)
    fs::set_permissions(path, fs::Permissions::from_mode(0o000))?;

    info!("Keyfile generated: {} (mode 000)", path);
    Ok(())
}

/// Add a keyfile to an existing LUKS container (requires password)
pub fn add_keyfile_to_luks(
    cmd: &CommandRunner,
    device: &str,
    password: &str,
    keyfile: &str,
) -> Result<()> {
    info!("Adding keyfile {} to LUKS device {}", keyfile, device);

    if cmd.is_dry_run() {
        println!("  [dry-run] cryptsetup luksAddKey {} {}", device, keyfile);
        return Ok(());
    }

    // Use stdin to pass password securely
    let mut child = Command::new("cryptsetup")
        .args(["luksAddKey", device, keyfile])
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| DeploytixError::CommandFailed {
            command: "cryptsetup luksAddKey".to_string(),
            stderr: e.to_string(),
        })?;

    // Write password to stdin (with newline, as expected by cryptsetup)
    if let Some(ref mut stdin) = child.stdin {
        writeln!(stdin, "{}", password)?;
    }
    drop(child.stdin.take());

    let status = child.wait()?;
    if !status.success() {
        return Err(DeploytixError::CommandFailed {
            command: "cryptsetup luksAddKey".to_string(),
            stderr: format!("Failed to add keyfile to {}", device),
        });
    }

    info!("Keyfile added to LUKS device: {}", device);
    Ok(())
}

/// Volume keyfile information
#[derive(Debug, Clone)]
pub struct VolumeKeyfile {
    /// Volume name (e.g., "Root", "Usr")
    pub volume_name: String,
    /// Path to keyfile
    pub keyfile_path: String,
    /// LUKS device path (kept for reference/debugging)
    #[allow(dead_code)]
    pub device: String,
}

/// Setup keyfiles for all encrypted volumes
///
/// This creates keyfiles in the installed system's /etc/cryptsetup-keys.d/
/// and adds them to each LUKS container for automatic unlocking.
pub fn setup_keyfiles_for_volumes(
    cmd: &CommandRunner,
    containers: &[LuksContainer],
    password: &str,
    install_root: &str,
) -> Result<Vec<VolumeKeyfile>> {
    info!(
        "Setting up keyfiles for {} encrypted volumes",
        containers.len()
    );

    let mut keyfiles = Vec::new();

    // Create keyfile directory in installed system
    let keyfile_dir = format!("{}{}", install_root, KEYFILE_DIR);
    if !cmd.is_dry_run() {
        fs::create_dir_all(&keyfile_dir)?;
        // Set directory permissions to 700
        fs::set_permissions(&keyfile_dir, fs::Permissions::from_mode(0o700))?;
    }

    for container in containers {
        let volume_name = container.volume_name.clone();

        // Generate keyfile path (inside installed system)
        let keyfile_rel = keyfile_path(&volume_name);
        let keyfile_full = format!("{}{}", install_root, keyfile_rel);

        // Generate the keyfile
        generate_keyfile(cmd, &keyfile_full)?;

        // Add keyfile to LUKS container
        add_keyfile_to_luks(cmd, &container.device, password, &keyfile_full)?;

        keyfiles.push(VolumeKeyfile {
            volume_name,
            keyfile_path: keyfile_rel,
            device: container.device.clone(),
        });
    }

    info!("Successfully created {} keyfiles", keyfiles.len());
    Ok(keyfiles)
}

/// Get all keyfile paths for mkinitcpio FILES array
#[allow(dead_code)]
pub fn get_keyfile_paths_for_initramfs(keyfiles: &[VolumeKeyfile]) -> Vec<String> {
    keyfiles.iter().map(|k| k.keyfile_path.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keyfile_path() {
        assert_eq!(keyfile_path("Root"), "/etc/cryptsetup-keys.d/cryptroot.key");
        assert_eq!(keyfile_path("Usr"), "/etc/cryptsetup-keys.d/cryptusr.key");
    }
}
