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
use crate::immutable::history;
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

/// Kernel cmdline of the running system.
const PROC_CMDLINE: &str = "/proc/cmdline";

/// The slot letter named by a kernel cmdline's `deploytix.slot=`.
///
/// Last occurrence wins, matching the kernel's own handling of a repeated
/// parameter and the `verity-ab` hook that actually performed the mount.
pub fn parse_slot(cmdline: &str) -> Option<String> {
    let mut found = None;
    for token in cmdline.split_whitespace() {
        if let Some(value) = token.strip_prefix("deploytix.slot=") {
            let value = value.trim_matches('"').to_uppercase();
            if value == "A" || value == "B" {
                found = Some(value);
            }
        }
    }
    found
}

/// The slot the running system actually booted from.
///
/// This is deliberately **not** [`SlotState::active`]: `active` is written at the
/// end of an update and means "boots next". Between staging an update and
/// rebooting, the two differ — and treating `active` as the running slot is what
/// made a second update in one session select the live slot as its build target,
/// mounting a dm-verity data device read-write underneath the running system.
///
/// Falls back to `active` only when the cmdline says nothing, which means an
/// install predating the `deploytix.slot=` parameter; there is no better source
/// there, and the fallback reproduces the previous behaviour rather than
/// guessing.
pub fn running_slot(state: &SlotState) -> String {
    std::fs::read_to_string(PROC_CMDLINE)
        .ok()
        .and_then(|cmdline| parse_slot(&cmdline))
        .unwrap_or_else(|| {
            warn!(
                "[lvm-ab] No deploytix.slot= on the kernel cmdline; assuming the running \
                 slot is the state file's active slot ({})",
                state.active
            );
            state.active.to_uppercase()
        })
}

/// Pick the slot an update should build into, and whether that composes onto an
/// already-staged update.
///
/// Returns `(target, composing)`. The one hard rule is that `target` is never the
/// running slot: building there mounts a live dm-verity data device read-write
/// underneath the running system.
///
/// - Nothing staged (`staged == running`) → build into the other slot, fresh.
/// - Something staged → build into *that* slot, composing onto it, so the two
///   updates end up in one image rather than the second discarding the first.
pub fn select_target_slot(session: &crate::immutable::SessionState) -> (String, bool) {
    match session.pending() {
        Some(pending) => (pending.to_string(), true),
        None => (ab::other_slot(&session.running).to_string(), false),
    }
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

/// Perform a transactional A/B update: build the inactive slot, verity-seal it,
/// and repoint the boot pointer at it.
pub fn run_update(
    cmd: &CommandRunner,
    extra_packages: &[String],
    opts: &UpdateOptions,
) -> Result<()> {
    let _lock = update::acquire_update_lock(cmd)?;
    let state = read_state()?;
    let vg = state.vg.clone();

    // `state.active` is the slot that boots *next*, not the one running now —
    // those differ as soon as an update is staged. Read the running slot from
    // the cmdline and drive everything from that.
    let session = crate::immutable::SessionState {
        running: running_slot(&state),
        staged: state.active.to_uppercase(),
    };

    // Compose onto a slot already staged this session; otherwise build into the
    // slot we are not running from.
    let (target, composing) = select_target_slot(&session);

    // The invariant that keeps the running system intact. Building into the
    // running slot means mounting its root LV read-write while the live
    // dm-verity root sits on top of it, then rsync-ing and pacman-ing over it —
    // which both discards whatever was staged and invalidates the verity tree
    // of the system currently executing. Assert it rather than trusting the
    // selection above to stay correct.
    if target.eq_ignore_ascii_case(&session.running) {
        return Err(DeploytixError::ConfigError(format!(
            "refusing to build into slot {} — it is the running slot. \
             (running={}, staged={})",
            target, session.running, session.staged
        )));
    }

    let (root_lv, hash_lv) = ab::slot_lvs(&target)
        .ok_or_else(|| DeploytixError::ConfigError(format!("invalid target slot '{target}'")))?;

    if composing {
        info!(
            "[lvm-ab] Composing onto slot {} ({}/{}), already staged for next boot; \
             running slot is {}",
            target, root_lv, hash_lv, session.running
        );
    } else {
        info!(
            "[lvm-ab] Building update into inactive slot {} ({}/{}); running slot is {}",
            target, root_lv, hash_lv, session.running
        );
    }

    let (local_files, repo_names) = update::classify_args(extra_packages);

    // Everything here is unwound on failure so a bad update leaves the running
    // slot and boot pointer untouched.
    let started_at = history::now_secs();
    let start = std::time::Instant::now();

    let result = (|| -> Result<history::PackageChanges> {
        cmd.run("sh", &["-c", &mount_target_cmd(&vg, root_lv, &target)])?;
        let t = target_dir(&target);
        if composing {
            // The slot already holds the staged update's result. Re-syncing the
            // running root over it — with `--delete`, no less — is exactly how
            // that update would be reverted, which is the bug being fixed.
            info!(
                "[lvm-ab] Slot {} already carries a staged update; skipping the root sync \
                 so this update composes with it",
                target
            );
        } else {
            info!("[lvm-ab] Syncing running root -> slot {}", target);
            cmd.run("sh", &["-c", &rsync_root_cmd(&target)])?;
        }

        let staged = update::stage_local_pkgs(cmd, &local_files)?;
        // Bracket the transaction with two `pacman -Q` reads. /var is shared
        // across both slots and is not part of the verity-sealed root, so this
        // pair is the only record of what the slot's build changed.
        let before = history::query_packages(cmd, &t);
        info!("[lvm-ab] Running pacman in slot {}", target);
        for pac in update::pacman_cmds(&staged, &repo_names) {
            cmd.run_in_chroot(&t, &pac)?;
        }
        let after = history::query_packages(cmd, &t);
        cmd.run_in_chroot(&t, "mkinitcpio -P")?;
        Ok(history::diff(&before, &after))
    })();

    // Always release the chroot mounts and clear the package staging dir.
    let _ = cmd.run("sh", &["-c", &unmount_target_cmd(&target)]);
    if !cmd.is_dry_run() {
        let _ = std::fs::remove_dir_all(update::PKG_STAGE_DIR);
    }

    // Best-effort history entry, written for failures too — a failed update is
    // exactly what a user wants to look at afterwards.
    if !cmd.is_dry_run() {
        history::write_record(&history::UpdateRecord {
            started_at,
            duration_secs: start.elapsed().as_secs(),
            backend: history::Backend::LvmAb,
            target: target.clone(),
            composed_from: composing.then(|| session.staged.clone()),
            request: history::Request::classify(&repo_names, &local_files),
            outcome: match &result {
                Ok(_) => history::Outcome::Succeeded,
                Err(e) => history::Outcome::Failed(e.to_string()),
            },
            changes: result.as_ref().ok().cloned().unwrap_or_default(),
        });
    }

    if let Err(e) = result {
        if composing {
            // Composing writes into the slot the boot pointer *already* names.
            // A failed transaction has therefore modified the image that boots
            // next, without re-sealing it — its recorded root hash no longer
            // describes it, so the next boot fails verity on the default entry.
            // Send the pointer back to the running slot, which this transaction
            // never touched and whose hash is still valid.
            let running_hash = state.roothash(&session.running).to_string();
            if running_hash.is_empty() {
                warn!(
                    "[lvm-ab] Update failed while composing into slot {}, and slot {} has no \
                     recorded root hash to fall back to. That slot's image no longer matches \
                     its hash: pick a good slot at the GRUB prompt if the next boot fails.",
                    target, session.running
                );
            } else {
                warn!(
                    "[lvm-ab] Update failed while composing into slot {} (which the boot \
                     pointer already named). Repointing boot back to the running slot {}, \
                     whose image is untouched and still sealed; the staged update is lost.",
                    target, session.running
                );
                let mut reverted = state.clone();
                reverted.active = session.running.clone();
                let _ = write_state(cmd, &reverted);
                let _ = activate_slot(cmd, &session.running, &running_hash);
            }
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

    if composing {
        info!(
            "[lvm-ab] Update ready. Slot {} now carries this update composed with the one \
             already staged; reboot to activate it (rollback: `deploytix rollback`).",
            target
        );
    } else {
        info!(
            "[lvm-ab] Update ready. Reboot to activate slot {} (rollback: `deploytix rollback`).",
            target
        );
    }
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

    fn session(running: &str, staged: &str) -> crate::immutable::SessionState {
        crate::immutable::SessionState {
            running: running.to_string(),
            staged: staged.to_string(),
        }
    }

    // ── running slot: cmdline, not the state file ────────────────────────────

    #[test]
    fn parses_the_running_slot_from_the_cmdline() {
        assert_eq!(
            parse_slot("root=/dev/mapper/deploytix_root deploytix.slot=B ro"),
            Some("B".to_string())
        );
        assert_eq!(parse_slot("deploytix.slot=a quiet"), Some("A".to_string()));
    }

    #[test]
    fn a_repeated_slot_parameter_takes_the_last_value() {
        // Kernel behaviour for a repeated parameter, and what the verity-ab
        // hook itself acted on.
        assert_eq!(
            parse_slot("deploytix.slot=A quiet deploytix.slot=B"),
            Some("B".to_string())
        );
    }

    #[test]
    fn a_cmdline_without_a_slot_yields_nothing() {
        assert_eq!(parse_slot("root=/dev/sda2 ro quiet"), None);
        assert_eq!(parse_slot("deploytix.slot=C"), None, "only A and B exist");
    }

    // ── target selection: never the running slot ────────────────────────────

    /// The reported bug. After one update, `active` is the staged slot B while
    /// the machine still runs A. Selecting `other_slot(active)` picked A -- the
    /// running slot -- and rsync'd over a live dm-verity image.
    #[test]
    fn a_second_update_composes_onto_the_staged_slot_not_the_running_one() {
        let (target, composing) = select_target_slot(&session("A", "B"));
        assert_eq!(
            target, "B",
            "must build into the staged slot, not the running one"
        );
        assert!(composing);
    }

    #[test]
    fn a_first_update_builds_into_the_inactive_slot() {
        let (target, composing) = select_target_slot(&session("A", "A"));
        assert_eq!(target, "B");
        assert!(!composing);

        let (target, composing) = select_target_slot(&session("B", "B"));
        assert_eq!(target, "A");
        assert!(!composing);
    }

    /// The invariant that protects the running system, over every combination.
    #[test]
    fn the_target_is_never_the_running_slot() {
        for running in ["A", "B"] {
            for staged in ["A", "B"] {
                let (target, _) = select_target_slot(&session(running, staged));
                assert_ne!(
                    target, running,
                    "running={running} staged={staged} selected the running slot"
                );
            }
        }
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
