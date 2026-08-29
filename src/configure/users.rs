//! User creation and management

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
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
        groups.join(", "),
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would create user {} with groups {:?}",
            username, groups,
        );
        return Ok(());
    }

    // Build groups string
    let groups_str = groups.join(",");

    let useradd_cmd = build_useradd_command(
        username,
        &groups_str,
        existing_home_ownership(config, install_root, username),
    );
    cmd.run_in_chroot(install_root, &useradd_cmd)?;

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

/// Owner of an existing home directory that a recovery install must adopt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeOwnership {
    pub uid: u32,
    pub gid: u32,
}

/// Read the owner of `/home/<user>` when this run is preserving an existing
/// home, so the new account can take the UID and GID that already own the
/// files.
///
/// Returns `None` for an ordinary install, or when the directory is not
/// there — in both cases the account is created normally.
fn existing_home_ownership(
    config: &DeploymentConfig,
    install_root: &str,
    username: &str,
) -> Option<HomeOwnership> {
    if !config.disk.recovery.reuse_home {
        return None;
    }

    let home = format!("{}/home/{}", install_root, username);
    let meta = fs::metadata(&home).ok()?;
    let ownership = HomeOwnership {
        uid: meta.uid(),
        gid: meta.gid(),
    };
    info!(
        "Recovery install: adopting uid {} / gid {} from the existing /home/{}",
        ownership.uid, ownership.gid, username
    );
    Some(ownership)
}

/// Build the `useradd` invocation for this account.
///
/// With an existing home, the account takes over the UID and GID that own
/// the files already there and `-M` keeps useradd from touching the
/// directory. Without that, the new account gets whatever UID happens to be
/// free — usually 1000, so it works by luck on a blank system and silently
/// leaves the user unable to read their own data when it does not.
fn build_useradd_command(
    username: &str,
    groups_str: &str,
    existing: Option<HomeOwnership>,
) -> String {
    match existing {
        Some(HomeOwnership { uid, gid }) => format!(
            "groupadd -g {gid} {username} 2>/dev/null || true; \
             useradd -M -u {uid} -g {gid} -G {groups} -s /bin/bash {username}",
            gid = gid,
            uid = uid,
            groups = groups_str,
            username = username,
        ),
        None => format!("useradd -m -G {} -s /bin/bash {}", groups_str, username),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_install_creates_the_home_directory() {
        let cmd = build_useradd_command("gamer", "wheel,video", None);
        assert_eq!(cmd, "useradd -m -G wheel,video -s /bin/bash gamer");
    }

    /// A recovery install must take the UID and GID that already own the
    /// preserved files, or the user cannot read their own data — and `-M`
    /// keeps useradd from touching the directory that is already there.
    #[test]
    fn a_preserved_home_hands_its_uid_and_gid_to_the_new_account() {
        let cmd = build_useradd_command(
            "gamer",
            "wheel,video",
            Some(HomeOwnership {
                uid: 1007,
                gid: 1007,
            }),
        );
        assert!(cmd.contains("groupadd -g 1007 gamer"));
        assert!(cmd.contains("useradd -M -u 1007 -g 1007 -G wheel,video -s /bin/bash gamer"));
        assert!(
            !cmd.contains("useradd -m"),
            "must not recreate the home: {}",
            cmd
        );
    }

    /// An unusual UID from the old install is carried over verbatim rather
    /// than normalised to 1000.
    #[test]
    fn a_non_default_uid_is_carried_over_verbatim() {
        let cmd =
            build_useradd_command("gamer", "wheel", Some(HomeOwnership { uid: 2345, gid: 60 }));
        assert!(cmd.contains("-u 2345 -g 60"), "unexpected command: {}", cmd);
    }
}
