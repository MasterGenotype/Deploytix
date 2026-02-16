//! SecureBoot configuration and signing
//!
//! Provides functions for:
//! - Generating or installing SecureBoot keys
//! - Signing EFI binaries (bootloader, kernel)
//! - Creating pacman hooks for automatic signing
//! - Key enrollment guidance

use crate::config::{DeploymentConfig, SecureBootMethod};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::info;

/// SecureBoot key paths (sbctl default locations)
pub const SBCTL_KEYS_DIR: &str = "/usr/share/secureboot/keys";

/// Setup SecureBoot keys based on the chosen method
pub fn setup_secureboot(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.system.secureboot {
        return Ok(());
    }

    info!("Setting up SecureBoot with method: {:?}", config.system.secureboot_method);

    match config.system.secureboot_method {
        SecureBootMethod::Sbctl => setup_sbctl(cmd, install_root)?,
        SecureBootMethod::ManualKeys => {
            let keys_path = config
                .system
                .secureboot_keys_path
                .as_ref()
                .ok_or_else(|| DeploytixError::ValidationError(
                    "SecureBoot keys path required for ManualKeys method".to_string()
                ))?;
            setup_manual_keys(cmd, keys_path, install_root)?;
        }
        SecureBootMethod::Shim => setup_shim(cmd, install_root)?,
    }

    // Create pacman hook for automatic signing
    create_signing_hook(cmd, config, install_root)?;

    info!("SecureBoot setup complete");
    Ok(())
}

/// Setup SecureBoot using sbctl
fn setup_sbctl(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Setting up SecureBoot with sbctl");

    if cmd.is_dry_run() {
        println!("  [dry-run] sbctl create-keys");
        println!("  [dry-run] sbctl enroll-keys --microsoft");
        return Ok(());
    }

    // Create keys
    cmd.run_in_chroot(install_root, "sbctl create-keys")
        .map_err(|e| DeploytixError::CommandFailed {
            command: "sbctl create-keys".to_string(),
            stderr: e.to_string(),
        })?;

    // Enroll keys (include Microsoft keys for compatibility)
    // Note: This only works if the system is booted in setup mode
    // User may need to do this manually from UEFI settings
    let enroll_result = cmd.run_in_chroot(install_root, "sbctl enroll-keys --microsoft");
    
    if enroll_result.is_err() {
        info!("Note: Key enrollment may need to be done manually from UEFI setup");
        info!("After first boot, run: sbctl enroll-keys --microsoft");
    }

    Ok(())
}

/// Setup SecureBoot with user-provided keys
fn setup_manual_keys(cmd: &CommandRunner, keys_path: &str, install_root: &str) -> Result<()> {
    info!("Setting up SecureBoot with manual keys from {}", keys_path);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would copy keys from {} to {}", keys_path, install_root);
        return Ok(());
    }

    // Expected key files
    let expected_files = [
        "PK.key", "PK.crt",    // Platform Key
        "KEK.key", "KEK.crt",  // Key Exchange Key
        "db.key", "db.crt",    // Signature Database
    ];

    // Verify keys exist
    for file in &expected_files {
        let path = format!("{}/{}", keys_path, file);
        if !std::path::Path::new(&path).exists() {
            return Err(DeploytixError::ValidationError(
                format!("Missing SecureBoot key file: {}", path)
            ));
        }
    }

    // Create target directory
    let target_dir = format!("{}/etc/secureboot/keys", install_root);
    fs::create_dir_all(&target_dir)?;

    // Copy keys
    for file in &expected_files {
        let src = format!("{}/{}", keys_path, file);
        let dst = format!("{}/{}", target_dir, file);
        fs::copy(&src, &dst)?;
        
        // Secure permissions for key files
        if file.ends_with(".key") {
            let mut perms = fs::metadata(&dst)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&dst, perms)?;
        }
    }

    info!("Manual keys installed to {}", target_dir);
    Ok(())
}

/// Setup SecureBoot using shim (MOK enrollment)
fn setup_shim(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Setting up SecureBoot with shim (MOK enrollment)");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would install shim-signed and configure MOK");
        return Ok(());
    }

    // Generate MOK keys
    let mok_dir = format!("{}/etc/secureboot/MOK", install_root);
    fs::create_dir_all(&mok_dir)?;

    // Generate MOK key pair
    cmd.run_in_chroot(
        install_root,
        &format!(
            "openssl req -new -x509 -newkey rsa:2048 -keyout /etc/secureboot/MOK/MOK.key \
             -out /etc/secureboot/MOK/MOK.crt -nodes -days 36500 -subj '/CN=Deploytix MOK/'"
        ),
    )
    .map_err(|e| DeploytixError::CommandFailed {
        command: "openssl (MOK generation)".to_string(),
        stderr: e.to_string(),
    })?;

    // Convert to DER format for enrollment
    cmd.run_in_chroot(
        install_root,
        "openssl x509 -in /etc/secureboot/MOK/MOK.crt -out /etc/secureboot/MOK/MOK.der -outform DER",
    )
    .map_err(|e| DeploytixError::CommandFailed {
        command: "openssl (DER conversion)".to_string(),
        stderr: e.to_string(),
    })?;

    // Secure key permissions
    let key_path = format!("{}/MOK.key", mok_dir);
    let mut perms = fs::metadata(&key_path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&key_path, perms)?;

    info!("MOK keys generated. User will need to enroll via mokutil on first boot.");
    info!("Run: mokutil --import /etc/secureboot/MOK/MOK.der");

    Ok(())
}

/// Sign an EFI binary
pub fn sign_efi_binary(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    binary_path: &str,
    install_root: &str,
) -> Result<()> {
    if !config.system.secureboot {
        return Ok(());
    }

    info!("Signing EFI binary: {}", binary_path);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would sign {}", binary_path);
        return Ok(());
    }

    let full_path = format!("{}{}", install_root, binary_path);

    match config.system.secureboot_method {
        SecureBootMethod::Sbctl => {
            // sbctl tracks and signs files
            cmd.run_in_chroot(
                install_root,
                &format!("sbctl sign -s {}", binary_path),
            )
            .map_err(|e| DeploytixError::CommandFailed {
                command: format!("sbctl sign {}", binary_path),
                stderr: e.to_string(),
            })?;
        }
        SecureBootMethod::ManualKeys | SecureBootMethod::Shim => {
            // Use sbsign directly
            let (key, cert) = get_signing_key_paths(config, install_root);
            
            cmd.run(
                "sbsign",
                &[
                    "--key", &key,
                    "--cert", &cert,
                    "--output", &full_path,
                    &full_path,
                ],
            )
            .map_err(|e| DeploytixError::CommandFailed {
                command: format!("sbsign {}", binary_path),
                stderr: e.to_string(),
            })?;
        }
    }

    info!("Signed: {}", binary_path);
    Ok(())
}

/// Get paths to signing key and certificate
fn get_signing_key_paths(config: &DeploymentConfig, install_root: &str) -> (String, String) {
    match config.system.secureboot_method {
        SecureBootMethod::Sbctl => {
            // Use sbctl's default key locations
            let keys_dir = format!("{}{}", install_root, SBCTL_KEYS_DIR);
            (
                format!("{}/db/db.key", keys_dir),
                format!("{}/db/db.pem", keys_dir),
            )
        }
        SecureBootMethod::ManualKeys => {
            (
                format!("{}/etc/secureboot/keys/db.key", install_root),
                format!("{}/etc/secureboot/keys/db.crt", install_root),
            )
        }
        SecureBootMethod::Shim => {
            (
                format!("{}/etc/secureboot/MOK/MOK.key", install_root),
                format!("{}/etc/secureboot/MOK/MOK.crt", install_root),
            )
        }
    }
}

/// Sign all boot-related files
pub fn sign_boot_files(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.system.secureboot {
        return Ok(());
    }

    info!("Signing boot files for SecureBoot");

    // Files to sign
    let files_to_sign = [
        "/boot/efi/EFI/BOOT/BOOTX64.EFI",  // GRUB EFI binary
        "/boot/vmlinuz-linux-zen",          // Kernel
    ];

    for file in &files_to_sign {
        let full_path = format!("{}{}", install_root, file);
        if std::path::Path::new(&full_path).exists() {
            sign_efi_binary(cmd, config, file, install_root)?;
        }
    }

    // For sbctl, use sign-all to catch any missed files
    if config.system.secureboot_method == SecureBootMethod::Sbctl {
        let _ = cmd.run_in_chroot(install_root, "sbctl sign-all");
    }

    Ok(())
}

/// Create pacman hook for automatic kernel signing
fn create_signing_hook(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    info!("Creating pacman hook for automatic SecureBoot signing");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would create /etc/pacman.d/hooks/99-secureboot.hook");
        return Ok(());
    }

    let hooks_dir = format!("{}/etc/pacman.d/hooks", install_root);
    fs::create_dir_all(&hooks_dir)?;

    let hook_content = match config.system.secureboot_method {
        SecureBootMethod::Sbctl => {
            r#"[Trigger]
Operation = Install
Operation = Upgrade
Type = Path
Target = usr/lib/modules/*/vmlinuz
Target = boot/vmlinuz-*

[Action]
Description = Signing kernel for SecureBoot...
When = PostTransaction
Exec = /usr/bin/sbctl sign-all
Depends = sbctl
"#
        }
        SecureBootMethod::ManualKeys | SecureBootMethod::Shim => {
            // For manual signing, create a script
            create_manual_signing_script(install_root)?;
            r#"[Trigger]
Operation = Install
Operation = Upgrade
Type = Path
Target = usr/lib/modules/*/vmlinuz
Target = boot/vmlinuz-*

[Action]
Description = Signing kernel for SecureBoot...
When = PostTransaction
Exec = /usr/local/bin/sign-kernel
"#
        }
    };

    let hook_path = format!("{}/99-secureboot.hook", hooks_dir);
    fs::write(&hook_path, hook_content)?;

    info!("Created SecureBoot signing hook");
    Ok(())
}

/// Create manual signing script for non-sbctl methods
fn create_manual_signing_script(install_root: &str) -> Result<()> {
    let script_dir = format!("{}/usr/local/bin", install_root);
    fs::create_dir_all(&script_dir)?;

    let script = r#"#!/bin/bash
# Sign kernel for SecureBoot

KEY="/etc/secureboot/keys/db.key"
CERT="/etc/secureboot/keys/db.crt"

# Fall back to MOK if db keys don't exist
if [ ! -f "$KEY" ]; then
    KEY="/etc/secureboot/MOK/MOK.key"
    CERT="/etc/secureboot/MOK/MOK.crt"
fi

if [ ! -f "$KEY" ] || [ ! -f "$CERT" ]; then
    echo "Error: SecureBoot keys not found"
    exit 1
fi

# Sign all kernels
for kernel in /boot/vmlinuz-*; do
    if [ -f "$kernel" ]; then
        echo "Signing $kernel..."
        sbsign --key "$KEY" --cert "$CERT" --output "$kernel" "$kernel"
    fi
done

# Sign GRUB if present
if [ -f /boot/efi/EFI/BOOT/BOOTX64.EFI ]; then
    echo "Signing GRUB..."
    sbsign --key "$KEY" --cert "$CERT" \
        --output /boot/efi/EFI/BOOT/BOOTX64.EFI \
        /boot/efi/EFI/BOOT/BOOTX64.EFI
fi

echo "SecureBoot signing complete"
"#;

    let script_path = format!("{}/sign-kernel", script_dir);
    fs::write(&script_path, script)?;
    
    let mut perms = fs::metadata(&script_path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms)?;

    Ok(())
}

/// Print SecureBoot enrollment instructions
pub fn print_enrollment_instructions(config: &DeploymentConfig) {
    if !config.system.secureboot {
        return;
    }

    println!("\n📋 SecureBoot Enrollment Instructions:");
    println!("======================================");

    match config.system.secureboot_method {
        SecureBootMethod::Sbctl => {
            println!("1. Reboot into UEFI firmware settings");
            println!("2. Enable 'Setup Mode' or clear existing keys");
            println!("3. Boot into the new system");
            println!("4. Run: sbctl enroll-keys --microsoft");
            println!("5. Reboot and enable SecureBoot in UEFI settings");
        }
        SecureBootMethod::ManualKeys => {
            println!("1. Copy your EFI signature list files to EFI partition");
            println!("2. Reboot into UEFI firmware settings");
            println!("3. Navigate to Key Management");
            println!("4. Enroll your PK, KEK, and db keys");
            println!("5. Enable SecureBoot");
        }
        SecureBootMethod::Shim => {
            println!("1. Boot into the new system");
            println!("2. Run: mokutil --import /etc/secureboot/MOK/MOK.der");
            println!("3. Enter a one-time password when prompted");
            println!("4. Reboot - MokManager will appear");
            println!("5. Select 'Enroll MOK' and enter the password");
            println!("6. SecureBoot should now work with signed kernels");
        }
    }

    println!();
}

/// Verify SecureBoot status
#[allow(dead_code)]
pub fn verify_secureboot_status() -> Result<bool> {
    use std::process::Command;

    let output = Command::new("sbctl")
        .arg("status")
        .output()
        .map_err(|e| DeploytixError::CommandFailed {
            command: "sbctl status".to_string(),
            stderr: e.to_string(),
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.contains("Secure Boot: enabled"))
}
