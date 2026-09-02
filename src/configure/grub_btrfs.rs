//! grub-btrfs integration: snapshot boot menu entries, snapper root config,
//! `/etc/default/grub-btrfs/config` generation, grub-btrfsd daemon service
//! files for all four init systems, and the grub.cfg regeneration that wires
//! the snapshot submenu into the boot menu.
//!
//! Runs as installer Phase 5.45 — after the bootloader phase, so the
//! `91-patch-grub-btrfs.hook` compat patch (encrypted layouts) fires during
//! the pacman transaction that installs grub-btrfs, and before finalize's
//! `mkinitcpio -P`, which needs the package's `grub-btrfs-overlayfs` hook
//! present when it appears in HOOKS.
//!
//! ## How a snapshot reaches the GRUB menu
//! grub-btrfs is two pieces. `/etc/grub.d/41_snapshots-btrfs` is a
//! grub-mkconfig generator: it writes the actual per-snapshot entries to a
//! separate `grub-btrfs.cfg` and emits a small *stub* into grub.cfg — a
//! submenu that `configfile`s that list if it exists. `grub-btrfsd` watches
//! `/.snapshots` and, on every change, either re-runs the generator in place
//! (when grub.cfg already carries the stub) or runs a full `grub-mkconfig`.
//! Three things therefore have to hold for entries to show up:
//!
//! 1. grub.cfg must contain the stub, which only a `grub-mkconfig` run *after*
//!    the package is installed can add. The bootloader phase ran before that,
//!    so this phase regenerates grub.cfg again ([`regenerate_grub_cfg`]).
//! 2. The stub must look where `grub-btrfs.cfg` really is. On standalone-GRUB
//!    systems grub.cfg is embedded in the signed EFI binary, so upstream's
//!    default location (`${prefix}`, the memdisk) can never contain a file
//!    written at runtime; the list is kept on the EFI System Partition and the
//!    stub reaches it through a GRUB variable set by
//!    [`ESP_LOCATOR_REL`] (see [`write_esp_locator_script`]).
//! 3. The generator must run against a real btrfs root. On a transactional
//!    immutable install the live `/` is an overlayfs, which the generator
//!    refuses and `grub-probe` cannot canonicalize, so the service runs
//!    [`WATCHER_PATH`] instead of the stock daemon and regenerates through
//!    `deploytix regen-grub` (a scratch chroot of the active snapshot set).
//!
//! Rollback semantics are deliberately partial: only `/` (the `@` subvolume)
//! is snapshotted; `@usr`, `@home`, `@var` and `@log` stay live. Booting a
//! snapshot shows the snapshot's root with the live everything-else. (The
//! immutable model layers paired `{@, @usr, @etc}` sets on top of this; see
//! `crate::immutable`.)

use crate::config::{DeploymentConfig, InitSystem};
use crate::configure::bootloader::{uses_standalone_grub, REINSTALL_GRUB_PATH};
use crate::configure::packages::pacman_install_chroot_reviewed_status;
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

/// Snapshot directory grub-btrfsd (or the deploytix watcher) monitors.
const SNAPSHOTS_DIR: &str = "/.snapshots";

/// Where `grub-btrfs.cfg` (the generated snapshot entries) lives on
/// standalone-GRUB systems: the EFI System Partition, the one place GRUB can
/// always read without a `cryptomount` and that is not baked into the signed
/// binary. Sits next to `BOOTX64.EFI` (the `--removable` install path).
pub const ESP_GBTRFS_DIR: &str = "/boot/efi/EFI/BOOT";

/// GRUB variable that [`ESP_LOCATOR_REL`] points at the ESP so the submenu
/// stub can `configfile` the snapshot list from there.
pub const ESP_GRUB_VAR: &str = "deploytix_esp";

/// grub.d generator (relative to a root) that sets [`ESP_GRUB_VAR`]. Named to
/// sort after `40_custom` and before `41_snapshots-btrfs`, which consumes it.
pub const ESP_LOCATOR_REL: &str = "etc/grub.d/40_deploytix-esp";

/// grub-btrfsd replacement used on transactional immutable installs.
pub const WATCHER_PATH: &str = "/usr/local/bin/deploytix-grub-btrfsd";

/// Install and configure grub-btrfs on the target system.
///
/// `root_fs_device` is the block device carrying the root btrfs filesystem:
/// the Root LUKS mapper path on encrypted layouts, the ROOT partition path
/// otherwise.
///
/// Returns `false` when the interactive policy declined the package install,
/// in which case nothing was configured — the caller must undo the parts of
/// the system that assume grub-btrfs is present (notably the
/// `grub-btrfs-overlayfs` entry in `HOOKS`, which would make the final
/// `mkinitcpio -P` hard-fail on a missing hook).
pub fn install_grub_btrfs(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    root_fs_device: &str,
    install_root: &str,
) -> Result<bool> {
    if !config.packages.install_grub_btrfs {
        return Ok(false);
    }

    if !install_packages(cmd, install_root)? {
        warn!(
            "grub-btrfs package install was declined during interactive review — \
             skipping snapper config, grub-btrfs config and the grub-btrfsd service"
        );
        return Ok(false);
    }

    configure_snapper_root(cmd, root_fs_device, install_root)?;
    create_overlay_subvolume(cmd, root_fs_device, install_root)?;
    write_grub_btrfs_config(cmd, config, install_root)?;
    write_grub_btrfsd_service(cmd, config, install_root)?;
    regenerate_grub_cfg(cmd, config, install_root)?;

    info!("grub-btrfs installation complete");
    Ok(true)
}

/// Regenerate the target's grub.cfg now that grub-btrfs is installed and
/// configured.
///
/// The bootloader phase ran `grub-mkconfig` before the package existed, so
/// the config it produced has no `41_snapshots-btrfs` section: no snapshot
/// submenu stub for GRUB to show, and no `snapshots-btrfs` marker for
/// grub-btrfsd. Without the stub, the entries grub-btrfsd later writes to
/// `grub-btrfs.cfg` are never reached from the menu; whether the daemon then
/// repairs that itself depends on its version (4.13 always runs a full
/// `grub-mkconfig`, newer daemons only re-run the generator in place once the
/// marker exists). Regenerating here makes the first snapshot the daemon sees
/// land in a config that is already wired for it, on every version.
///
/// Standalone-GRUB systems embed grub.cfg in the signed EFI binary, so they go
/// through the reinstall-grub pipeline (mkconfig → mkstandalone → sign)
/// rather than a bare `grub-mkconfig`.
fn regenerate_grub_cfg(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let standalone = uses_standalone_grub(config);
    let reinstall_present = cmd.is_dry_run()
        || std::path::Path::new(&format!("{install_root}{REINSTALL_GRUB_PATH}")).exists();
    let command = if standalone && reinstall_present {
        REINSTALL_GRUB_PATH
    } else {
        if standalone {
            warn!(
                "{} not found — regenerating grub.cfg with plain grub-mkconfig; the \
                 embedded standalone config will not carry the snapshot submenu until \
                 reinstall-grub runs",
                REINSTALL_GRUB_PATH
            );
        }
        "grub-mkconfig -o /boot/grub/grub.cfg"
    };

    info!(
        "Regenerating grub.cfg with grub-btrfs installed (snapshot submenu stub): {}",
        command
    );
    cmd.run_in_chroot(install_root, command)?;
    Ok(())
}

/// Install grub-btrfs and its runtime dependencies via pacman in chroot.
///
/// Routed through the interactive review policy like every other optional
/// package collection, so `--interactive` runs get to inspect or edit the
/// transaction.  Returns `false` when the policy skipped the install.
fn install_packages(cmd: &CommandRunner, install_root: &str) -> Result<bool> {
    info!(
        "Installing grub-btrfs packages: {}",
        GRUB_BTRFS_PACKAGES.join(", ")
    );

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would install via pacman: {:?}",
            GRUB_BTRFS_PACKAGES
        );
        return Ok(true);
    }

    pacman_install_chroot_reviewed_status(
        cmd,
        install_root,
        "grub-btrfs (snapshot boot)",
        GRUB_BTRFS_PACKAGES.iter().map(|p| p.to_string()).collect(),
    )
}

/// Shell command that creates the top-level `@snapshots` subvolume on
/// `root_fs_device` (mounted by `subvolid=5`, the filesystem root) and gives
/// it the root-only mode snapper expects.
///
/// The `chmod` targets the subvolume rather than the `/.snapshots` mount
/// point: `btrfs subvolume create` yields 0755, and once mounted it is this
/// inode users reach, so restricting only the covered directory would leave
/// snapshot metadata world-traversable.
fn create_snapshots_subvolume_cmd(root_fs_device: &str) -> String {
    format!(
        "mkdir -p /mnt && mount -t btrfs -o subvolid=5 {dev} /mnt && \
         btrfs subvolume create /mnt/@snapshots && chmod 750 /mnt/@snapshots; \
         ret=$?; umount /mnt; exit $ret",
        dev = root_fs_device
    )
}

/// Shell command that creates the top-level `@overlay` subvolume on
/// `root_fs_device` (mounted by `subvolid=5`, the filesystem root).
///
/// This subvolume holds the overlayfs upperdir/workdir used when booting a
/// read-only snapshot (see the mountcrypt hook). Keeping the scratch on disk
/// instead of a tmpfs frees snapshot-boot writes (`/tmp`, `/etc`, `/root`,
/// builds) from the ~50%-of-RAM tmpfs ceiling; the hook wipes it each boot so
/// changes remain ephemeral. Idempotent: a pre-existing `@overlay` is fine.
fn create_overlay_subvolume_cmd(root_fs_device: &str) -> String {
    format!(
        "mkdir -p /mnt && mount -t btrfs -o subvolid=5 {dev} /mnt && \
         test -e /mnt/@overlay || btrfs subvolume create /mnt/@overlay; \
         ret=$?; umount /mnt; exit $ret",
        dev = root_fs_device
    )
}

/// Create the top-level `@overlay` subvolume for the snapshot-boot overlayfs
/// upperdir. Gated behind grub-btrfs like the rest of the snapshot machinery.
fn create_overlay_subvolume(
    cmd: &CommandRunner,
    root_fs_device: &str,
    install_root: &str,
) -> Result<()> {
    info!("Creating top-level @overlay subvolume for snapshot-boot overlay upperdir");

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would create top-level @overlay subvolume on {}",
            root_fs_device
        );
        return Ok(());
    }

    cmd.run_in_chroot(install_root, &create_overlay_subvolume_cmd(root_fs_device))?;
    Ok(())
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
            "  [dry-run] Would replace nested .snapshots with top-level @snapshots (mode 0750) on {}",
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
    //
    //    The mode is set on the subvolume here, not on the mount point below:
    //    `btrfs subvolume create` makes it 0755, and once it is mounted over
    //    /.snapshots it is the subvolume's own inode that users reach, so a
    //    chmod on the covered directory would leave snapshot metadata
    //    world-traversable.
    cmd.run_in_chroot(install_root, "btrfs subvolume delete /.snapshots")?;
    cmd.run_in_chroot(
        install_root,
        &create_snapshots_subvolume_cmd(root_fs_device),
    )?;

    // 3. Mount point for the top-level subvolume (0750 per snapper convention:
    //    snapshot metadata is root-only). Restricted too, so the placeholder
    //    is not permissive while nothing is mounted over it.
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
/// grub.cfg is embedded in the signed EFI binary, which has two consequences:
///
/// - a full regeneration must go through the rebuild + re-sign pipeline, so
///   `GRUB_BTRFS_MKCONFIG` points at `/usr/local/bin/reinstall-grub` instead
///   of plain `grub-mkconfig`;
/// - the submenu stub inside the embedded grub.cfg cannot use upstream's
///   default `${prefix}/grub-btrfs.cfg` — `${prefix}` is the memdisk, which
///   no runtime write can reach — so the snapshot list is written to the
///   EFI System Partition ([`ESP_GBTRFS_DIR`]) and located at boot through
///   the [`ESP_GRUB_VAR`] variable that [`write_esp_locator_script`] sets.
fn write_grub_btrfs_config(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let standalone = uses_standalone_grub(config);
    let mkconfig = if standalone {
        REINSTALL_GRUB_PATH
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
        if standalone {
            println!(
                "  [dry-run] Would keep grub-btrfs.cfg on the ESP ({}) and write /{}",
                ESP_GBTRFS_DIR, ESP_LOCATOR_REL
            );
        }
        return Ok(());
    }

    if standalone {
        let reinstall = format!("{}{}", install_root, REINSTALL_GRUB_PATH);
        if !std::path::Path::new(&reinstall).exists() {
            warn!(
                "reinstall-grub not found at {} — GRUB_BTRFS_MKCONFIG will reference a missing script",
                reinstall
            );
        }
    }

    // Standalone GRUB: the snapshot list lives on the ESP and the stub finds
    // it via $deploytix_esp. The `\$` survives the bash `source` of this
    // config as a literal `$`, which 41_snapshots-btrfs copies verbatim into
    // grub.cfg, where GRUB expands it.
    let standalone_block = if standalone {
        format!(
            r#"
# This system boots a standalone GRUB image: grub.cfg is embedded in the
# signed EFI binary's memdisk, so the upstream default location for the
# snapshot list (${{prefix}}, i.e. the memdisk) can never contain a file
# written at runtime. The list is kept on the EFI System Partition instead —
# unencrypted, readable by GRUB with no cryptomount — and the submenu stub in
# grub.cfg reaches it through ${var}, set by /{locator}.
GRUB_BTRFS_GBTRFS_DIRNAME="{esp_dir}"
GRUB_BTRFS_GBTRFS_SEARCH_DIRNAME="(\${var})/EFI/BOOT"
"#,
            var = ESP_GRUB_VAR,
            locator = ESP_LOCATOR_REL,
            esp_dir = ESP_GBTRFS_DIR,
        )
    } else {
        String::new()
    };

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
{standalone_block}"#,
        cryptodisk = cryptodisk,
        mkconfig = mkconfig,
        standalone_block = standalone_block,
    );

    let dir = format!("{}/etc/default/grub-btrfs", install_root);
    fs::create_dir_all(&dir)?;
    let path = format!("{}/config", dir);
    fs::write(&path, content)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;

    if standalone {
        write_esp_locator_script(install_root)?;
    }

    Ok(())
}

/// The grub.d generator that publishes the ESP's location as a GRUB variable.
///
/// Emitted into grub.cfg ahead of the grub-btrfs stub (lexical order). The
/// UUID is probed at generation time rather than baked in at install, so a
/// reformatted ESP is picked up by the next regeneration.
pub fn esp_locator_script() -> String {
    format!(
        r#"#!/bin/sh
# Generated by Deploytix — locate the EFI System Partition for grub-btrfs.
#
# This system boots a standalone GRUB image (SecureBoot sbctl + encryption):
# grub.cfg lives in the signed EFI binary's memdisk, so ${{prefix}} can never
# hold a file written at runtime. grub-btrfs therefore writes its snapshot
# list to the ESP (GRUB_BTRFS_GBTRFS_DIRNAME in /etc/default/grub-btrfs/config)
# and the submenu stub reads it from (${var})/EFI/BOOT — this script sets
# that variable. The ESP is unencrypted, so no cryptomount is needed before
# the menu is shown.
#
# Runs before 41_snapshots-btrfs. Never fatal: without a UUID the variable
# points at the memdisk, so the stub's existence test fails cleanly and the
# submenu is simply absent.

esp_dir="{esp_mount}"
grub_probe="${{grub_probe:-grub-probe}}"

esp_uuid="$("${{grub_probe}}" --target=fs_uuid "${{esp_dir}}" 2>/dev/null || true)"
if [ -z "${{esp_uuid}}" ]; then
    esp_uuid="$(findmnt -no UUID "${{esp_dir}}" 2>/dev/null || true)"
fi

if [ -z "${{esp_uuid}}" ]; then
    echo "deploytix: cannot determine the ESP filesystem UUID; the snapshot submenu will be unavailable" >&2
    printf 'set {var}=memdisk\n'
    exit 0
fi

printf 'search --no-floppy --fs-uuid --set={var} %s\n' "${{esp_uuid}}"
"#,
        var = ESP_GRUB_VAR,
        esp_mount = "/boot/efi",
    )
}

/// Install [`esp_locator_script`] under `install_root` (executable, like
/// every grub.d generator — grub-mkconfig skips non-executable files).
fn write_esp_locator_script(install_root: &str) -> Result<()> {
    let path = format!("{}/{}", install_root, ESP_LOCATOR_REL);
    if let Some(parent) = std::path::Path::new(&path).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, esp_locator_script())?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    info!("  Written ESP locator generator: /{}", ESP_LOCATOR_REL);
    Ok(())
}

/// The grub-btrfsd replacement for transactional immutable installs.
///
/// Keeps the daemon's contract — watch the snapshot directory, regenerate on
/// create/delete — but hands regeneration to `deploytix regen-grub`, which
/// mounts the active snapshot set at a scratch chroot (a real btrfs root, so
/// `grub-probe` and `41_snapshots-btrfs` work) and runs `grub-mkconfig`
/// there. The stock daemon would run both against the live overlayfs `/`,
/// where the generator exits early and grub-probe fails outright.
pub fn immutable_watcher_script() -> String {
    format!(
        r#"#!/bin/bash
# deploytix-grub-btrfsd: grub-btrfsd replacement for the transactional
# immutable root.
# Generated by Deploytix
#
# On an immutable install the live / is an overlayfs stacked on the read-only
# @ (or snapshot-set) subvolume. grub-btrfs's generator
# (/etc/grub.d/41_snapshots-btrfs) exits as soon as it sees a non-btrfs /, and
# grub-mkconfig's grub-probe cannot canonicalize an overlay root at all — so
# the stock grub-btrfsd would either do nothing or rewrite grub.cfg from a
# root it cannot describe. This watcher keeps the daemon's contract (watch the
# snapshot directory, regenerate on create/delete) but hands the regeneration
# to `deploytix regen-grub`, which mounts the active snapshot set at a scratch
# chroot — a real btrfs root — and runs grub-mkconfig there.
#
# Usage: deploytix-grub-btrfsd [SNAPSHOT_DIR]   (default {snapdir})

set -u

snapdir="${{1:-{snapdir}}}"
# Coalesce the burst of files snapper writes per snapshot into one
# regeneration, like grub-btrfsd does.
settle=5
deploytix="$(command -v deploytix 2>/dev/null || echo /usr/bin/deploytix)"

log() {{ printf 'deploytix-grub-btrfsd: %s\n' "$*"; }}
err() {{ printf 'deploytix-grub-btrfsd: %s\n' "$*" >&2; }}

if ! command -v inotifywait >/dev/null 2>&1; then
    err "inotifywait not found (install inotify-tools); exiting"
    exit 1
fi
if [ ! -x "${{deploytix}}" ]; then
    err "deploytix not found at ${{deploytix}}; exiting"
    exit 1
fi

log "watching ${{snapdir}} (immutable root: regenerating via deploytix regen-grub)"
while true; do
    if [ ! -d "${{snapdir}}" ]; then
        err "${{snapdir}} does not exist; retrying in 30s"
        sleep 30
        continue
    fi
    # -e unmount: /.snapshots going away changes what GRUB should list too.
    if ! inotifywait -q -q -e create -e delete -e unmount "${{snapdir}}"; then
        sleep "${{settle}}"
        continue
    fi
    sleep "${{settle}}"
    log "snapshot change detected; regenerating grub.cfg"
    if "${{deploytix}}" regen-grub; then
        log "grub.cfg regenerated"
    else
        err "grub.cfg regeneration failed"
    fi
done
"#,
        snapdir = SNAPSHOTS_DIR,
    )
}

/// Install [`immutable_watcher_script`] at [`WATCHER_PATH`] under `install_root`.
fn write_immutable_watcher(install_root: &str) -> Result<()> {
    let path = format!("{}{}", install_root, WATCHER_PATH);
    if let Some(parent) = std::path::Path::new(&path).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, immutable_watcher_script())?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    info!("  Written immutable snapshot watcher: {}", WATCHER_PATH);
    Ok(())
}

/// The daemon a grub-btrfsd service should run: the stock grub-btrfsd, or the
/// deploytix watcher on immutable btrfs installs (see
/// [`immutable_watcher_script`]). Returns `(program, extra_args)`; the extra
/// args are the stock daemon's `--syslog`, which the watcher does not take.
fn daemon_command(config: &DeploymentConfig, syslog: bool) -> (String, String) {
    if config.immutable_btrfs() {
        (WATCHER_PATH.to_string(), String::new())
    } else {
        (
            "/usr/bin/grub-btrfsd".to_string(),
            if syslog {
                "--syslog ".to_string()
            } else {
                String::new()
            },
        )
    }
}

/// Write the grub-btrfsd service definition for the configured init system.
///
/// Upstream ships only systemd units. The daemon must run as root: it
/// watches `/.snapshots` (0750 root-only) and regenerates grub.cfg — on
/// standalone systems it additionally rebuilds and re-signs the EFI binary,
/// the same trust boundary the pacman reinstall hook operates in.
///
/// Transactional immutable installs run the deploytix watcher instead of the
/// stock daemon, because the live `/` there is an overlayfs the grub-btrfs
/// generator cannot work from.
fn write_grub_btrfsd_service(
    cmd: &CommandRunner,
    config: &DeploymentConfig,
    install_root: &str,
) -> Result<()> {
    let immutable = config.immutable_btrfs();

    if cmd.is_dry_run() {
        println!(
            "  [dry-run] Would write grub-btrfsd service definition for {} ({})",
            config.system.init,
            if immutable {
                "deploytix immutable watcher"
            } else {
                "stock grub-btrfsd"
            }
        );
        return Ok(());
    }

    if immutable {
        write_immutable_watcher(install_root)?;
    }

    match config.system.init {
        InitSystem::Runit => {
            let sv_dir = format!("{}/etc/runit/sv/grub-btrfsd", install_root);
            fs::create_dir_all(&sv_dir)?;

            // No --syslog: stdout goes to svlogd via log/run.
            let (program, _) = daemon_command(config, false);
            let run_script = format!(
                "#!/bin/sh\n\
                 exec 2>&1\n\
                 exec {program} {SNAPSHOTS_DIR}\n"
            );
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
            let (program, syslog) = daemon_command(config, true);
            let script = format!(
                "#!/sbin/openrc-run\n\
                 description=\"grub-btrfs snapshot menu daemon\"\n\
                 command=\"{program}\"\n\
                 command_args=\"{syslog}{SNAPSHOTS_DIR}\"\n\
                 command_background=true\n\
                 pidfile=\"/var/run/grub-btrfsd.pid\"\n\
                 \n\
                 depend() {{\n\
                 \tneed localmount\n\
                 }}\n"
            );
            let script_path = format!("{}/grub-btrfsd", init_d);
            fs::write(&script_path, script)?;
            fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written OpenRC service: /etc/init.d/grub-btrfsd");
        }

        InitSystem::S6 => {
            let sv_dir = format!("{}/etc/s6/adminsv/grub-btrfsd", install_root);
            fs::create_dir_all(&sv_dir)?;

            fs::write(format!("{}/type", sv_dir), "longrun\n")?;

            let (program, _) = daemon_command(config, false);
            let run_script = format!(
                "#!/bin/sh\n\
                 exec {program} {SNAPSHOTS_DIR} 2>&1\n"
            );
            let run_path = format!("{}/run", sv_dir);
            fs::write(&run_path, run_script)?;
            fs::set_permissions(&run_path, fs::Permissions::from_mode(0o755))?;

            info!("  Written s6 service: /etc/s6/adminsv/grub-btrfsd/");
        }

        InitSystem::Dinit => {
            let dinit_d = format!("{}/etc/dinit.d", install_root);
            fs::create_dir_all(&dinit_d)?;

            let (program, syslog) = daemon_command(config, true);
            let service = format!(
                "type = process\n\
                 command = {program} {syslog}{SNAPSHOTS_DIR}\n\
                 restart = true\n"
            );
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

    /// Policy that declines every pacman invocation, mirroring an operator
    /// choosing "skip" at the interactive review prompt.
    struct DecliningPolicy;

    impl crate::utils::interactive::InteractivePolicy for DecliningPolicy {
        fn confirm_pacman(
            &self,
            _inv: &crate::utils::interactive::PacmanInvocation,
        ) -> crate::utils::interactive::PacmanDecision {
            crate::utils::interactive::PacmanDecision::Skip
        }

        fn prompt_extras(
            &self,
            _can_use_yay: bool,
        ) -> (crate::utils::interactive::ExtraPackages, bool) {
            (Default::default(), false)
        }
    }

    #[test]
    fn declined_install_reports_skip_and_configures_nothing() {
        // Not dry-run: the review policy must actually be consulted. A Skip
        // has to short-circuit before any chroot command would run — if it
        // did not, this test would try to exec snapper on the host.
        let cmd = CommandRunner::new(false).with_policy(
            std::sync::Arc::new(DecliningPolicy) as crate::utils::interactive::PolicyHandle
        );
        let config = base_config();
        let root = temp_root("declined");

        let installed = install_grub_btrfs(&cmd, &config, "/dev/mapper/Crypt-Root", &root).unwrap();

        assert!(!installed, "a declined transaction must report skipped");
        assert!(
            !std::path::Path::new(&format!("{}/etc/default/grub-btrfs/config", root)).exists(),
            "declined install must not write the grub-btrfs config"
        );
        assert!(
            !std::path::Path::new(&format!("{}/etc/runit", root)).exists(),
            "declined install must not write service files"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn snapshots_subvolume_mode_set_on_subvolume_not_mountpoint() {
        let cmd = create_snapshots_subvolume_cmd("/dev/mapper/Crypt-Root");

        // The mount covers /.snapshots, so the mode that actually governs
        // access must land on the subvolume inode.
        let create = cmd.find("btrfs subvolume create /mnt/@snapshots").unwrap();
        let chmod = cmd.find("chmod 750 /mnt/@snapshots").unwrap();
        assert!(create < chmod, "chmod must follow the subvolume creation");

        // Chained with && so a failed create cannot leave an unrestricted
        // subvolume behind, and /mnt is always unmounted afterwards.
        assert!(cmd.contains("create /mnt/@snapshots && chmod 750 /mnt/@snapshots"));
        assert!(cmd.contains("umount /mnt"));
        assert!(cmd.contains("mount -t btrfs -o subvolid=5 /dev/mapper/Crypt-Root /mnt"));

        let status = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(&cmd)
            .status();
        if let Ok(status) = status {
            assert!(status.success(), "generated command is not valid shell");
        }
    }

    #[test]
    fn overlay_subvolume_created_idempotently_at_fs_root() {
        let cmd = create_overlay_subvolume_cmd("/dev/mapper/Crypt-Root");

        // Created at the filesystem root (subvolid=5), like @snapshots, so it
        // is a top-level sibling of @ rather than nested under a snapshot.
        assert!(cmd.contains("mount -t btrfs -o subvolid=5 /dev/mapper/Crypt-Root /mnt"));
        // Idempotent: a pre-existing @overlay must not error a re-run.
        assert!(cmd.contains("test -e /mnt/@overlay || btrfs subvolume create /mnt/@overlay"));
        assert!(cmd.contains("umount /mnt"));

        let status = std::process::Command::new("sh")
            .arg("-n")
            .arg("-c")
            .arg(&cmd)
            .status();
        if let Ok(status) = status {
            assert!(status.success(), "generated command is not valid shell");
        }
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

    fn standalone_config() -> DeploymentConfig {
        let mut config = base_config();
        config.system.secureboot = true;
        config.system.secureboot_method = SecureBootMethod::Sbctl;
        config.disk.encryption = true;
        assert!(uses_standalone_grub(&config));
        config
    }

    fn immutable_config() -> DeploymentConfig {
        let mut config = base_config();
        config.packages.immutable_root = true;
        assert!(config.immutable_btrfs());
        config
    }

    /// Syntax-check a generated script with `shell -n`; skipped when the
    /// shell is not installed.
    fn assert_valid_script(shell: &str, path: &str) {
        if let Ok(status) = std::process::Command::new(shell)
            .arg("-n")
            .arg(path)
            .status()
        {
            assert!(status.success(), "{path} is not valid {shell}");
        }
    }

    #[test]
    fn config_standalone_keeps_snapshot_list_on_esp() {
        let cmd = CommandRunner::new(false);
        let config = standalone_config();
        let root = temp_root("standalone_esp");

        write_grub_btrfs_config(&cmd, &config, &root).unwrap();
        let content = read_config(&root);
        assert!(content.contains("GRUB_BTRFS_GBTRFS_DIRNAME=\"/boot/efi/EFI/BOOT\""));
        assert!(
            content.contains("GRUB_BTRFS_GBTRFS_SEARCH_DIRNAME=\"(\\$deploytix_esp)/EFI/BOOT\"")
        );

        // 41_snapshots-btrfs sources this file with bash and copies the search
        // directory verbatim into grub.cfg, so after sourcing it must carry a
        // literal `$deploytix_esp` for GRUB (not bash) to expand.
        if let Ok(out) = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                ". {}/etc/default/grub-btrfs/config && printf %s \"$GRUB_BTRFS_GBTRFS_SEARCH_DIRNAME\"",
                root
            ))
            .output()
        {
            assert!(out.status.success(), "config does not source cleanly");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                "($deploytix_esp)/EFI/BOOT"
            );
        }

        // The generator that publishes the ESP location must be executable
        // (grub-mkconfig skips non-executable grub.d files) and valid sh.
        let locator = format!("{}/{}", root, ESP_LOCATOR_REL);
        let script = fs::read_to_string(&locator).unwrap();
        assert!(script.contains("search --no-floppy --fs-uuid --set=deploytix_esp"));
        assert!(script.contains("set deploytix_esp=memdisk"));
        let mode = fs::metadata(&locator).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "grub.d generators must be executable");
        assert_valid_script("sh", &locator);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn config_standard_grub_has_no_esp_override() {
        let cmd = CommandRunner::new(false);
        let config = base_config();
        let root = temp_root("std_no_esp");

        write_grub_btrfs_config(&cmd, &config, &root).unwrap();
        let content = read_config(&root);
        // ${prefix} is the on-disk /boot/grub here, so upstream's default is right.
        assert!(!content.contains("GRUB_BTRFS_GBTRFS_DIRNAME"));
        assert!(!content.contains("GRUB_BTRFS_GBTRFS_SEARCH_DIRNAME"));
        assert!(!std::path::Path::new(&format!("{}/{}", root, ESP_LOCATOR_REL)).exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn service_immutable_runs_deploytix_watcher() {
        let cmd = CommandRunner::new(false);
        let mut config = immutable_config();
        config.system.init = InitSystem::Runit;
        let root = temp_root("svc_immutable_runit");

        write_grub_btrfsd_service(&cmd, &config, &root).unwrap();
        let run = fs::read_to_string(format!("{}/etc/runit/sv/grub-btrfsd/run", root)).unwrap();
        assert!(run.contains("exec /usr/local/bin/deploytix-grub-btrfsd /.snapshots"));
        assert!(!run.contains("/usr/bin/grub-btrfsd"));

        let watcher = format!("{}{}", root, WATCHER_PATH);
        let script = fs::read_to_string(&watcher).unwrap();
        assert!(script.contains("inotifywait"));
        assert!(script.contains("regen-grub"));
        let mode = fs::metadata(&watcher).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "watcher must be executable");
        assert_valid_script("bash", &watcher);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn service_immutable_other_inits_drop_syslog_flag() {
        // The watcher logs to stdout/stderr only; the stock daemon's --syslog
        // must not be passed to it.
        let cmd = CommandRunner::new(false);
        let mut config = immutable_config();

        config.system.init = InitSystem::OpenRC;
        let root = temp_root("svc_immutable_openrc");
        write_grub_btrfsd_service(&cmd, &config, &root).unwrap();
        let script = fs::read_to_string(format!("{}/etc/init.d/grub-btrfsd", root)).unwrap();
        assert!(script.contains("command=\"/usr/local/bin/deploytix-grub-btrfsd\""));
        assert!(script.contains("command_args=\"/.snapshots\""));
        assert!(!script.contains("--syslog"));
        let _ = fs::remove_dir_all(&root);

        config.system.init = InitSystem::Dinit;
        let root = temp_root("svc_immutable_dinit");
        write_grub_btrfsd_service(&cmd, &config, &root).unwrap();
        let service = fs::read_to_string(format!("{}/etc/dinit.d/grub-btrfsd", root)).unwrap();
        assert!(service.contains("command = /usr/local/bin/deploytix-grub-btrfsd /.snapshots"));
        assert!(!service.contains("--syslog"));
        let _ = fs::remove_dir_all(&root);

        config.system.init = InitSystem::S6;
        let root = temp_root("svc_immutable_s6");
        write_grub_btrfsd_service(&cmd, &config, &root).unwrap();
        let run = fs::read_to_string(format!("{}/etc/s6/adminsv/grub-btrfsd/run", root)).unwrap();
        assert!(run.contains("exec /usr/local/bin/deploytix-grub-btrfsd /.snapshots"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn service_mutable_install_writes_no_watcher() {
        let cmd = CommandRunner::new(false);
        let mut config = base_config();
        config.system.init = InitSystem::S6;
        let root = temp_root("svc_mutable_no_watcher");

        write_grub_btrfsd_service(&cmd, &config, &root).unwrap();
        assert!(!std::path::Path::new(&format!("{}{}", root, WATCHER_PATH)).exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn dry_run_install_covers_standalone_and_immutable() {
        // Both variants add steps (ESP locator, watcher, reinstall-grub regen)
        // that must stay dry-run safe end to end.
        for (name, config) in [
            ("dryrun_standalone", standalone_config()),
            ("dryrun_immutable", immutable_config()),
        ] {
            let cmd = CommandRunner::new(true);
            let root = temp_root(name);
            assert!(install_grub_btrfs(&cmd, &config, "/dev/mapper/Crypt-Root", &root).unwrap());
            assert!(
                !std::path::Path::new(&format!("{}/etc", root)).exists(),
                "dry-run must not write under the install root"
            );
            let _ = fs::remove_dir_all(&root);
        }
    }
}
