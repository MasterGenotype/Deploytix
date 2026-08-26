//! Device-mapper ownership resolution.
//!
//! Teardown (emergency cleanup and the `cleanup` subcommand) must only ever
//! touch device-mapper nodes belonging to the disk being installed to.  A
//! deploytix-deployed host runs on containers named by the very same scheme
//! the installer uses (`Crypt-Root`, `Crypt-Home`, …), so selecting victims
//! by name prefix would target the live system's own volumes.
//!
//! Everything here resolves *ownership* instead: which physical disks back a
//! given mapping, walking the dm stack down through `/sys/class/block/…/slaves`.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Guard against pathological or cyclic dm stacks.
const MAX_STACK_DEPTH: usize = 16;

/// Resolve a device path to its kernel block name.
///
/// `/dev/mapper/Crypt-Root` → `dm-3`, `/dev/sda3` → `sda3`.
/// Returns `None` when the node does not exist.
pub fn kernel_name(device: &str) -> Option<String> {
    let path = if device.starts_with('/') {
        Path::new(device).to_path_buf()
    } else {
        Path::new("/dev").join(device)
    };
    let resolved = fs::canonicalize(path).ok()?;
    Some(resolved.file_name()?.to_string_lossy().to_string())
}

/// Map a kernel block name to the whole-disk it belongs to.
///
/// `sda3` → `sda`, `nvme0n1p2` → `nvme0n1`, `sda` → `sda`.
fn base_disk(kname: &str) -> Option<String> {
    let sys = Path::new("/sys/class/block").join(kname);
    if !sys.exists() {
        return None;
    }
    // A partition carries a `partition` attribute; its parent directory in
    // sysfs is the containing disk.
    if sys.join("partition").exists() {
        let resolved = fs::canonicalize(&sys).ok()?;
        let parent = resolved.parent()?;
        return Some(parent.file_name()?.to_string_lossy().to_string());
    }
    Some(kname.to_string())
}

/// Every physical disk backing `device`, walking dm mappings down to the
/// partitions (and thus disks) they are built on.
///
/// A stacked layout — LUKS over an LV over a PV over a partition — resolves to
/// the single disk holding that partition.  An empty set means the backing
/// could not be determined; callers must treat that as "not mine".
pub fn backing_disks(device: &str) -> BTreeSet<String> {
    let mut disks = BTreeSet::new();
    let Some(kname) = kernel_name(device) else {
        return disks;
    };
    let mut seen = BTreeSet::new();
    collect_backing(&kname, &mut disks, &mut seen, 0);
    disks
}

fn collect_backing(
    kname: &str,
    disks: &mut BTreeSet<String>,
    seen: &mut BTreeSet<String>,
    depth: usize,
) {
    if depth > MAX_STACK_DEPTH || !seen.insert(kname.to_string()) {
        return;
    }

    let slaves = Path::new("/sys/class/block").join(kname).join("slaves");
    let mut had_slave = false;
    if let Ok(entries) = fs::read_dir(&slaves) {
        for entry in entries.filter_map(|e| e.ok()) {
            had_slave = true;
            let slave = entry.file_name().to_string_lossy().to_string();
            collect_backing(&slave, disks, seen, depth + 1);
        }
    }

    // A node with no slaves is a real partition or disk — the bottom of the stack.
    if !had_slave {
        if let Some(disk) = base_disk(kname) {
            disks.insert(disk);
        }
    }
}

/// True when `device` resolves to at least one backing disk and every one of
/// them is in `disks`.
///
/// Unresolvable backing yields `false`: teardown skips what it cannot prove it
/// owns.
pub fn backed_only_by(device: &str, disks: &BTreeSet<String>) -> bool {
    if disks.is_empty() {
        return false;
    }
    let backing = backing_disks(device);
    !backing.is_empty() && backing.iter().all(|d| disks.contains(d))
}

/// Disks backing everything currently mounted under `root`.
///
/// This is how teardown learns which disk it is working on when no target was
/// named explicitly: whatever the in-progress install has mounted under
/// `/install` defines the blast radius.
pub fn disks_mounted_under(root: &str) -> BTreeSet<String> {
    let mut disks = BTreeSet::new();
    let Ok(mounts) = fs::read_to_string("/proc/mounts") else {
        return disks;
    };
    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let (source, target) = (parts[0], parts[1]);
        if !is_under(target, root) || !source.starts_with("/dev/") {
            continue;
        }
        disks.extend(backing_disks(source));
    }
    disks
}

/// Mount points served by `device` that live outside `root`.
///
/// A non-empty result means the mapping is carrying a filesystem of the
/// running system, not of the installation — teardown must leave it alone
/// even if the ownership check somehow passed.
pub fn mounts_outside(device: &str, root: &str) -> Vec<String> {
    let mut found = Vec::new();
    let Some(kname) = kernel_name(device) else {
        return found;
    };
    let Ok(mounts) = fs::read_to_string("/proc/mounts") else {
        return found;
    };
    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let (source, target) = (parts[0], parts[1]);
        if !source.starts_with("/dev/") || is_under(target, root) {
            continue;
        }
        if kernel_name(source).as_deref() == Some(kname.as_str()) {
            found.push(target.to_string());
        }
    }
    found
}

/// Path-aware prefix test: `/installer` is not under `/install`.
pub fn is_under(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    path == root || path.starts_with(&format!("{}/", root))
}

/// Device-mapper nodes for LVM volume group `vg`.
///
/// LVM encodes `vg-lv` in the dm name, doubling literal hyphens in either
/// component, so the prefix for a VG's nodes is its escaped name plus `-`.
pub fn vg_mapper_nodes(vg: &str) -> Vec<String> {
    let prefix = format!("{}-", vg.replace('-', "--"));
    let mut nodes = Vec::new();
    let Ok(entries) = fs::read_dir("/dev/mapper") else {
        return nodes;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(&prefix) {
            nodes.push(name);
        }
    }
    nodes
}

/// Whether volume group `vg` is active and built entirely on `disks`.
///
/// A VG with no active nodes returns `false` — there is nothing to deactivate,
/// and a name match alone is never enough to act on.
pub fn vg_backed_only_by(vg: &str, disks: &BTreeSet<String>) -> bool {
    let nodes = vg_mapper_nodes(vg);
    !nodes.is_empty()
        && nodes
            .iter()
            .all(|n| backed_only_by(&format!("/dev/mapper/{}", n), disks))
}

/// Names of dm nodes under `/dev/mapper` matching `predicate`.
pub fn mapper_nodes<F: Fn(&str) -> bool>(predicate: F) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir("/dev/mapper") else {
        return names;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if predicate(&name) {
            names.push(name);
        }
    }
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_under_requires_a_path_boundary() {
        assert!(is_under("/install", "/install"));
        assert!(is_under("/install/boot", "/install"));
        assert!(is_under("/install/", "/install"));
        assert!(!is_under("/installer", "/install"));
        assert!(!is_under("/", "/install"));
    }

    #[test]
    fn vg_node_prefix_doubles_hyphens() {
        // LVM escapes hyphens in VG names, so vg-0 maps to vg--0-<lv>.
        let prefix = format!("{}-", "vg-0".replace('-', "--"));
        assert_eq!(prefix, "vg--0-");
        assert!("vg--0-thinpool".starts_with(&prefix));
        // A different VG whose name merely shares a prefix must not match.
        assert!(!"vg--01-thinpool".starts_with("vg--0-t"));
    }

    #[test]
    fn unknown_backing_is_never_owned() {
        let mut disks = BTreeSet::new();
        disks.insert("sda".to_string());
        // A node that does not exist resolves to no backing disks at all.
        assert!(!backed_only_by(
            "/dev/mapper/deploytix-no-such-node",
            &disks
        ));
        // An empty target set never owns anything, even a real device.
        assert!(!backed_only_by("/dev/mapper/anything", &BTreeSet::new()));
    }
}
