//! dm-verity helpers for the LVM immutable A/B backend.
//!
//! Each root slot (`root_a`/`root_b`) is a read-only OS image protected by a
//! dm-verity Merkle tree stored on a sibling hash LV (`hash_a`/`hash_b`).
//! [`format_verity`] builds the tree over a frozen slot and returns its **root
//! hash**; that hash is pinned in the slot's GRUB entry (`deploytix.roothash=`)
//! and passed to the initramfs, which opens the verity device with
//! [`verity_open_cmd`] before mounting `/` read-only.
//!
//! `veritysetup` ships with `cryptsetup`, which deploytix already depends on for
//! LUKS, so no new package is required.

use crate::utils::command::CommandRunner;
use crate::utils::error::{DeploytixError, Result};
use tracing::info;

/// device-mapper name the initramfs opens the active slot's verity device as.
pub const VERITY_MAPPER_NAME: &str = "deploytix_root";

/// Placeholder root hash returned by [`format_verity`] in dry-run mode.
pub const DRY_RUN_ROOT_HASH: &str = "<dry-run-roothash>";

/// Format a dm-verity hash tree for `data_dev`, storing it on `hash_dev`, and
/// return the resulting **root hash**.
///
/// `data_dev` must already hold the finished, frozen (read-only) filesystem — any
/// later change to it invalidates the hash. Runs `veritysetup format` and parses
/// the `Root hash:` line from its output. In dry-run mode nothing is executed and
/// [`DRY_RUN_ROOT_HASH`] is returned.
pub fn format_verity(cmd: &CommandRunner, data_dev: &str, hash_dev: &str) -> Result<String> {
    info!(
        "[verity] Formatting dm-verity tree for {} on {}",
        data_dev, hash_dev
    );
    let out = cmd.run("veritysetup", &["format", data_dev, hash_dev])?;
    let Some(out) = out else {
        // dry-run
        return Ok(DRY_RUN_ROOT_HASH.to_string());
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    parse_root_hash(&stdout).ok_or_else(|| DeploytixError::CommandFailed {
        command: format!("veritysetup format {data_dev} {hash_dev}"),
        stderr: format!("could not parse 'Root hash' from veritysetup output:\n{stdout}"),
    })
}

/// Extract the hex root hash from `veritysetup format` output (the value on the
/// `Root hash:` line).
fn parse_root_hash(output: &str) -> Option<String> {
    for line in output.lines() {
        // Lines look like: "Root hash:      <hex>"
        if let Some(rest) = line.split_once("Root hash:") {
            let hash = rest.1.trim();
            if !hash.is_empty() {
                return Some(hash.to_string());
            }
        }
    }
    None
}

/// Shell command that opens the verity device `data_dev` (verified against
/// `hash_dev` + `root_hash`) as `/dev/mapper/{name}`. Used inside the initramfs
/// hook and for direct activation.
pub fn verity_open_cmd(name: &str, data_dev: &str, hash_dev: &str, root_hash: &str) -> String {
    format!("veritysetup open {data_dev} {name} {hash_dev} {root_hash}")
}

/// Open the active slot's verity device as [`VERITY_MAPPER_NAME`].
pub fn verity_open(
    cmd: &CommandRunner,
    data_dev: &str,
    hash_dev: &str,
    root_hash: &str,
) -> Result<()> {
    cmd.run(
        "veritysetup",
        &["open", data_dev, VERITY_MAPPER_NAME, hash_dev, root_hash],
    )
    .map(|_| ())
}

/// Close a verity mapping opened by [`verity_open`].
pub fn verity_close(cmd: &CommandRunner, name: &str) -> Result<()> {
    cmd.run("veritysetup", &["close", name]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_root_hash_from_veritysetup_output() {
        let sample = "\
VERITY header information for /dev/vg/root_a
UUID:            8f0e...
Hash type:       1
Data blocks:     262144
Data block size: 4096
Hash block size: 4096
Hash algorithm:  sha256
Salt:            abcd1234
Root hash:      53b4...deadbeef
";
        assert_eq!(parse_root_hash(sample).as_deref(), Some("53b4...deadbeef"));
    }

    #[test]
    fn missing_root_hash_returns_none() {
        assert_eq!(parse_root_hash("no hash here\n"), None);
        assert_eq!(parse_root_hash("Root hash:   \n"), None);
    }

    #[test]
    fn open_cmd_shape() {
        let c = verity_open_cmd(
            VERITY_MAPPER_NAME,
            "/dev/vg/root_a",
            "/dev/vg/hash_a",
            "abc123",
        );
        assert_eq!(
            c,
            "veritysetup open /dev/vg/root_a deploytix_root /dev/vg/hash_a abc123"
        );
    }

    #[test]
    fn format_is_dry_run_safe() {
        let cmd = CommandRunner::new(true);
        let h = format_verity(&cmd, "/dev/vg/root_a", "/dev/vg/hash_a").unwrap();
        assert_eq!(h, DRY_RUN_ROOT_HASH);
    }
}
