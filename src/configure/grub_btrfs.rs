//! grub-btrfs integration: snapshot boot menu entries, snapper root config,
//! `/etc/default/grub-btrfs/config` generation, and grub-btrfsd daemon
//! service files for all four init systems.
//!
//! Runs as installer Phase 5.45 — after the bootloader phase, so the
//! `91-patch-grub-btrfs.hook` compat patch (encrypted layouts) fires during
//! the pacman transaction that installs grub-btrfs, and before finalize's
//! `mkinitcpio -P`, which needs the package's `grub-btrfs-overlayfs` hook
//! present when it appears in HOOKS.
//!
//! Rollback semantics are deliberately partial: only `/` (the `@` subvolume)
//! is snapshotted; `@usr`, `@home`, `@var` and `@log` stay live. Booting a
//! snapshot shows the snapshot's root with the live everything-else.

use crate::config::{DeploymentConfig, InitSystem};
use crate::configure::bootloader::uses_standalone_grub;
use crate::configure::packages::pacman_install_chroot;
use crate::disk::formatting::get_partition_uuid;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tracing::{info, warn};

/// Packages required for snapshot boot menu entries. All are official-repo
/// packages — no AUR/yay required. inotify-tools is grub-btrfsd's watch
/// dependency; snapper manages `/.snapshots` (it is also installable via
/// `install_btrfs_tools`, hence `--needed` for idempotence).
const GRUB_BTRFS_PACKAGES: &[&str] = &["grub-btrfs", "inotify-tools", "snapper"];

/// Install and configure grub-btrfs on the target system.
///
/// `root_fs_device` is the block device carrying the root btrfs filesystem:
/// the Root LUKS mapper path on encrypted layouts, the ROOT partition path
/// otherwise.
pub fn install_grub_btrfs(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    root_fs_device: &str,
    install_root: &str,
) -> Result<()> {
    if !config.packages.install_grub_btrfs {
        return Ok(());
    }

    install_packages(cmd, install_root)?;
    configure_snapper_root(cmd, root_fs_device, install_root)?;
    write_grub_btrfs_config(cmd, config, install_root)?;
    write_grub_btrfsd_service(cmd, config, install_root)?;

    info!("grub-btrfs installation complete");
    Ok(())
}

/// Install grub-btrfs and its runtime dependencies via pacman in chroot.
fn install_packages(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    info!(
        "Installing grub-btrfs packages: {}",
        GRUB_BTRFS_PACKAGES.join(", ")
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install via pacman: {:?}",
            GRUB_BTRFS_PACKAGES
        );
        return Ok(());
    }

    let pacman_cmd = format!(
        "pacman -S --noconfirm --needed {}",
        GRUB_BTRFS_PACKAGES.join(" ")
    );
    pacman_install_chroot(cmd, install_root, &pacman_cmd)
}

/// Create the snapper config for `/` with a top-level `@snapshots` subvolume
/// mounted at `/.snapshots` (the standard snapper + grub-btrfs layout).
///
/// `snapper create-config` makes a *nested* `.snapshots` subvolume under `@`;
/// nested snapshots die with `@` on rollback and get carried into snapshots
/// of `@`, so it is replaced with a top-level sibling of `@`.
fn configure_snapper_root(
    cmd: &CommandRunner,
    root_fs_device: &str,
    install_root: &str,
) -> Result<()> {
    info!("Configuring snapper root config with top-level @snapshots subvolume");

    if cmd.is_dry_run() {
        println!("  [dry-run] snapper --no-dbus -c root create-config /");
        println!(
            "  [dry-run] Would replace nested .snapshots with top-level @snapshots on {}",
            root_fs_device
        );
        println!(
            "  [dry-run] Would append /.snapshots (subvol=@snapshots) to /etc/fstab and mount it"
        );
        return Ok(());
    }

    // 1. Snapper config for the root subvolume (also registers it in
    //    /etc/conf.d/snapper's SNAPPER_CONFIGS).
    cmd.run_in_chroot(install_root, "snapper --no-dbus -c root create-config /")?;

    // 2. Drop the nested .snapshots subvolume snapper just created and
    //    replace it with a top-level @snapshots (subvolid=5 = fs root).
    cmd.run_in_chroot(install_root, "btrfs subvolume delete /.snapshots")?;
    cmd.run_in_chroot(
        install_root,
        &format!(
            "mkdir -p /mnt && mount -t btrfs -o subvolid=5 {dev} /mnt && \
             btrfs subvolume create /mnt/@snapshots; ret=$?; umount /mnt; exit $ret",
            dev = root_fs_device
        ),
    )?;

    // 3. Mount point for the top-level subvolume (0750 per snapper convention:
    //    snapshot metadata is root-only).
    cmd.run_in_chroot(
        install_root,
        "mkdir -p /.snapshots && chmod 750 /.snapshots",
    )?;

    // 4. fstab entry, matching the format of the generated subvolume entries.
    let root_uuid = get_partition_uuid(root_fs_device)?;
    let fstab_path = format!("{}/etc/fstab", install_root);
    let mut fstab = fs::read_to_string(&fstab_path)?;
    if !fstab.contains("  /.snapshots  ") {
        if !fstab.ends_with('\n') {
            fstab.push('\n');
        }
        fstab.push_str(&format!(
            "UUID={}  /.snapshots  btrfs  subvol=@snapshots,defaults,noatime,compress=zstd  0  0\n",
            root_uuid
        ));
        fs::write(&fstab_path, fstab)?;
        info!("Appended /.snapshots entry to /etc/fstab");
    }

    // 5. Mount it now so grub-btrfsd's watch path and any install-time
    //    snapper calls work; finalize's unmount_all takes it down with the
    //    rest of the tree.
    cmd.run_in_chroot(install_root, "mount /.snapshots")?;

    Ok(())
}

/// Write `/etc/default/grub-btrfs/config`, overwriting the package default.
///
/// On standalone-GRUB systems (SecureBoot sbctl + encryption) the on-disk
/// grub.cfg is embedded in the signed EFI binary, so snapshot changes must
/// trigger the full rebuild + re-sign pipeline: `GRUB_BTRFS_MKCONFIG` points
/// at the existing `/usr/local/bin/reinstall-grub` script instead of plain
/// `grub-mkconfig`.
fn write_grub_btrfs_config(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let standalone = uses_standalone_grub(config);
    let mkconfig = if standalone {
        "/usr/local/bin/reinstall-grub"
    } else {
        "/usr/bin/grub-mkconfig"
    };
    let cryptodisk = if config.disk.boot_encryption {
        "true"
    } else {
        "false"
    };

    info!(
        "Writing /etc/default/grub-btrfs/config (GRUB_BTRFS_MKCONFIG={})",
        mkconfig
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would write /etc/default/grub-btrfs/config \
             (GRUB_BTRFS_MKCONFIG={}, GRUB_BTRFS_ENABLE_CRYPTODISK={})",
            mkconfig, cryptodisk
        );
        return Ok(());
    }

    if standalone {
        let reinstall = format!("{}/usr/local/bin/reinstall-grub", install_root);
        if !std::path::Path::new(&reinstall).exists() {
            warn!(
                "reinstall-grub not found at {} — GRUB_BTRFS_MKCONFIG will reference a missing script",
                reinstall
            );
        }
    }

    // The DEPLOYTIX-CRYPTODISK-V1 marker makes patch-grub-btrfs-integrity's
    // ensure_cryptodisk_flag() treat the line as already managed, so the
    // dormant compat patch and this generated config compose instead of
    // rewriting each other.
    let content = format!(
        r#"#!/usr/bin/env bash
# Generated by Deploytix — grub-btrfs configuration.
# Replaces the package default; pacman preserves later upstream changes as
# /etc/default/grub-btrfs/config.pacnew.

GRUB_BTRFS_MKCONFIG_LIB=/usr/share/grub/grub-mkconfig_lib

# GRUB only needs cryptomount preludes in snapshot entries when /boot itself
# is a LUKS container; matched to the layout's boot_encryption at install.
GRUB_BTRFS_ENABLE_CRYPTODISK="{cryptodisk}" # DEPLOYTIX-CRYPTODISK-V1

# Cap the number of snapshot menu entries. Regenerations on standalone-GRUB
# systems rebuild and re-sign the EFI binary (seconds per run), so keep the
# per-event work bounded.
GRUB_BTRFS_LIMIT="10"

# Command grub-btrfsd runs when snapshots change. Standalone-GRUB systems use
# the reinstall-grub pipeline (grub-mkconfig -> grub-mkstandalone -> sbctl
# sign-all) so new entries reach the signed, embedded config.
GRUB_BTRFS_MKCONFIG={mkconfig}
"#,
        cryptodisk = cryptodisk,
        mkconfig = mkconfig,
    );

    let dir = format!("{}/etc/default/grub-btrfs", install_root);
    fs::create_dir_all(&dir)?;
    let path = format!("{}/config", dir);
    fs::write(&path, content)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;

    Ok(())
}

/// Write the grub-btrfsd service definition for the configured init system.
///
/// Upstream ships only systemd units. The daemon must run as root: it
/// watches `/.snapshots` (0750 root-only) and regenerates grub.cfg — on
/// standalone systems it additionally rebuilds and re-signs the EFI binary,
/// the same trust boundary the pacman reinstall hook operates in.
fn write_grub_btrfsd_service(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would write grub-btrfsd service definition for {}",
            config.system.init
        );
        return Ok(());
    }

    match config.system.init {
        InitSystem::Runit => {
            let sv_dir = format!("{}/etc/runit/sv/grub-btrfsd", install_root);
            fs::create_dir_all(&sv_dir)?;

            // No --syslog: stdout goes to svlogd via log/run.
            let run_script = "#!/bin/sh\n\
                              exec 2>&1\n\
                              exec /usr/bin/grub-btrfsd /.snapshots\n";
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            let log_dir = format!("{}/log", sv_dir);
            fs::create_dir_all(&log_dir)?;
            let log_run = "#!/bin/sh\n\
                           [ -d /var/log/grub-btrfsd ] || install -dm 755 /var/log/grub-btrfsd\n\
                           exec svlogd -tt /var/log/grub-btrfsd\n";
            let log_run_path = format!("{}/run", log_dir);
            fs::write(&log_run_path, log_run)?;
            fs::set_permissions(&log_run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written runit service: /etc/runit/sv/grub-btrfsd/");
        }

        InitSystem::OpenRC => {
            let init_d = format!("{}/etc/init.d", install_root);
            fs::create_dir_all(&init_d)?;

            // localmount: /.snapshots must be mounted before the daemon
            // starts watching it.
            let script = "#!/sbin/openrc-run\n\
                          description=\"grub-btrfs snapshot menu daemon\"\n\
                          command=\"/usr/bin/grub-btrfsd\"\n\
                          command_args=\"--syslog /.snapshots\"\n\
                          command_background=true\n\
                          pidfile=\"/var/run/grub-btrfsd.pid\"\n\
                          \n\
                          depend() {\n\
                          \tneed localmount\n\
                          }\n";
            let script_path = format!("{}/grub-btrfsd", init_d);
            fs::write(&script_path, script)?;
            fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written OpenRC service: /etc/init.d/grub-btrfsd");
        }

        InitSystem::S6 => {
            let sv_dir = format!("{}/etc/s6/adminsv/grub-btrfsd", install_root);
            fs::create_dir_all(&sv_dir)?;

            fs::write(format!("{}/type", sv_dir), "longrun\n")?;

            let run_script = "#!/bin/sh\n\
                              exec /usr/bin/grub-btrfsd /.snapshots 2>&1\n";
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written s6 service: /etc/s6/adminsv/grub-btrfsd/");
        }

        InitSystem::Dinit => {
            let dinit_d = format!("{}/etc/dinit.d", install_root);
            fs::create_dir_all(&dinit_d)?;

            let service = "type = process\n\
                           command = /usr/bin/grub-btrfsd --syslog /.snapshots\n\
                           restart = true\n";
            let service_path = format!("{}/grub-btrfsd", dinit_d);
            fs::write(&service_path, service)?;
            fs::set_permissions(&service_path, fs::Permissions::from_mode(0o644))?;

            info!("  Written dinit service: /etc/dinit.d/grub-btrfsd");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DeploymentConfig, Filesystem, InitSystem, SecureBootMethod};

    fn base_config() -> DeploymentConfig {
        let mut config = DeploymentConfig::sample();
        config.disk.filesystem = Filesystem::Btrfs;
        config.disk.boot_filesystem = Filesystem::Btrfs;
        config.disk.use_subvolumes = true;
        config.packages.install_grub_btrfs = true;
        config
    }

    fn temp_root(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("deploytix_grub_btrfs_test_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn read_config(install_root: &str) -> String {
        fs::read_to_string(format!("{}/etc/default/grub-btrfs/config", install_root)).unwrap()
    }

    #[test]
    fn dry_run_writes_nothing() {
        let cmd = CommandRunner::new(true);
        let config = base_config();
        let root = temp_root("dryrun");

        install_grub_btrfs(&cmd, &config, "/dev/mapper/Crypt-Root", &root).unwrap();
        assert!(
            !std::path::Path::new(&format!("{}/etc/default/grub-btrfs", root)).exists(),
            "dry-run must not write the grub-btrfs config"
        );
        assert!(
            !std::path::Path::new(&format!("{}/etc/runit", root)).exists(),
            "dry-run must not write service files"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn config_standard_grub_uses_plain_mkconfig() {
        let cmd = CommandRunner::new(false);
        let config = base_config();
        let root = temp_root("std_mkconfig");

        write_grub_btrfs_config(&cmd, &config, &root).unwrap();
        let content = read_config(&root);
        assert!(content.contains("GRUB_BTRFS_MKCONFIG=/usr/bin/grub-mkconfig"));
        assert!(!content.contains("GRUB_BTRFS_MKCONFIG=/usr/local/bin/reinstall-grub"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn config_standalone_grub_uses_reinstall_script() {
        let cmd = CommandRunner::new(false);
        let mut config = base_config();
        config.system.secureboot = true;
        config.system.secureboot_method = SecureBootMethod::Sbctl;
        config.disk.encryption = true;
        let root = temp_root("standalone_mkconfig");

        write_grub_btrfs_config(&cmd, &config, &root).unwrap();
        let content = read_config(&root);
        assert!(content.contains("GRUB_BTRFS_MKCONFIG=/usr/local/bin/reinstall-grub"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn config_cryptodisk_tracks_boot_encryption() {
        let cmd = CommandRunner::new(false);

        let mut config = base_config();
        config.disk.boot_encryption = true;
        let root = temp_root("cryptodisk_on");
        write_grub_btrfs_config(&cmd, &config, &root).unwrap();
        assert!(read_config(&root)
            .contains("GRUB_BTRFS_ENABLE_CRYPTODISK=\"true\" # DEPLOYTIX-CRYPTODISK-V1"));
        let _ = fs::remove_dir_all(&root);

        let config = base_config();
        let root = temp_root("cryptodisk_off");
        write_grub_btrfs_config(&cmd, &config, &root).unwrap();
        assert!(read_config(&root)
            .contains("GRUB_BTRFS_ENABLE_CRYPTODISK=\"false\" # DEPLOYTIX-CRYPTODISK-V1"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn service_runit_supervised_without_syslog() {
        let cmd = CommandRunner::new(false);
        let mut config = base_config();
        config.system.init = InitSystem::Runit;
        let root = temp_root("svc_runit");

        write_grub_btrfsd_service(&cmd, &config, &root).unwrap();
        let run = fs::read_to_string(format!("{}/etc/runit/sv/grub-btrfsd/run", root)).unwrap();
        assert!(run.contains("exec /usr/bin/grub-btrfsd /.snapshots"));
        assert!(!run.contains("--syslog"));
        let log_run =
            fs::read_to_string(format!("{}/etc/runit/sv/grub-btrfsd/log/run", root)).unwrap();
        assert!(log_run.contains("svlogd"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn service_openrc_depends_on_localmount() {
        let cmd = CommandRunner::new(false);
        let mut config = base_config();
        config.system.init = InitSystem::OpenRC;
        let root = temp_root("svc_openrc");

        write_grub_btrfsd_service(&cmd, &config, &root).unwrap();
        let script = fs::read_to_string(format!("{}/etc/init.d/grub-btrfsd", root)).unwrap();
        assert!(script.contains("command=\"/usr/bin/grub-btrfsd\""));
        assert!(script.contains("--syslog /.snapshots"));
        assert!(script.contains("need localmount"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn service_s6_longrun_in_adminsv() {
        let cmd = CommandRunner::new(false);
        let mut config = base_config();
        config.system.init = InitSystem::S6;
        let root = temp_root("svc_s6");

        write_grub_btrfsd_service(&cmd, &config, &root).unwrap();
        let type_file =
            fs::read_to_string(format!("{}/etc/s6/adminsv/grub-btrfsd/type", root)).unwrap();
        assert_eq!(type_file, "longrun\n");
        let run = fs::read_to_string(format!("{}/etc/s6/adminsv/grub-btrfsd/run", root)).unwrap();
        assert!(run.contains("exec /usr/bin/grub-btrfsd /.snapshots"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn service_dinit_process_with_restart() {
        let cmd = CommandRunner::new(false);
        let mut config = base_config();
        config.system.init = InitSystem::Dinit;
        let root = temp_root("svc_dinit");

        write_grub_btrfsd_service(&cmd, &config, &root).unwrap();
        let service = fs::read_to_string(format!("{}/etc/dinit.d/grub-btrfsd", root)).unwrap();
        assert!(service.contains("type = process"));
        assert!(service.contains("command = /usr/bin/grub-btrfsd --syslog /.snapshots"));
        assert!(service.contains("restart = true"));

        let _ = fs::remove_dir_all(&root);
    }
}
