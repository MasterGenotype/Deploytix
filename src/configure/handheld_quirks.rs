//! Handheld controller quirks
//!
//! Ships a udev rule that stops the detachable/internal game controllers on
//! Lenovo Legion Go family handhelds (Legion Go, Legion Go 2, Legion Go S)
//! from repeatedly dropping and re-enumerating — the symptom users see is a
//! pad that vanishes and reappears in Steam every few seconds, sometimes
//! several times a minute.
//!
//! Three independent causes are addressed by one rule file, because on the
//! Legion Go 2 they compound:
//!
//! 1. **USB runtime power management.** The controllers hang off an internal
//!    USB hub. Left at the kernel default (`power/control = auto`) the link
//!    is runtime-suspended as soon as the pad goes briefly idle, and the
//!    controller firmware re-enumerates instead of resuming — userspace sees
//!    a disconnect immediately followed by a reconnect.
//! 2. **Driver binding.** The Legion Go controller IDs reached `xpad`
//!    upstream, but the Legion Go 2 IDs are newer than many stable kernels.
//!    With no `xpad` match the vendor-specific interface falls through to
//!    `hid-generic`, which misreads the report descriptor and makes the pad
//!    flap between present and gone.
//! 3. **hidraw permissions.** Handheld Daemon and Steam both open the raw
//!    HID interface. Upstream HHD's own user rules only match product IDs
//!    `618*`, which misses `0x61eb` and the Legion Go 2 range; a daemon that
//!    cannot open the node retries in a loop, tearing down and rebuilding
//!    its emulated pad on every attempt.
//!
//! Applying the rules is decided by [`decide`]: by default they are written
//! only when the installing host's DMI product name is a known Legion Go
//! family machine, and `packages.handheld_controller_quirks` can force the
//! decision either way (needed when deploying to removable media from a
//! different machine).

use crate::config::DeploymentConfig;
use crate::utils::command::CommandRunner;
use crate::utils::error::Result;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::OnceLock;
use tracing::info;

/// Sysfs file naming the machine model. Handheld Daemon keys its own device
/// detection off this same file, so the identifiers below match the ones it
/// uses.
const DMI_PRODUCT_NAME: &str = "/sys/devices/virtual/dmi/id/product_name";

/// Path of the shipped rule, relative to the install root.
const RULES_PATH: &str = "etc/udev/rules.d/60-deploytix-handheld-controllers.rules";

/// A handheld whose controllers need the quirks below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandheldModel {
    LegionGo,
    LegionGo2,
    LegionGoS,
}

impl HandheldModel {
    pub fn as_str(self) -> &'static str {
        match self {
            HandheldModel::LegionGo => "Lenovo Legion Go",
            HandheldModel::LegionGo2 => "Lenovo Legion Go 2",
            HandheldModel::LegionGoS => "Lenovo Legion Go S",
        }
    }
}

/// DMI `product_name` values for the Legion Go family.
///
/// Lenovo ships the bare machine type here (`83E1`, not "Legion Go"), and a
/// single model spans several types across SKUs and refreshes.
const LEGION_DMI_MODELS: &[(&str, HandheldModel)] = &[
    ("83E1", HandheldModel::LegionGo),
    ("83N0", HandheldModel::LegionGo2),
    ("83N1", HandheldModel::LegionGo2),
    ("83L3", HandheldModel::LegionGoS),
    ("83N6", HandheldModel::LegionGoS),
    ("83Q2", HandheldModel::LegionGoS),
    ("83Q3", HandheldModel::LegionGoS),
];

/// Map a DMI `product_name` to a known handheld, ignoring surrounding
/// whitespace and case.
pub fn model_for_dmi(product_name: &str) -> Option<HandheldModel> {
    let trimmed = product_name.trim();
    LEGION_DMI_MODELS
        .iter()
        .find(|(dmi, _)| trimmed.eq_ignore_ascii_case(dmi))
        .map(|(_, model)| *model)
}

/// Identify the machine deploytix is *running on*.
///
/// This is the target device in the normal case (booted from live media on
/// the handheld itself); when deploying to removable media from a desktop it
/// is not, which is why `packages.handheld_controller_quirks = true` exists.
///
/// Cached: DMI cannot change under a running kernel, and the GUI panel asks
/// on every frame.
pub fn detect_host_model() -> Option<HandheldModel> {
    static DETECTED: OnceLock<Option<HandheldModel>> = OnceLock::new();
    *DETECTED.get_or_init(|| {
        let product = fs::read_to_string(DMI_PRODUCT_NAME).ok()?;
        model_for_dmi(&product)
    })
}

/// Outcome of the auto/force/disable resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuirkDecision {
    /// Write the rules. Carries the detected model, if the host is one —
    /// `None` means the config forced the rules onto unrecognised hardware.
    Install(Option<HandheldModel>),
    /// Leave the target alone.
    Skip,
}

/// Resolve `packages.handheld_controller_quirks` against the host's DMI.
///
/// * `Some(false)` — never write the rules.
/// * `Some(true)` — always write them.
/// * `None` (the default) — write them only on a recognised handheld.
pub fn decide(config: &DeploymentConfig) -> QuirkDecision {
    match config.packages.handheld_controller_quirks {
        Some(false) => QuirkDecision::Skip,
        Some(true) => QuirkDecision::Install(detect_host_model()),
        None => match detect_host_model() {
            Some(model) => QuirkDecision::Install(Some(model)),
            None => QuirkDecision::Skip,
        },
    }
}

/// udev rules that keep the Legion Go family controllers attached.
///
/// Every rule matches vendor `17ef` (Lenovo) with product `61??` rather than
/// a fixed ID: the Legion controllers enumerate across `0x6182`, `0x6183`,
/// `0x6184`, `0x6185` and `0x61eb`, and the Legion Go 2 adds further IDs in
/// the same range. Matching the range covers SKUs and firmware revisions we
/// have not seen without needing a table of product IDs to keep current.
const CONTROLLER_RULES: &str = r#"# Handheld controller quirks (installed by Deploytix)
#
# Keeps the detachable/internal controllers on Lenovo Legion Go family
# handhelds (Legion Go, Legion Go 2, Legion Go S) from repeatedly
# disconnecting and reconnecting.
#
# Vendor 17ef is Lenovo. The Legion controllers enumerate in the 0x61xx
# product range (0x6182/0x6183/0x6184/0x6185 and 0x61eb on the Legion Go,
# with further 0x61xx IDs on the Legion Go 2), so each rule matches "61??"
# rather than pinning one product ID.

ACTION!="add|change", GOTO="deploytix_handheld_end"

# 1. Pin USB runtime power management off for the controllers.
#
# The controllers sit behind an internal USB hub. With runtime PM left at
# its "auto" default the link is suspended as soon as the pad goes briefly
# idle, and the controller firmware re-enumerates instead of resuming --
# which userspace sees as a disconnect immediately followed by a reconnect.
# Holding power/control at "on" keeps the port awake; runtime-PM references
# propagate upwards, so this also stops the internal hub suspending
# underneath them.
SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="17ef", ATTR{idProduct}=="61??", TEST=="power/control", ATTR{power/control}="on"
SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="17ef", ATTR{idProduct}=="61??", TEST=="power/autosuspend_delay_ms", ATTR{power/autosuspend_delay_ms}="-1"

# 2. Bind the controllers to xpad on kernels that predate their IDs.
#
# The Legion Go controller IDs landed in xpad upstream; the Legion Go 2 IDs
# are newer than many stable kernels. With no xpad match the vendor-specific
# interface falls through to hid-generic, which misreads the report
# descriptor and makes the pad flap between present and gone. Registering
# the ID through xpad's new_id is a no-op when the running kernel already
# knows it -- the write fails with EEXIST, which is swallowed.
SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="17ef", ATTR{idProduct}=="61??", RUN+="/bin/sh -c '/usr/bin/modprobe xpad; echo 17ef %s{idProduct} > /sys/bus/usb/drivers/xpad/new_id 2>/dev/null || true'"

# 3. Let the session user reach the controllers' hidraw nodes.
#
# Handheld Daemon and Steam both read the raw HID interface. Upstream HHD
# only grants access to product IDs matching "618*", which misses 0x61eb and
# the Legion Go 2 range; a daemon that cannot open the node retries in a
# loop, and each retry tears down and rebuilds its emulated pad.
KERNEL=="hidraw*", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="17ef", ATTRS{idProduct}=="61??", MODE="0660", GROUP="input", TAG+="uaccess"

LABEL="deploytix_handheld_end"
"#;

/// Write the handheld controller udev rules into the target system.
///
/// No-op when [`decide`] says to skip, so the caller can invoke this
/// unconditionally.
pub fn install(cmd: &CommandRunner, config: &DeploymentConfig, install_root: &str) -> Result<()> {
    let detected = match decide(config) {
        QuirkDecision::Skip => return Ok(()),
        QuirkDecision::Install(detected) => detected,
    };

    match detected {
        Some(model) => info!("Applying handheld controller quirks for {}", model.as_str()),
        None => info!(
            "Applying handheld controller quirks (forced by config; \
             this host is not a recognised Legion Go family device)"
        ),
    }

    if cmd.is_dry_run() {
        println!("  [dry-run] Would write /{}", RULES_PATH);
        return Ok(());
    }

    let rules_path = format!("{}/{}", install_root, RULES_PATH);
    if let Some(parent) = std::path::Path::new(&rules_path).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&rules_path, CONTROLLER_RULES)?;
    fs::set_permissions(&rules_path, fs::Permissions::from_mode(0o644))?;
    info!("  Written udev rule: /{}", RULES_PATH);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(flag: Option<bool>) -> DeploymentConfig {
        let mut config = DeploymentConfig::sample();
        config.packages.handheld_controller_quirks = flag;
        config
    }

    #[test]
    fn dmi_lookup_covers_the_legion_go_family() {
        assert_eq!(model_for_dmi("83E1"), Some(HandheldModel::LegionGo));
        assert_eq!(model_for_dmi("83N0"), Some(HandheldModel::LegionGo2));
        assert_eq!(model_for_dmi("83N1"), Some(HandheldModel::LegionGo2));
        assert_eq!(model_for_dmi("83L3"), Some(HandheldModel::LegionGoS));
        assert_eq!(model_for_dmi("83N6"), Some(HandheldModel::LegionGoS));
        assert_eq!(model_for_dmi("83Q2"), Some(HandheldModel::LegionGoS));
        assert_eq!(model_for_dmi("83Q3"), Some(HandheldModel::LegionGoS));
    }

    /// The sysfs read keeps the trailing newline, and vendors are not
    /// consistent about case.
    #[test]
    fn dmi_lookup_tolerates_whitespace_and_case() {
        assert_eq!(model_for_dmi("83N0\n"), Some(HandheldModel::LegionGo2));
        assert_eq!(model_for_dmi("  83n0  "), Some(HandheldModel::LegionGo2));
    }

    #[test]
    fn dmi_lookup_ignores_other_machines() {
        assert_eq!(model_for_dmi("20XW"), None);
        assert_eq!(model_for_dmi(""), None);
        // Prefix/suffix matches must not count.
        assert_eq!(model_for_dmi("83N"), None);
        assert_eq!(model_for_dmi("83N00"), None);
    }

    /// An explicit `false` wins over detection, so a user who does not want
    /// the rules never gets them even on a Legion Go.
    #[test]
    fn an_explicit_false_always_skips() {
        assert_eq!(decide(&config_with(Some(false))), QuirkDecision::Skip);
    }

    /// An explicit `true` installs even when the installing host is not a
    /// handheld — the deploy target may be removable media.
    #[test]
    fn an_explicit_true_always_installs() {
        assert!(matches!(
            decide(&config_with(Some(true))),
            QuirkDecision::Install(_)
        ));
    }

    /// Unset means "auto": the decision must agree with host detection.
    #[test]
    fn the_default_follows_host_detection() {
        let expected = match detect_host_model() {
            Some(model) => QuirkDecision::Install(Some(model)),
            None => QuirkDecision::Skip,
        };
        assert_eq!(decide(&config_with(None)), expected);
    }

    /// Every rule must match the whole 0x61xx range: a table of fixed
    /// product IDs is what leaves the Legion Go 2 uncovered.
    #[test]
    fn rules_match_the_legion_product_range_not_fixed_ids() {
        assert_eq!(CONTROLLER_RULES.matches(r#"=="61??""#).count(), 4);
        assert!(!CONTROLLER_RULES.contains(r#"idProduct=="6182""#));
        assert!(!CONTROLLER_RULES.contains(r#"idProduct=="618*""#));
    }

    #[test]
    fn rules_disable_runtime_suspend_and_bind_xpad() {
        assert!(CONTROLLER_RULES.contains(r#"ATTR{power/control}="on""#));
        assert!(CONTROLLER_RULES.contains(r#"ATTR{power/autosuspend_delay_ms}="-1""#));
        assert!(CONTROLLER_RULES.contains("/sys/bus/usb/drivers/xpad/new_id"));
        // A kernel that already knows the ID must not fail the rule.
        assert!(CONTROLLER_RULES.contains("|| true"));
    }

    /// The rules must land at the documented path inside the install root,
    /// world-readable so udev can load them.
    #[test]
    fn install_writes_the_rule_into_the_target_root() {
        let root = std::env::temp_dir().join(format!("deploytix-quirks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let root = root.to_string_lossy().to_string();

        let cmd = CommandRunner::new(false);
        install(&cmd, &config_with(Some(true)), &root).expect("install");

        let written = fs::read_to_string(format!("{}/{}", root, RULES_PATH)).expect("rule written");
        assert_eq!(written, CONTROLLER_RULES);
        let mode = fs::metadata(format!("{}/{}", root, RULES_PATH))
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644);

        let _ = fs::remove_dir_all(&root);
    }

    /// A disabled run must leave the target untouched, not write an empty
    /// or commented-out rule file.
    #[test]
    fn install_writes_nothing_when_disabled() {
        let root = std::env::temp_dir().join(format!("deploytix-noquirks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let root = root.to_string_lossy().to_string();

        let cmd = CommandRunner::new(false);
        install(&cmd, &config_with(Some(false)), &root).expect("install");

        assert!(!std::path::Path::new(&format!("{}/{}", root, RULES_PATH)).exists());

        let _ = fs::remove_dir_all(&root);
    }

    /// The tri-state must survive a TOML round trip: an absent key is
    /// "auto", and an explicit choice must come back unchanged rather than
    /// collapsing into it.
    #[test]
    fn the_flag_round_trips_through_toml() {
        for flag in [None, Some(true), Some(false)] {
            let config = config_with(flag);
            let text = toml::to_string(&config).expect("serialise");
            let parsed: DeploymentConfig = toml::from_str(&text).expect("parse");
            assert_eq!(parsed.packages.handheld_controller_quirks, flag);
        }
    }

    /// `DEVTYPE` is a uevent property, not a udev match key — writing it
    /// bare makes `udevadm verify` reject the file, and udev then loads
    /// none of the rules in it.
    #[test]
    fn devtype_is_matched_through_env() {
        assert!(CONTROLLER_RULES.contains(r#"ENV{DEVTYPE}=="usb_device""#));
        assert!(!CONTROLLER_RULES.contains(r#", DEVTYPE=="#));
    }

    /// Both the `GOTO` and its `LABEL` must be present or udev refuses to
    /// load the whole file.
    #[test]
    fn the_goto_has_a_matching_label() {
        assert!(CONTROLLER_RULES.contains(r#"GOTO="deploytix_handheld_end""#));
        assert!(CONTROLLER_RULES.contains(r#"LABEL="deploytix_handheld_end""#));
    }
}
