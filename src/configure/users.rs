//! User creation and management

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
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

    info!("Creating user: {}", username);

    if cmd.is_dry_run() {
        println!("  [dry-run] Would create user {} with groups {:?}", username, groups);
        return Ok(());
    }

    // Build groups string
    let groups_str = groups.join(",");

    // Create user with useradd
    let useradd_cmd = format!(
        "useradd -m -G {} -s /bin/bash {}",
        groups_str, username
    );
    cmd.run_in_chroot(install_root, &useradd_cmd)?;

    // Set password using chpasswd
    let chpasswd_cmd = format!("echo '{}:{}' | chpasswd", username, password);
    cmd.run_in_chroot(install_root, &chpasswd_cmd)?;

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
pub fn set_root_password(
    cmd: &CommandRunner,
    password: &str,
    install_root: &str,
) -> Result<()> {
    info!("Setting root password");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would set root password");
        return Ok(());
    }

    let chpasswd_cmd = format!("echo 'root:{}' | chpasswd", password);
    cmd.run_in_chroot(install_root, &chpasswd_cmd)?;

    Ok(())
}

/// Lock root account (disable root login)
pub fn lock_root_account(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Locking root account");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would lock root account");
        return Ok(());
    }

    cmd.run_in_chroot(install_root, "passwd -l root")?;

    Ok(())
}
