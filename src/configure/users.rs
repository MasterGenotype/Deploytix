//! User creation and management

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::info;

/// Create user account
pub fn create_user(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let username = &config.user.name;
    let password = &config.user.password;
    let groups = &config.user.groups;

    info!(
        "Creating user '{}' with groups [{}]",
        username,
        groups.join(", ")
    );

    let encrypt_home = config.user.encrypt_home;

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would create user {} with groups {:?} (encrypt_home={})",
            username, groups, encrypt_home
        );
        return Ok(());
    }

    // Build groups string
    let groups_str = groups.join(",");

    // Create user: skip home dir creation (-M) when gocryptfs will handle it
    let useradd_cmd = if encrypt_home {
        format!("useradd -M -G {} -s /bin/bash {}", groups_str, username)
    } else {
        format!("useradd -m -G {} -s /bin/bash {}", groups_str, username)
    };
    cmd.run_in_chroot(install_root, &useradd_cmd)?;

    // For encrypted home: create the mount point directory
    if encrypt_home {
        let home_dir = format!("{}/home/{}", install_root, username);
        fs::create_dir_all(&home_dir)?;
        // Set ownership and permissions via chroot
        cmd.run_in_chroot(
            install_root,
            &format!("chown {}:{} /home/{}", username, username, username),
        )?;
        cmd.run_in_chroot(install_root, &format!("chmod 700 /home/{}", username))?;
    }

    // Set password using chpasswd, passing credentials via a temp file to
    // avoid shell injection when the password contains single quotes or
    // other shell metacharacters.
    let temp_path = format!("{}/tmp/.deploytix_chpasswd", install_root);
    fs::write(&temp_path, format!("{}:{}", username, password))?;
    let mut perms = fs::metadata(&temp_path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&temp_path, perms)?;
    let result = cmd.run_in_chroot(install_root, "chpasswd < /tmp/.deploytix_chpasswd");
    let _ = fs::remove_file(&temp_path);
    result?;

    // Configure sudoers if user should be sudoer
    if config.user.sudoer {
        configure_sudoers(cmd, install_root)?;
    }

    info!("User {} created successfully", username);
    Ok(())
}

/// Configure sudoers for wheel group
fn configure_sudoers(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Configuring sudoers for wheel group");

    let sudoers_path = format!("{}/etc/sudoers", install_root);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would enable wheel group in sudoers");
        return Ok(());
    }

    // Read current sudoers
    let content = fs::read_to_string(&sudoers_path).unwrap_or_default();

    // Enable wheel group NOPASSWD (matching original script behavior)
    // Uncomment: # %wheel ALL=(ALL:ALL) NOPASSWD: ALL
    let new_content = content
        .lines()
        .map(|line| {
            if line.contains("# %wheel ALL=(ALL:ALL) NOPASSWD: ALL") {
                "%wheel ALL=(ALL:ALL) NOPASSWD: ALL"
            } else if line.contains("# %wheel ALL=(ALL) NOPASSWD: ALL") {
                "%wheel ALL=(ALL) NOPASSWD: ALL"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    fs::write(&sudoers_path, new_content + "\n")?;

    Ok(())
}

/// Set root password
#[allow(dead_code)]
pub fn set_root_password(cmd: &CommandRunner, password: &str, install_root: &str) -> Result<()> {
    info!("Setting root password");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would set root password");
        return Ok(());
    }

    // Pass credentials via a temp file to avoid shell injection.
    let temp_path = format!("{}/tmp/.deploytix_chpasswd", install_root);
    fs::write(&temp_path, format!("root:{}", password))?;
    let mut perms = fs::metadata(&temp_path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&temp_path, perms)?;
    let result = cmd.run_in_chroot(install_root, "chpasswd < /tmp/.deploytix_chpasswd");
    let _ = fs::remove_file(&temp_path);
    result?;

    Ok(())
}

/// Lock root account (disable root login)
#[allow(dead_code)]
pub fn lock_root_account(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Locking root account");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would lock root account");
        return Ok(());
    }

    cmd.run_in_chroot(install_root, "passwd -l root")?;

    Ok(())
}
