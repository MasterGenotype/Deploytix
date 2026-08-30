//! LVM immutable **A/B dual-slot** transactional backend (dm-verity).
//!
//! Two root logical volumes — `root_a`/`root_b` — alternate. The active slot is
//! mounted read-only and dm-verity integrity-checked (its hash tree lives on the
//! sibling `hash_a`/`hash_b` LV). `deploytix update` builds the *inactive* slot
//! and flips the boot pointer; `deploytix rollback` flips back. The running slot
//! is never modified, so an interrupted or failed update is a no-op.
//!
//! ## Boot pointer
//! The active slot + each slot's verity root hash are recorded in a small state
//! file on the shared `/boot` ([`STATE_FILE`]). The default GRUB entry carries
//! `deploytix.slot=<X> deploytix.roothash=<hashX>` on its cmdline; activation is
//! just a sed-rewrite of those tokens in `/boot/grub/grub.cfg` (no
//! `grub-mkconfig`/`grub-probe`, which would choke on the dm-verity root). The
//! `verity-ab` initramfs hook reads them, opens the slot's verity device, and
//! mounts `/` read-only.
//!
//! ## Shared writable state
//! `/var`, `/home` and `/boot` are shared across slots. As with the btrfs
//! backend, the pacman DB lives on the shared `/var`, so a rollback restores the
//! slot's `/usr` files but not the package database. See `docs/IMMUTABLE_LVM_AB.md`.

use crate::config::Filesystem;
use crate::disk::lvm::{ab, lv_path};
use crate::immutable::update::{self, UpdateOptions};
use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use tracing::{info, warn};

/// Slot-pointer state file on the shared `/boot`.
pub const STATE_FILE: &str = "/boot/deploytix-slots.conf";
/// Authoritative boot config rewritten to point at the active slot.
pub const GRUB_CFG: &str = "/boot/grub/grub.cfg";
/// Default grub template, patched best-effort for consistency.
pub const GRUB_DEFAULT: &str = "/etc/default/grub";
/// Where the inactive slot is assembled for the chroot.
fn target_dir(slot: &str) -> String {
    format!("/run/deploytix-slot/{slot}")
}

/// Parsed slot-pointer state ([`STATE_FILE`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotState {
    /// Active slot letter (`"A"`/`"B"`).
    pub active: String,
    /// Volume group holding the slots.
    pub vg: String,
    /// Verity root hash for slot A (empty if unbuilt).
    pub roothash_a: String,
    /// Verity root hash for slot B (empty if unbuilt).
    pub roothash_b: String,
}

impl SlotState {
    /// Serialise to the `key=value` state-file format.
    pub fn to_conf(&self) -> String {
        format!(
            "# deploytix immutable A/B slot pointer\n\
             active={}\nvg={}\nroothash_a={}\nroothash_b={}\n",
            self.active, self.vg, self.roothash_a, self.roothash_b
        )
    }

    /// Parse the `key=value` state-file format.
    pub fn from_conf(text: &str) -> SlotState {
        let mut active = "A".to_string();
        let mut vg = String::new();
        let mut roothash_a = String::new();
        let mut roothash_b = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let v = v.trim().to_string();
                match k.trim() {
                    "active" => active = v,
                    "vg" => vg = v,
                    "roothash_a" => roothash_a = v,
                    "roothash_b" => roothash_b = v,
                    _ => {}
                }
            }
        }
        SlotState {
            active,
            vg,
            roothash_a,
            roothash_b,
        }
    }

    /// The root hash recorded for `slot`.
    pub fn roothash(&self, slot: &str) -> &str {
        match slot {
            "A" | "a" => &self.roothash_a,
            _ => &self.roothash_b,
        }
    }

    /// Set the root hash for `slot`.
    pub fn set_roothash(&mut self, slot: &str, hash: &str) {
        match slot {
            "A" | "a" => self.roothash_a = hash.to_string(),
            _ => self.roothash_b = hash.to_string(),
        }
    }
}

/// The slot the running system actually booted from (`deploytix.slot=` on the
/// kernel cmdline, last occurrence wins — matching the `verity-ab` hook).
///
/// [`SlotState::active`] is the slot that will boot *next*: `run_update` sets it
/// to the slot it just built. The two therefore differ exactly while an update
/// is staged and not yet booted, and `fallback` (normally `state.active`) covers
/// a cmdline without the token.
pub fn running_slot(fallback: &str) -> String {
    std::fs::read_to_string("/proc/cmdline")
        .ok()
        .and_then(|c| parse_slot(&c))
        .unwrap_or_else(|| fallback.to_string())
}

/// Extract the effective `deploytix.slot=` value from a kernel cmdline.
fn parse_slot(cmdline: &str) -> Option<String> {
    let mut found = None;
    for arg in cmdline.split_whitespace() {
        if let Some(value) = arg.strip_prefix("deploytix.slot=") {
            let value = value.trim_matches('"');
            if !value.is_empty() {
                found = Some(value.to_uppercase());
            }
        }
    }
    found
}

/// Whether this system uses the LVM immutable A/B backend (used for dispatch).
pub fn detect() -> bool {
    std::path::Path::new(STATE_FILE).exists()
}

/// Read the slot state, erroring if this is not an LVM immutable system.
pub fn read_state() -> Result<SlotState> {
    let text = std::fs::read_to_string(STATE_FILE).map_err(|e| {
        DeploytixError::ConfigError(format!(
            "not an LVM immutable deploytix system (cannot read {STATE_FILE}: {e}); \
             `deploytix update`/`rollback` only apply to immutable installs"
        ))
    })?;
    Ok(SlotState::from_conf(&text))
}

/// Write the slot state to [`STATE_FILE`].
pub fn write_state(cmd: &CommandRunner, state: &SlotState) -> Result<()> {
    if cmd.is_dry_run() {
        println!("  [dry-run] Would write {STATE_FILE}:\n{}", state.to_conf());
        return Ok(());
    }
    std::fs::write(STATE_FILE, state.to_conf())?;
    Ok(())
}

/// `sed` that repoints every GRUB entry's `deploytix.slot=`/`deploytix.roothash=`
/// tokens at `slot`/`roothash` in `file`. A `|` delimiter avoids escaping.
fn set_pointer_sed(file: &str, slot: &str, roothash: &str) -> String {
    format!(
        "sed -i 's|deploytix.slot=[^ \"]*|deploytix.slot={slot}|g; \
         s|deploytix.roothash=[^ \"]*|deploytix.roothash={roothash}|g' {file}"
    )
}

/// Make `slot` (with its `roothash`) the default boot by rewriting the pointer
/// tokens in `/boot/grub/grub.cfg` (authoritative) and, best-effort, in
/// `/etc/default/grub`. No `grub-mkconfig` — the live root is a dm-verity device
/// that `grub-probe` cannot canonicalize.
pub fn activate_slot(cmd: &CommandRunner, slot: &str, roothash: &str) -> Result<()> {
    info!(
        "[lvm-ab] Repointing default boot to slot {} (roothash {})",
        slot,
        short_hash(roothash)
    );
    cmd.run("sh", &["-c", &set_pointer_sed(GRUB_CFG, slot, roothash)])?;
    // Best-effort: keep the default grub template in sync so a later
    // grub-mkconfig (e.g. a kernel install) preserves the active slot.
    let _ = cmd.run(
        "sh",
        &["-c", &set_pointer_sed(GRUB_DEFAULT, slot, roothash)],
    );
    Ok(())
}

/// Short prefix of a verity hash for logging.
fn short_hash(hash: &str) -> String {
    hash.chars().take(12).collect()
}

/// Shell that mounts the inactive slot's root LV read-write at its target and
/// rbinds the shared `/var`, `/home`, `/boot` so `artix-chroot` can run pacman
/// and mkinitcpio against it.
fn mount_target_cmd(vg: &str, root_lv: &str, slot: &str) -> String {
    let t = target_dir(slot);
    let dev = lv_path(vg, root_lv);
    format!(
        "set -e; t={t}; mkdir -p \"$t\"; mount {dev} \"$t\"; \
         for d in var home boot; do mkdir -p \"$t/$d\"; mount --rbind \"/$d\" \"$t/$d\"; done"
    )
}

/// Shell that rsyncs the running (active) root tree into the mounted inactive
/// slot, excluding pseudo-filesystems and the shared/separate mounts (which are
/// rbind-mounted for the chroot but must not be copied into the image).
fn rsync_root_cmd(slot: &str) -> String {
    let t = target_dir(slot);
    format!(
        "rsync -aHAX --delete \
         --exclude='/proc/*' --exclude='/sys/*' --exclude='/dev/*' \
         --exclude='/run/*' --exclude='/tmp/*' --exclude='/mnt/*' \
         --exclude='/media/*' --exclude='/lost+found' \
         --exclude='/var/*' --exclude='/home/*' --exclude='/boot/*' \
         --exclude='/run/deploytix-slot' \
         / \"{t}/\""
    )
}

/// Shell that recursively unmounts and removes the slot's chroot target.
fn unmount_target_cmd(slot: &str) -> String {
    let t = target_dir(slot);
    format!("umount -R {t} 2>/dev/null || true; rmdir {t} 2>/dev/null || true")
}

/// The slot an update writes into: `(slot, already_staged)`.
///
/// Normally the inactive slot — the one not running. But once an update is
/// staged, `active` names it while the session still runs the *other* slot, so
/// "the inactive slot" would be the running root: rebuilding it would rsync
/// over the live system and throw away the staged packages. Forgetting a
/// package and updating again therefore extends the staged slot instead, and it
/// stays the slot the system boots into.
fn update_target(active: &str, running: &str) -> (String, bool) {
    if active.eq_ignore_ascii_case(running) {
        (ab::other_slot(active).to_string(), false)
    } else {
        (active.to_string(), true)
    }
}

/// Perform a transactional A/B update: build the inactive slot, verity-seal it,
/// and repoint the boot pointer at it.
pub fn run_update(
    cmd: &CommandRunner,
    extra_packages: &[String],
    opts: &UpdateOptions,
) -> Result<()> {
    let state = read_state()?;
    let active = state.active.clone();
    let running = running_slot(&active);

    // Forgetting a package and updating again in the same session must extend
    // the slot already staged, not rebuild the other one — which, once an
    // update is staged, is the slot this session is running from. Rebuilding it
    // would rsync over the running root and drop the staged packages.
    let (target, staged) = update_target(&active, &running);

    let vg = state.vg.clone();
    let (root_lv, hash_lv) = ab::slot_lvs(&target)
        .ok_or_else(|| DeploytixError::ConfigError(format!("invalid target slot '{target}'")))?;

    if staged {
        info!(
            "[lvm-ab] Adding to slot {} ({}), already staged for the next boot",
            target, root_lv
        );
    } else {
        info!(
            "[lvm-ab] Building update into inactive slot {} ({}/{})",
            target, root_lv, hash_lv
        );
    }

    let (local_files, repo_names) = update::classify_args(extra_packages);

    // Everything here is unwound on failure so a bad update leaves the running
    // slot and boot pointer untouched.
    let result = (|| -> Result<()> {
        cmd.run("sh", &["-c", &mount_target_cmd(&vg, root_lv, &target)])?;
        let t = target_dir(&target);
        // Only a fresh target needs the running root copied in; a staged slot
        // already holds it, plus whatever earlier updates installed.
        if !staged {
            info!("[lvm-ab] Syncing active root -> slot {}", target);
            cmd.run("sh", &["-c", &rsync_root_cmd(&target)])?;
        }

        let staged = update::stage_local_pkgs(cmd, &local_files)?;
        info!("[lvm-ab] Running pacman in slot {}", target);
        for pac in update::pacman_cmds(&staged, &repo_names) {
            cmd.run_in_chroot(&t, &pac)?;
        }
        cmd.run_in_chroot(&t, "mkinitcpio -P")?;
        Ok(())
    })();

    // Always release the chroot mounts and clear the package staging dir.
    let _ = cmd.run("sh", &["-c", &unmount_target_cmd(&target)]);
    if !cmd.is_dry_run() {
        let _ = std::fs::remove_dir_all(update::PKG_STAGE_DIR);
    }

    if let Err(e) = result {
        if staged {
            warn!(
                "[lvm-ab] Update failed; slot {} keeps what earlier updates staged \
                 and stays selected for the next boot",
                target
            );
        } else {
            warn!(
                "[lvm-ab] Update failed; slot {} left inactive, boot pointer unchanged",
                target
            );
        }
        return Err(e);
    }

    // Seal the freshly built slot with a new dm-verity tree and repoint boot.
    let data_dev = lv_path(&vg, root_lv);
    let hash_dev = lv_path(&vg, hash_lv);
    let roothash = crate::configure::verity::format_verity(cmd, &data_dev, &hash_dev)?;

    let mut new_state = state;
    new_state.set_roothash(&target, &roothash);
    new_state.active = target.clone();
    write_state(cmd, &new_state)?;
    activate_slot(cmd, &target, &roothash)?;

    info!(
        "[lvm-ab] Update ready. Reboot to activate slot {} (rollback: `deploytix rollback`).",
        target
    );
    if opts.reboot {
        cmd.run("reboot", &[])?;
    }
    Ok(())
}

/// Roll back to the other slot (its image + verity hash are intact).
///
/// `selection`: `None` or the other slot letter flips slots; an explicit letter
/// equal to the active slot is a no-op error.
pub fn run_rollback(cmd: &CommandRunner, selection: Option<&str>, reboot: bool) -> Result<()> {
    let state = read_state()?;
    let target = match selection {
        None => ab::other_slot(&state.active).to_string(),
        Some(s) => {
            let s = s.to_uppercase();
            if s != "A" && s != "B" {
                return Err(DeploytixError::ConfigError(format!(
                    "invalid slot '{s}' (expected A or B)"
                )));
            }
            s
        }
    };
    if target.eq_ignore_ascii_case(&state.active) {
        return Err(DeploytixError::ConfigError(format!(
            "slot {target} is already active; nothing to roll back to"
        )));
    }
    let roothash = state.roothash(&target);
    if roothash.is_empty() {
        return Err(DeploytixError::ConfigError(format!(
            "slot {target} has no built image (no recorded root hash)"
        )));
    }

    info!("[lvm-ab] Rolling back: default boot -> slot {}", target);
    let mut new_state = state.clone();
    new_state.active = target.clone();
    write_state(cmd, &new_state)?;
    activate_slot(cmd, &target, roothash)?;

    info!(
        "[lvm-ab] Rollback staged. Reboot to activate slot {}.",
        target
    );
    if reboot {
        cmd.run("reboot", &[])?;
    }
    Ok(())
}

/// Print the A/B slots, their verity hashes, and the active marker.
pub fn print_slots(cmd: &CommandRunner) -> Result<()> {
    let state = read_state()?;
    println!("Immutable A/B slots (VG: {}):", state.vg);
    for slot in ["A", "B"] {
        let active = if slot.eq_ignore_ascii_case(&state.active) {
            " *"
        } else {
            "  "
        };
        let hash = state.roothash(slot);
        let hash_disp = if hash.is_empty() {
            "(unbuilt)".to_string()
        } else {
            short_hash(hash)
        };
        println!("{active} slot {slot}  roothash={hash_disp}");
    }
    let _ = cmd;
    Ok(())
}

/// The filesystem type used when (re)formatting a slot image, from the install
/// config — reserved for a future `--fresh` reformatting path.
#[allow(dead_code)]
fn slot_fs(fs: &Filesystem) -> &'static str {
    match fs {
        Filesystem::Btrfs => "btrfs",
        Filesystem::Xfs => "xfs",
        Filesystem::F2fs => "f2fs",
        _ => "ext4",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_targets_the_inactive_slot_then_keeps_filling_it() {
        // Nothing staged (active == running): build the other slot.
        assert_eq!(update_target("A", "A"), ("B".to_string(), false));
        assert_eq!(update_target("B", "B"), ("A".to_string(), false));

        // An update is staged into B while A is still running. The forgotten
        // package joins B; targeting "the inactive slot" would target A, the
        // running root, and rsync away everything B holds.
        assert_eq!(update_target("B", "A"), ("B".to_string(), true));
        assert_eq!(update_target("A", "B"), ("A".to_string(), true));
    }

    #[test]
    fn running_slot_reads_the_last_cmdline_token() {
        assert_eq!(parse_slot("ro deploytix.slot=B quiet"), Some("B".into()));
        assert_eq!(
            parse_slot("deploytix.slot=A deploytix.slot=b"),
            Some("B".into())
        );
        assert_eq!(parse_slot("deploytix.slot=\"a\""), Some("A".into()));
        // No token → the caller keeps its fallback (state.active).
        assert_eq!(parse_slot("root=/dev/mapper/x ro"), None);
        assert_eq!(parse_slot("deploytix.slot="), None);
        assert_eq!(running_slot("A"), running_slot("A"));
    }

    fn sample_state() -> SlotState {
        SlotState {
            active: "A".into(),
            vg: "vg0".into(),
            roothash_a: "aaaa1111bbbb2222".into(),
            roothash_b: "".into(),
        }
    }

    #[test]
    fn state_roundtrips() {
        let s = sample_state();
        let parsed = SlotState::from_conf(&s.to_conf());
        assert_eq!(parsed, s);
    }

    #[test]
    fn from_conf_defaults_and_ignores_comments() {
        let s = SlotState::from_conf("# comment\nactive=B\nvg=vg1\nroothash_b=deadbeef\n");
        assert_eq!(s.active, "B");
        assert_eq!(s.vg, "vg1");
        assert_eq!(s.roothash_a, "");
        assert_eq!(s.roothash_b, "deadbeef");
    }

    #[test]
    fn roothash_get_set_by_slot() {
        let mut s = sample_state();
        assert_eq!(s.roothash("A"), "aaaa1111bbbb2222");
        s.set_roothash("B", "ffff");
        assert_eq!(s.roothash("b"), "ffff");
    }

    #[test]
    fn pointer_sed_rewrites_both_tokens_globally() {
        let cmd = set_pointer_sed(GRUB_CFG, "B", "abc123");
        assert!(cmd.contains("deploytix.slot=[^ \"]*|deploytix.slot=B"));
        assert!(cmd.contains("deploytix.roothash=[^ \"]*|deploytix.roothash=abc123"));
        // Global flag on the first substitution (using `|` as the sed delimiter).
        assert!(cmd.contains("deploytix.slot=B|g;"));
    }

    #[test]
    fn mount_and_rsync_cmds_target_the_slot() {
        let m = mount_target_cmd("vg0", ab::ROOT_B, "B");
        assert!(m.contains("mount /dev/vg0/root_b"));
        assert!(m.contains("mount --rbind \"/$d\""));
        let r = rsync_root_cmd("B");
        assert!(r.contains("--delete"));
        assert!(r.contains("--exclude='/var/*'"));
        assert!(r.contains("/run/deploytix-slot/B/"));
    }

    #[test]
    fn update_dry_run_is_safe_without_state_file_errors() {
        // With no state file present read_state errors; ensure it is the
        // friendly config error, not a panic.
        // (Only meaningful when STATE_FILE is absent on the test host.)
        if !std::path::Path::new(STATE_FILE).exists() {
            let cmd = CommandRunner::new(true);
            let err = run_update(&cmd, &[], &UpdateOptions::default()).unwrap_err();
            assert!(matches!(err, DeploytixError::ConfigError(_)));
        }
    }
}
