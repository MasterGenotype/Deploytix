//! gocryptfs encrypted home directory setup
//!
//! Configures gocryptfs + pam_mount for transparent auto-unlocking of
//! encrypted home directories on login. Encrypted data is stored in
//! `/home/user.cipher/` and mounted via FUSE at `/home/user/`.

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use tracing::info;

/// Set up gocryptfs encrypted home directory for the configured user.
///
/// Preconditions:
/// - User already created (useradd -M) with empty mount point at /home/user
/// - gocryptfs, pam_mount, fuse2 packages installed via basestrap
///
/// Steps:
/// 1. Enable user_allow_other in /etc/fuse.conf
/// 2. Create and initialize cipher directory
/// 3. Temporarily mount, populate with skel files, unmount
/// 4. Configure pam_mount.conf.xml
/// 5. Configure PAM (system-login)
pub fn setup_encrypted_home(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if !config.user.encrypt_home {
        return Ok(());
    }

    let username = &config.user.name;
    let password = &config.user.password;

    info!(
        "Setting up gocryptfs encrypted home for user '{}'",
        username
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would set up gocryptfs encrypted home for {}",
            username
        );
        println!(
            "  [dry-run] Would create /home/{}.cipher and initialize gocryptfs",
            username
        );
        println!("  [dry-run] Would configure pam_mount and PAM for auto-unlock");
        return Ok(());
    }

    configure_fuse(cmd, install_root)?;
    init_cipher_directory(cmd, username, password, install_root)?;
    populate_skel(cmd, username, password, install_root)?;
    configure_pam_mount(username, install_root)?;
    configure_pam(install_root)?;

    info!("gocryptfs encrypted home setup complete for '{}'", username);
    Ok(())
}

/// Enable `user_allow_other` in /etc/fuse.conf so pam_mount can use allow_other.
fn configure_fuse(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Enabling user_allow_other in /etc/fuse.conf");

    let fuse_conf_path = format!("{}/etc/fuse.conf", install_root);

    let content = fs::read_to_string(&fuse_conf_path).unwrap_or_default();

    // Check if already enabled
    if content.lines().any(|l| l.trim() == "user_allow_other") {
        info!("user_allow_other already enabled in fuse.conf");
        return Ok(());
    }

    // Uncomment if commented, otherwise append
    let new_content = if content.contains("#user_allow_other") {
        content.replace("#user_allow_other", "user_allow_other")
    } else {
        format!("{}\nuser_allow_other\n", content.trim_end())
    };

    fs::write(&fuse_conf_path, new_content)?;
    // Ensure correct permissions
    cmd.run("chmod", &["644", &fuse_conf_path])?;

    Ok(())
}

/// Create the cipher directory and initialize gocryptfs.
fn init_cipher_directory(
    cmd: &CommandRunner,
    username: &str,
    password: &str,
    install_root: &str,
) -> Result<()> {
    let cipher_dir = format!("{}/home/{}.cipher", install_root, username);

    info!("Creating cipher directory: {}", cipher_dir);
    fs::create_dir_all(&cipher_dir)?;

    // Initialize gocryptfs with the user's password
    // Use -extpass to provide password non-interactively
    let init_cmd = format!(
        "gocryptfs -init -q -extpass \"echo '{}'\" /home/{}.cipher",
        password, username
    );
    cmd.run_in_chroot(install_root, &init_cmd)?;

    // Set ownership of cipher directory and its contents to the user
    cmd.run_in_chroot(
        install_root,
        &format!(
            "chown -R {}:{} /home/{}.cipher",
            username, username, username
        ),
    )?;
    cmd.run_in_chroot(
        install_root,
        &format!("chmod 700 /home/{}.cipher", username),
    )?;

    info!("gocryptfs cipher directory initialized");
    Ok(())
}

/// Temporarily mount the encrypted dir, copy skel files, then unmount.
///
/// All operations are combined into a single chroot invocation so the
/// gocryptfs FUSE daemon stays alive throughout. Separate artix-chroot
/// calls would tear down /dev/fuse between invocations, leaving the
/// mount point in a "Transport endpoint is not connected" state.
fn populate_skel(
    cmd: &CommandRunner,
    username: &str,
    password: &str,
    install_root: &str,
) -> Result<()> {
    info!("Populating encrypted home with skeleton files");

    let combined_cmd = format!(
        "gocryptfs -extpass \"echo '{}'\" /home/{}.cipher /home/{} && \
         cp -a /etc/skel/. /home/{}/ && \
         chown -R {}:{} /home/{} && \
         fusermount -u /home/{}",
        password, username, username, username, username, username, username, username
    );
    cmd.run_in_chroot(install_root, &combined_cmd)?;

    info!("Skeleton files copied to encrypted home");
    Ok(())
}

/// Configure pam_mount.conf.xml with a volume entry for the user.
fn configure_pam_mount(username: &str, install_root: &str) -> Result<()> {
    info!("Configuring pam_mount.conf.xml for user '{}'", username);

    let conf_path = format!("{}/etc/security/pam_mount.conf.xml", install_root);

    let content = fs::read_to_string(&conf_path).unwrap_or_default();

    // Build the volume entry
    let volume_entry = format!(
        "<volume user=\"{}\" fstype=\"fuse\" options=\"nodev,nosuid,quiet,allow_other\" \
         path=\"/usr/bin/gocryptfs#/home/%(USER).cipher\" mountpoint=\"/home/%(USER)\" />",
        username
    );

    if content.contains(&volume_entry) {
        info!("pam_mount volume entry already present");
        return Ok(());
    }

    // Insert before the closing </pam_mount> tag
    let new_content = if content.contains("</pam_mount>") {
        content.replace("</pam_mount>", &format!("{}\n</pam_mount>", volume_entry))
    } else {
        // pam_mount.conf.xml doesn't exist or is malformed; create a minimal one
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n\
             <!DOCTYPE pam_mount SYSTEM \"pam_mount.conf.xml.dtd\">\n\
             <pam_mount>\n\
             {}\n\
             </pam_mount>\n",
            volume_entry
        )
    };

    // Ensure parent directory exists
    let conf_dir = format!("{}/etc/security", install_root);
    fs::create_dir_all(&conf_dir)?;

    fs::write(&conf_path, new_content)?;

    info!("pam_mount.conf.xml configured");
    Ok(())
}

/// Add pam_mount lines to /etc/pam.d/system-login for auto-mount on login.
fn configure_pam(install_root: &str) -> Result<()> {
    info!("Configuring PAM system-login for pam_mount");

    let pam_path = format!("{}/etc/pam.d/system-login", install_root);

    let content = fs::read_to_string(&pam_path).unwrap_or_default();

    let mut modified = false;
    let mut new_lines: Vec<String> = Vec::new();
    let mut auth_added = false;
    let mut session_added = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Insert `auth optional pam_mount.so` before `auth include system-auth`
        if !auth_added && trimmed == "auth      include   system-auth" {
            new_lines.push("auth      optional  pam_mount.so".to_string());
            auth_added = true;
            modified = true;
        }

        new_lines.push(line.to_string());

        // Insert `session optional pam_mount.so` after `session include system-auth`
        if !session_added && trimmed == "session   include   system-auth" {
            new_lines.push("session   optional  pam_mount.so".to_string());
            session_added = true;
            modified = true;
        }
    }

    // Fallback: if the expected lines weren't found, append at end
    if !auth_added {
        // Try a more relaxed match
        let mut fallback_lines: Vec<String> = Vec::new();
        for line in new_lines.iter() {
            let trimmed = line.trim();
            if !auth_added
                && trimmed.starts_with("auth")
                && trimmed.contains("include")
                && trimmed.contains("system-auth")
            {
                fallback_lines.push("auth      optional  pam_mount.so".to_string());
                auth_added = true;
                modified = true;
            }
            fallback_lines.push(line.clone());
        }
        if auth_added {
            new_lines = fallback_lines;
        }
    }

    if !session_added {
        let mut fallback_lines: Vec<String> = Vec::new();
        for line in new_lines.iter() {
            fallback_lines.push(line.clone());
            let trimmed = line.trim();
            if !session_added
                && trimmed.starts_with("session")
                && trimmed.contains("include")
                && trimmed.contains("system-auth")
            {
                fallback_lines.push("session   optional  pam_mount.so".to_string());
                session_added = true;
                modified = true;
            }
        }
        if session_added {
            new_lines = fallback_lines;
        }
    }

    // Last resort: just append if nothing matched
    if !auth_added {
        new_lines.push("auth      optional  pam_mount.so".to_string());
        modified = true;
    }
    if !session_added {
        new_lines.push("session   optional  pam_mount.so".to_string());
        modified = true;
    }

    if modified {
        // Check for existing pam_mount lines to avoid duplicates
        if content.contains("pam_mount.so") {
            info!("pam_mount already present in system-login, skipping");
            return Ok(());
        }

        let new_content = new_lines.join("\n") + "\n";
        fs::write(&pam_path, new_content)?;
        info!("PAM system-login updated with pam_mount entries");
    }

    Ok(())
}
