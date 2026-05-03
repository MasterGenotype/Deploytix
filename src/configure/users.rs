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
    let preserve_home = config.disk.preserve_home;

    info!(
        "Creating user '{}' with groups [{}] (preserve_home: {})",
        username,
        groups.join(", "),
        preserve_home,
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would create user {} with groups {:?} (preserve_home={})",
            username, groups, preserve_home,
        );
        return Ok(());
    }

    // Build groups string
    let groups_str = groups.join(",");

    let useradd_cmd = format!("useradd -m -G {} -s /bin/bash {}", groups_str, username);
    cmd.run_in_chroot(install_root, &useradd_cmd)?;

    // When preserve_home is enabled the preserved /home/<user> directory has
    // file ownership from the old system (potentially a different UID/GID).
    // Fix ownership so the newly created user can access their files.
    if preserve_home {
        let home_dir = format!("{}/home/{}", install_root, username);
        if std::path::Path::new(&home_dir).exists() {
            info!(
                "preserve_home: fixing ownership of /home/{} to match new UID/GID",
                username
            );
            cmd.run_in_chroot(
                install_root,
                &format!("chown -R {}:{} /home/{}", username, username, username),
            )?;
        }
    }

    // Set password using chpasswd, passing credentials via a temp file to
    // avoid shell injection when the password contains single quotes or
    // other shell metacharacters.
    let temp_path = format!("{}/var/tmp/.deploytix_chpasswd", install_root);
    fs::write(&temp_path, format!("{}:{}\n", username, password))?;
    let mut perms = fs::metadata(&temp_path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&temp_path, perms)?;
    let result = cmd.run_in_chroot(install_root, "chpasswd < /var/tmp/.deploytix_chpasswd");
    let _ = fs::remove_file(&temp_path);
    result?;

    // Configure sudoers if user should be sudoer
    if config.user.sudoer {
        configure_sudoers(cmd, install_root)?;
    }

    // Raise nofile ulimit so gamescope-session-plus can set ulimit -n 524288
    configure_ulimits(install_root)?;

    // Ensure ~/.local/bin is in PATH via .bashrc
    configure_bashrc_path(install_root, username)?;

    info!("User {} created successfully", username);
    Ok(())
}

/// Write /etc/security/limits.d drop-in to raise the nofile limit.
///
/// gamescope-session-plus calls `ulimit -n 524288`; PAM must allow this.
fn configure_ulimits(install_root: &str) -> Result<()> {
    let limits_dir = format!("{}/etc/security/limits.d", install_root);
    fs::create_dir_all(&limits_dir)?;

    let limits_path = format!("{}/99-deploytix-nofile.conf", limits_dir);
    info!("Writing nofile limits to {}", limits_path);
    fs::write(
        &limits_path,
        "# Deploytix: raise file descriptor limit for gamescope-session-plus\n\
         * soft nofile 524288\n\
         * hard nofile 524288\n",
    )?;

    Ok(())
}

/// Append `~/.local/bin` to PATH in the user's `.bashrc` if not already present.
fn configure_bashrc_path(install_root: &str, username: &str) -> Result<()> {
    let bashrc_path = format!("{}/home/{}/.bashrc", install_root, username);

    let existing = fs::read_to_string(&bashrc_path).unwrap_or_default();

    // Skip if the export is already present
    if existing.contains("$HOME/.local/bin") {
        info!("~/.local/bin PATH export already present in .bashrc");
        return Ok(());
    }

    let snippet = "\n# Add ~/.local/bin to PATH\n\
                    export PATH=\"$HOME/.local/bin${PATH:+:$PATH}\"\n";

    let mut content = existing;
    content.push_str(snippet);
    fs::write(&bashrc_path, content)?;

    info!(
        "Added ~/.local/bin PATH export to /home/{}/.bashrc",
        username
    );
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
    let temp_path = format!("{}/var/tmp/.deploytix_chpasswd", install_root);
    fs::write(&temp_path, format!("root:{}\n", password))?;
    let mut perms = fs::metadata(&temp_path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(&temp_path, perms)?;
    let result = cmd.run_in_chroot(install_root, "chpasswd < /var/tmp/.deploytix_chpasswd");
    let _ = fs::remove_file(&temp_path);
    result?;

    Ok(())
}

/// Configure autologin and session-manager exec for S6 init + session switching.
///
/// S6 has no greetd (no `greetd-s6` package in Artix repos), so the normal
/// greetd IPC path that creates a proper Class=user seat session is
/// unavailable.  This function provides an equivalent by:
///
/// 1. **agetty autologin** — patches `/etc/s6/sv/agetty-tty1/run` to add
///    `--autologin <user>` so the user is logged in automatically on tty1
///    without a password prompt.  The patch is idempotent: if `--autologin`
///    is already present, the file is not rewritten.  If the s6 agetty
///    service directory does not exist yet (the package may not be installed
///    until first boot), the modification is skipped with a warning.
///
/// 2. **~/.bash_profile exec** — appends a snippet that runs
///    `deploytix-session-manager` when the session is a login shell on tty1
///    and no graphical session is already active.  The append is idempotent:
///    if the sentinel comment is already present, nothing is written.
pub fn configure_s6_session_autologin(
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let username = &config.user.name;
    info!(
        "Configuring S6 autologin + session-manager exec for user '{}'",
        username
    );

    // 1. Patch the s6 agetty-tty1 run script for autologin.
    let agetty_run = format!("{}/etc/s6/sv/agetty-tty1/run", install_root);
    if std::path::Path::new(&agetty_run).exists() {
        let content = fs::read_to_string(&agetty_run)?;
        if content.contains("--autologin") {
            info!("  agetty-tty1 run script already has --autologin, skipping");
        } else {
            // Insert --autologin <user> before the TTY/linux arguments.
            // Typical lines end with: `agetty … tty1 linux` or `agetty … "$TTY" linux`
            let patched = content.replace(
                "exec agetty",
                &format!("exec agetty --autologin {}", username),
            );
            fs::write(&agetty_run, &patched)?;
            info!(
                "  Patched agetty-tty1 run script with --autologin {}",
                username
            );
        }
    } else {
        tracing::warn!(
            "  agetty-tty1 s6 service not found at {} — \
             autologin will not be configured automatically. \
             Install util-linux-s6 and add --autologin {} to \
             /etc/s6/sv/agetty-tty1/run manually.",
            agetty_run,
            username
        );
    }

    // 2. Write ~/.bash_profile exec snippet.
    let profile_path = format!("{}/home/{}/.bash_profile", install_root, username);
    let existing = fs::read_to_string(&profile_path).unwrap_or_default();

    // Sentinel keeps the append idempotent across re-runs.
    let sentinel = "# deploytix: S6 session-manager autostart";
    if existing.contains(sentinel) {
        info!("  ~/.bash_profile already contains session-manager autostart, skipping");
        return Ok(());
    }

    let snippet = format!(
        "\n{sentinel}\n\
         # Launch the gamescope/desktop session manager on tty1.\n\
         # Skipped when a graphical session is already active (SSH, nested).\n\
         if [[ -z \"${{DISPLAY:-}}${{WAYLAND_DISPLAY:-}}\" ]] && [[ \"$(tty)\" = \"/dev/tty1\" ]]; then\n\
         \texec /usr/bin/deploytix-session-manager\n\
         fi\n",
        sentinel = sentinel,
    );

    let mut content = existing;
    content.push_str(&snippet);
    fs::write(&profile_path, content)?;

    info!(
        "  Added session-manager autostart to /home/{}/.bash_profile",
        username
    );

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
