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
    /// Active mapper name, potentially disambiguated (e.g., Crypt-Root or Crypt-Root-1)
    pub mapper_name: String,
    /// Mapped device path (e.g., /dev/mapper/Crypt-Root)
    pub mapped_path: String,
    /// Canonical volume name for config generation (e.g., "Root", "Usr", "Lvm", "Boot")
    /// Boot-time configs (crypttab, fstab, hooks) use this instead of parsing mapper_name.
    pub volume_name: String,
}

/// Check whether a device-mapper name is already active.
pub fn is_mapper_active(name: &str) -> bool {
    std::path::Path::new(&format!("/dev/mapper/{}", name)).exists()
}

/// Return `desired` if it is not already active, otherwise try
/// `desired-1`, `desired-2`, … up to `-99`.
pub fn resolve_mapper_name(desired: &str) -> String {
    if !is_mapper_active(desired) {
        return desired.to_string();
    }
    tracing::warn!("Mapper name '{}' already in use, disambiguating", desired);
    for i in 1..=99 {
        let candidate = format!("{}-{}", desired, i);
        if !is_mapper_active(&candidate) {
            tracing::info!("Using disambiguated mapper name '{}'", candidate);
            return candidate;
        }
    }
    // Extremely unlikely; fall back to the original and let cryptsetup report the error
    desired.to_string()
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
    let canonical_mapper = config.disk.luks_mapper_name.clone();
    let volume_name = canonical_mapper.trim_start_matches("Crypt-").to_string();
    let mapper_name = resolve_mapper_name(&canonical_mapper);
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
            volume_name,
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
        volume_name,
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

/// Open an existing LUKS container by name.
/// A credential that unlocks a LUKS container.
///
/// A home-preserving recovery install adopts an existing container rather
/// than reformatting it, and the credential that opens it comes from the
/// user — a keyfile carried on the installer host, or a passphrase typed
/// at the prompt. Both are modelled here so callers take one code path.
///
/// Note this is *not* the same thing as the per-volume keyfiles under
/// `/etc/cryptsetup-keys.d` that [`crate::configure::keyfiles`] generates
/// and bakes into the target's initramfs. This one lives on the installer
/// host and is read once.
#[derive(Clone)]
pub enum Credential {
    /// Path to a keyfile readable on the installer host.
    Keyfile(String),
    /// A passphrase.
    Passphrase(String),
}

/// A volume to adopt rather than create during a recovery install.
///
/// The named container is opened with `credential` instead of being
/// `luksFormat`ed, so its data survives the reinstall. Only one volume is
/// adoptable today (`/home`); the field is named rather than positional so
/// the rest of the pipeline can match on it without knowing which.
#[derive(Debug, Clone)]
pub struct AdoptSpec {
    /// Title-case volume name, matching [`LuksContainer::volume_name`]
    /// (e.g. `"Home"`).
    pub volume_name: String,
    /// The credential that unlocks the existing container.
    pub credential: Credential,
}

impl AdoptSpec {
    /// Whether this spec names `volume_name`.
    pub fn matches(&self, volume_name: &str) -> bool {
        self.volume_name.eq_ignore_ascii_case(volume_name)
    }
}

impl std::fmt::Debug for Credential {
    /// Redacts the passphrase — this type ends up in error paths and logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::Keyfile(path) => write!(f, "Keyfile({})", path),
            Credential::Passphrase(_) => write!(f, "Passphrase(<redacted>)"),
        }
    }
}

impl Credential {
    /// How to describe this credential in a log line or error, without
    /// disclosing a passphrase.
    pub fn describe(&self) -> String {
        match self {
            Credential::Keyfile(path) => format!("keyfile {}", path),
            Credential::Passphrase(_) => "passphrase".to_string(),
        }
    }

    /// The `--key-file` arguments this credential contributes, if any.
    fn key_file_args(&self) -> Vec<&str> {
        match self {
            Credential::Keyfile(path) => vec!["--key-file", path],
            Credential::Passphrase(_) => Vec::new(),
        }
    }

    /// The text to write to cryptsetup's stdin, if any.
    fn stdin_secret(&self) -> Option<&str> {
        match self {
            Credential::Keyfile(_) => None,
            Credential::Passphrase(p) => Some(p),
        }
    }
}

/// Run `cryptsetup` with a credential supplied either as `--key-file` or on
/// stdin, and return its output.
///
/// `credential_args` are spliced in before `args`, so callers pass the
/// subcommand and operands and this handles how the secret is delivered.
pub(crate) fn run_cryptsetup(
    subcommand_args: &[&str],
    credential: &Credential,
    label: &str,
) -> Result<std::process::Output> {
    let mut args: Vec<&str> = Vec::new();
    args.extend(credential.key_file_args());
    args.extend_from_slice(subcommand_args);

    let mut child = Command::new("cryptsetup")
        .args(&args)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| DeploytixError::CommandFailed {
            command: label.to_string(),
            stderr: e.to_string(),
        })?;

    // cryptsetup reads the passphrase from stdin; with --key-file it does
    // not, so stdin is simply closed.
    if let Some(secret) = credential.stdin_secret() {
        if let Some(ref mut stdin) = child.stdin {
            writeln!(stdin, "{}", secret)?;
        }
    }
    drop(child.stdin.take());

    child
        .wait_with_output()
        .map_err(|e| DeploytixError::CommandFailed {
            command: label.to_string(),
            stderr: e.to_string(),
        })
}

/// Check that `credential` unlocks `device`, without opening it.
///
/// `cryptsetup open --test-passphrase` validates the credential against the
/// container's keyslots and maps nothing. A recovery install must call this
/// in the prepare phase: if the credential is wrong and we only find out
/// after the disk has been repartitioned, the data the feature exists to
/// preserve is already gone.
pub fn verify_luks_credential(
    cmd: &CommandRunner,
    device: &str,
    credential: &Credential,
) -> Result<()> {
    info!(
        "Verifying {} unlocks LUKS container {}",
        credential.describe(),
        device
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] cryptsetup open --test-passphrase {} ({})",
            device,
            credential.describe()
        );
        return Ok(());
    }

    if let Credential::Keyfile(path) = credential {
        if !std::path::Path::new(path).is_file() {
            return Err(DeploytixError::ValidationError(format!(
                "keyfile {} does not exist or is not a regular file",
                path
            )));
        }
    }

    let output = run_cryptsetup(
        &["open", "--test-passphrase", device],
        credential,
        "cryptsetup open --test-passphrase",
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DeploytixError::CommandFailed {
            command: "cryptsetup open --test-passphrase".to_string(),
            stderr: format!(
                "{} does not unlock {}: {}",
                credential.describe(),
                device,
                stderr.trim()
            ),
        });
    }

    info!("{} unlocks {}", credential.describe(), device);
    Ok(())
}

/// Open an existing LUKS container with either credential type.
///
/// Unlike [`setup_multi_volume_encryption`], this never formats: it adopts
/// a container that already exists.
pub fn open_luks_with(
    cmd: &CommandRunner,
    device: &str,
    mapper_name: &str,
    credential: &Credential,
) -> Result<()> {
    info!(
        "Opening existing LUKS container {} as {} with {}",
        device,
        mapper_name,
        credential.describe()
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] cryptsetup open {} {} ({})",
            device,
            mapper_name,
            credential.describe()
        );
        return Ok(());
    }

    let output = run_cryptsetup(
        &["open", device, mapper_name],
        credential,
        "cryptsetup open",
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DeploytixError::CommandFailed {
            command: "cryptsetup open".to_string(),
            stderr: format!("Failed to open LUKS container: {}", stderr.trim()),
        });
    }

    // Wait for the mapper node to appear.
    std::thread::sleep(std::time::Duration::from_millis(500));

    Ok(())
}

pub fn open_luks(
    cmd: &CommandRunner,
    device: &str,
    mapper_name: &str,
    password: &str,
) -> Result<()> {
    if cmd.is_dry_run() {
        println!("  [dry-run] cryptsetup open {} {}", device, mapper_name);
        return Ok(());
    }
    luks_open(device, mapper_name, password)
}

/// Open a LUKS container (internal)
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
    let canonical_mapper = config.disk.luks_boot_mapper_name.clone();
    let volume_name = canonical_mapper.trim_start_matches("Crypt-").to_string();
    let mapper_name = resolve_mapper_name(&canonical_mapper);
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
            volume_name,
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
        volume_name,
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
    canonical_mapper: &str,
    volume_name: &str,
) -> Result<LuksContainer> {
    setup_single_luks_inner(cmd, device, password, canonical_mapper, volume_name, false)
}

/// Setup LUKS2 encryption with dm-integrity for a single partition
///
/// Same as `setup_single_luks` but adds per-sector HMAC-SHA256 integrity protection.
pub fn setup_single_luks_with_integrity(
    cmd: &CommandRunner,
    device: &str,
    password: &str,
    canonical_mapper: &str,
    volume_name: &str,
) -> Result<LuksContainer> {
    setup_single_luks_inner(cmd, device, password, canonical_mapper, volume_name, true)
}

fn setup_single_luks_inner(
    cmd: &CommandRunner,
    device: &str,
    password: &str,
    canonical_mapper: &str,
    volume_name: &str,
    integrity: bool,
) -> Result<LuksContainer> {
    let mapper_name = resolve_mapper_name(canonical_mapper);
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
            mapper_name: mapper_name.clone(),
            mapped_path,
            volume_name: volume_name.to_string(),
        });
    }

    // Format LUKS container (with or without integrity)
    if integrity {
        luks_format_integrity(device, password)?;
    } else {
        luks_format(device, password)?;
    }

    // Open LUKS container
    luks_open(device, &mapper_name, password)?;

    info!(
        "LUKS2 encryption setup complete: {} -> {}",
        device, mapped_path
    );

    Ok(LuksContainer {
        device: device.to_string(),
        mapper_name,
        mapped_path,
        volume_name: volume_name.to_string(),
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
    adopt: Option<&AdoptSpec>,
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
        let volume_name = to_title_case(name);
        let canonical_mapper = format!("Crypt-{}", volume_name);
        let mapper_name = resolve_mapper_name(&canonical_mapper);
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

        // A volume named by `adopt` already exists and is being preserved:
        // open it, never format it. luksFormat would write a fresh header
        // and destroy every keyslot along with access to the data.
        match adopt.filter(|a| a.matches(&volume_name)) {
            Some(spec) => {
                info!(
                    "Adopting existing LUKS container {} as {} (not reformatting)",
                    luks_device, mapper_name
                );
                open_luks_with(cmd, &luks_device, &mapper_name, &spec.credential)?;
            }
            None => {
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
            }
        }

        info!(
            "LUKS encryption setup complete: {} -> {}",
            luks_device, mapped_path
        );

        containers.push(LuksContainer {
            device: luks_device,
            mapper_name,
            mapped_path,
            volume_name: volume_name.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── to_title_case ────────────────────────────────────────────────────────

    #[test]
    fn to_title_case_capitalizes_first_letter_lowercase_rest() {
        assert_eq!(to_title_case("root"), "Root");
        assert_eq!(to_title_case("home"), "Home");
        assert_eq!(to_title_case("usr"), "Usr");
    }

    #[test]
    fn to_title_case_handles_already_uppercase_input() {
        assert_eq!(to_title_case("ROOT"), "Root");
        assert_eq!(to_title_case("HOME"), "Home");
    }

    #[test]
    fn to_title_case_handles_mixed_case_input() {
        assert_eq!(to_title_case("rOoT"), "Root");
        assert_eq!(to_title_case("HoMe"), "Home");
    }

    #[test]
    fn to_title_case_returns_empty_string_for_empty_input() {
        assert_eq!(to_title_case(""), "");
    }

    #[test]
    fn to_title_case_handles_single_character() {
        assert_eq!(to_title_case("a"), "A");
        assert_eq!(to_title_case("Z"), "Z");
    }

    // ── Credential (recovery-install unlock) ─────────────────────────────

    #[test]
    fn keyfile_credentials_pass_key_file_and_never_use_stdin() {
        let c = Credential::Keyfile("/run/media/usb/crypthome.key".to_string());
        assert_eq!(
            c.key_file_args(),
            vec!["--key-file", "/run/media/usb/crypthome.key"]
        );
        assert!(c.stdin_secret().is_none());
    }

    #[test]
    fn passphrase_credentials_use_stdin_and_add_no_arguments() {
        let c = Credential::Passphrase("hunter2".to_string());
        assert!(c.key_file_args().is_empty());
        assert_eq!(c.stdin_secret(), Some("hunter2"));
    }

    /// This type reaches error paths and logs; a passphrase must not.
    #[test]
    fn debug_and_describe_redact_the_passphrase() {
        let c = Credential::Passphrase("hunter2".to_string());
        assert!(!format!("{:?}", c).contains("hunter2"));
        assert!(!c.describe().contains("hunter2"));

        let k = Credential::Keyfile("/tmp/home.key".to_string());
        assert!(format!("{:?}", k).contains("/tmp/home.key"));
        assert!(k.describe().contains("/tmp/home.key"));
    }

    #[test]
    fn a_missing_keyfile_is_rejected_before_cryptsetup_runs() {
        let cmd = CommandRunner::new(false);
        let c = Credential::Keyfile("/nonexistent/does-not-exist.key".to_string());
        let err = verify_luks_credential(&cmd, "/dev/null", &c)
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not exist"), "unexpected error: {}", err);
    }

    #[test]
    fn dry_run_verification_touches_nothing() {
        let cmd = CommandRunner::new(true);
        let c = Credential::Keyfile("/nonexistent/does-not-exist.key".to_string());
        assert!(verify_luks_credential(&cmd, "/dev/null", &c).is_ok());
    }

    /// Exercises the real cryptsetup credential check. Ignored by default
    /// because it needs cryptsetup and a prepared container; set one up with:
    ///
    /// ```sh
    /// truncate -s 32M /tmp/home.img
    /// head -c 512 /dev/urandom > /tmp/good.key
    /// head -c 512 /dev/urandom > /tmp/bad.key
    /// cryptsetup luksFormat --type luks2 --batch-mode \
    ///     --key-file /tmp/good.key /tmp/home.img
    /// cargo test --lib verifies_a_real_luks_container -- --ignored
    /// ```
    #[test]
    #[ignore = "needs cryptsetup and a prepared LUKS container at /tmp/home.img"]
    fn verifies_a_real_luks_container() {
        let cmd = CommandRunner::new(false);
        let good = Credential::Keyfile("/tmp/good.key".to_string());
        let bad = Credential::Keyfile("/tmp/bad.key".to_string());

        verify_luks_credential(&cmd, "/tmp/home.img", &good)
            .expect("the formatting keyfile must unlock the container");

        let err = verify_luks_credential(&cmd, "/tmp/home.img", &bad)
            .expect_err("an unrelated keyfile must be rejected")
            .to_string();
        assert!(err.contains("does not unlock"), "unexpected error: {}", err);
    }

    // ── AdoptSpec ────────────────────────────────────────────────────────

    #[test]
    fn adopt_matches_the_named_volume_case_insensitively() {
        let spec = AdoptSpec {
            volume_name: "Home".to_string(),
            credential: Credential::Keyfile("/tmp/home.key".to_string()),
        };
        assert!(spec.matches("Home"));
        assert!(spec.matches("home"));
        assert!(spec.matches("HOME"));
    }

    /// Matching the wrong volume would reformat a container that should have
    /// been adopted, or adopt one that should have been created.
    #[test]
    fn adopt_does_not_match_other_volumes() {
        let spec = AdoptSpec {
            volume_name: "Home".to_string(),
            credential: Credential::Keyfile("/tmp/home.key".to_string()),
        };
        for other in ["Root", "Usr", "Var", "Boot"] {
            assert!(!spec.matches(other), "must not match {}", other);
        }
    }
}
