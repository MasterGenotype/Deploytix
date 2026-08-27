//! User creation and management

use crate::config::{DeploymentConfig, SudoPolicy};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
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
        groups.join(", "),
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would create user {} with groups {:?}",
            username, groups,
        );
        return Ok(());
    }

    // argv, not a shell string: `groups` and `username` come from config and
    // must never be re-parsed by bash.
    let groups_str = groups.join(",");
    cmd.run_in_chroot_argv(
        install_root,
        &[
            "useradd",
            "-m",
            "-G",
            &groups_str,
            "-s",
            "/bin/bash",
            username,
        ],
    )?;

    // The password goes to chpasswd over a pipe.  It is not in argv (which
    // would expose it via /proc/<pid>/cmdline) and not in a temp file (the
    // previous approach created <root>/var/tmp/.deploytix_chpasswd at the
    // process umask and only chmod'd it to 0600 afterwards, leaving a
    // window where it was world-readable).
    set_password(cmd, install_root, username, password.as_str())?;

    // Configure sudoers if user should be sudoer
    if config.user.sudoer {
        configure_sudoers(cmd, install_root, config.system.sudo_policy)?;
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

/// Filename of the sudoers drop-in Deploytix owns.
///
/// `sudo` ignores files in `/etc/sudoers.d` whose names contain a dot or
/// end in `~`, so no extension.  The numeric prefix keeps the include
/// order explicit.
const SUDOERS_DROPIN: &str = "10-deploytix-wheel";

/// Body of the sudoers drop-in for `policy`.  Pure, so it can be asserted
/// on without a chroot to validate against.
fn sudoers_dropin_content(policy: SudoPolicy) -> String {
    format!(
        "# Managed by Deploytix. Regenerated on every install.\n\
         # Policy: {}\n\
         {}\n",
        policy,
        policy.sudoers_rule()
    )
}

/// Grant `%wheel` sudo via a drop-in under `/etc/sudoers.d`.
///
/// `/etc/sudoers` itself is never touched.  Deploytix <= 1.4.0 rewrote it
/// in place after a `read_to_string(..).unwrap_or_default()`, so an
/// unreadable or not-yet-installed `/etc/sudoers` produced a one-byte file
/// containing a newline — destroying the sudo config and still returning
/// `Ok(())`.  It also only matched two exact commented strings, so a
/// shipped sudoers differing by whitespace made the whole call a silent
/// no-op, and it hardcoded `NOPASSWD: ALL` for every wheel member.
///
/// The drop-in is written to a temporary path, checked with `visudo -cf`,
/// and only then moved into place, so a malformed rule can never become
/// active — a broken `/etc/sudoers.d` entry locks every user out of sudo.
fn configure_sudoers(cmd: &CommandRunner, install_root: &str, policy: SudoPolicy) -> Result<()> {
    info!("Configuring sudo for wheel group ({})", policy);

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would write /etc/sudoers.d/{} with: {}",
            SUDOERS_DROPIN,
            policy.sudoers_rule()
        );
        return Ok(());
    }

    let sudoers_d = format!("{}/etc/sudoers.d", install_root);
    fs::create_dir_all(&sudoers_d)?;
    fs::set_permissions(&sudoers_d, fs::Permissions::from_mode(0o750))?;

    let content = sudoers_dropin_content(policy);

    // Stage under a name sudo deliberately ignores (the dot), so that even
    // if validation fails and cleanup is interrupted, the half-written file
    // is inert rather than lockout-inducing.
    let staged_name = format!(".{}.new", SUDOERS_DROPIN);
    let staged_host = format!("{}/{}", sudoers_d, staged_name);
    let staged_chroot = format!("/etc/sudoers.d/{}", staged_name);
    let final_host = format!("{}/{}", sudoers_d, SUDOERS_DROPIN);

    fs::write(&staged_host, &content)?;
    fs::set_permissions(&staged_host, fs::Permissions::from_mode(0o440))?;

    let check = cmd.run_in_chroot_argv(install_root, &["visudo", "-cf", &staged_chroot]);
    if let Err(e) = check {
        let _ = fs::remove_file(&staged_host);
        return Err(DeploytixError::ConfigError(format!(
            "generated sudoers drop-in failed visudo validation, refusing to \
             install it (this would have locked wheel out of sudo): {}",
            e
        )));
    }

    fs::rename(&staged_host, &final_host)?;
    fs::set_permissions(&final_host, fs::Permissions::from_mode(0o440))?;

    info!("Wrote /etc/sudoers.d/{} ({})", SUDOERS_DROPIN, policy);
    Ok(())
}

/// Set `account`'s password by piping `user:password` to `chpasswd`.
///
/// The secret never appears in argv or on disk.
fn set_password(
    cmd: &CommandRunner,
    install_root: &str,
    account: &str,
    password: &str,
) -> Result<()> {
    cmd.run_in_chroot_argv_stdin(
        install_root,
        &["chpasswd"],
        &format!("{}:{}\n", account, password),
    )?;
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

    set_password(cmd, install_root, "root", password)
}

/// Lock root account (disable root login)
#[allow(dead_code)]
pub fn lock_root_account(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!("Locking root account");

    if cmd.is_dry_run() {
        println!("  [dry-run] Would lock root account");
        return Ok(());
    }

    cmd.run_in_chroot_argv(install_root, &["passwd", "-l", "root"])?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch install root that cleans itself up.
    struct TempRoot(String);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "deploytix-users-{}-{}-{:?}",
                tag,
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(path.join("etc")).unwrap();
            Self(path.to_string_lossy().into_owned())
        }

        fn path(&self, rel: &str) -> std::path::PathBuf {
            std::path::Path::new(&self.0).join(rel)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // ── Drop-in content ──────────────────────────────────────────────────

    #[test]
    fn password_policy_does_not_grant_nopasswd() {
        let content = sudoers_dropin_content(SudoPolicy::Password);
        assert!(content.contains("%wheel ALL=(ALL:ALL) ALL"));
        assert!(
            !content.contains("NOPASSWD"),
            "default policy must require a password: {}",
            content
        );
    }

    #[test]
    fn nopasswd_policy_is_available_when_asked_for_explicitly() {
        let content = sudoers_dropin_content(SudoPolicy::NoPasswd);
        assert!(content.contains("%wheel ALL=(ALL:ALL) NOPASSWD: ALL"));
    }

    #[test]
    fn dropin_content_is_a_complete_line_terminated_file() {
        for policy in [SudoPolicy::Password, SudoPolicy::NoPasswd] {
            let content = sudoers_dropin_content(policy);
            assert!(content.ends_with('\n'), "sudoers files must end in newline");
            // Every non-comment line must be the rule itself.
            let rules: Vec<_> = content
                .lines()
                .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
                .collect();
            assert_eq!(rules, vec![policy.sudoers_rule()]);
        }
    }

    #[test]
    fn dropin_filename_is_one_sudo_will_actually_read() {
        // sudo skips /etc/sudoers.d entries containing '.' or ending '~'.
        assert!(!SUDOERS_DROPIN.contains('.'));
        assert!(!SUDOERS_DROPIN.ends_with('~'));
    }

    // ── Regression: the /etc/sudoers truncation bug ──────────────────────
    //
    // Deploytix <= 1.4.0 did `read_to_string(sudoers).unwrap_or_default()`
    // then wrote the result back, so an unreadable or absent /etc/sudoers
    // silently became a one-byte file and the function still returned Ok.

    #[test]
    fn never_writes_to_etc_sudoers_even_when_validation_fails() {
        let root = TempRoot::new("nosudoerswrite");
        let sentinel = "# original sudoers\nroot ALL=(ALL:ALL) ALL\n";
        fs::write(root.path("etc/sudoers"), sentinel).unwrap();

        // Not a real chroot and visudo is not present inside it, so
        // validation cannot succeed — exactly the failure path we care
        // about.
        let cmd = CommandRunner::new(false);
        let result = configure_sudoers(&cmd, &root.0, SudoPolicy::Password);

        assert!(
            result.is_err(),
            "must refuse to install a drop-in it could not validate"
        );
        assert_eq!(
            fs::read_to_string(root.path("etc/sudoers")).unwrap(),
            sentinel,
            "/etc/sudoers must be left byte-for-byte untouched"
        );
    }

    #[test]
    fn missing_etc_sudoers_is_not_replaced_by_a_stub() {
        let root = TempRoot::new("nostub");
        // Deliberately no /etc/sudoers at all — the old code wrote "\n" here.
        let cmd = CommandRunner::new(false);
        let _ = configure_sudoers(&cmd, &root.0, SudoPolicy::Password);

        assert!(
            !root.path("etc/sudoers").exists(),
            "must not fabricate an /etc/sudoers"
        );
    }

    #[test]
    fn failed_validation_leaves_no_active_dropin_and_no_staged_leftovers() {
        let root = TempRoot::new("cleanup");
        let cmd = CommandRunner::new(false);
        let result = configure_sudoers(&cmd, &root.0, SudoPolicy::Password);
        assert!(result.is_err());

        let dropin = root.path(&format!("etc/sudoers.d/{}", SUDOERS_DROPIN));
        assert!(
            !dropin.exists(),
            "an unvalidated drop-in must never become active"
        );

        // The staged file is removed; and even if it survived, its name
        // starts with '.' so sudo would ignore it.
        let staged = root.path(&format!("etc/sudoers.d/.{}.new", SUDOERS_DROPIN));
        assert!(!staged.exists(), "staged file should be cleaned up");
    }

    #[test]
    fn dry_run_touches_nothing_at_all() {
        let root = TempRoot::new("dryrun");
        let sentinel = "# original sudoers\n";
        fs::write(root.path("etc/sudoers"), sentinel).unwrap();

        let cmd = CommandRunner::new(true);
        configure_sudoers(&cmd, &root.0, SudoPolicy::NoPasswd).unwrap();

        assert_eq!(
            fs::read_to_string(root.path("etc/sudoers")).unwrap(),
            sentinel
        );
        assert!(
            !root.path("etc/sudoers.d").exists(),
            "dry-run must not create /etc/sudoers.d"
        );
    }
}
