//! Handheld controller diagnostics
//!
//! Renders, in one command, everything needed to reason about a handheld
//! game controller that misbehaves: its USB identity (including
//! `bcdDevice`, the only field that can distinguish two hardware
//! generations sharing a product ID), whether the device declares
//! remote-wakeup capability, its runtime power-management state, which
//! driver bound each USB interface, and which driver bound each HID child.
//!
//! Everything is read straight from sysfs rather than by shelling out to
//! `lsusb`/`usb-devices`, so the report works on a freshly deployed system
//! that has no `usbutils` installed, and needs no root.

use crate::configure::handheld_quirks;
use crate::utils::error::Result;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

/// Root of the USB device tree in sysfs.
const USB_DEVICES: &str = "/sys/bus/usb/devices";

/// Root of the HID device tree in sysfs.
const HID_DEVICES: &str = "/sys/bus/hid/devices";

/// Absolute path of the udev rule `handheld_quirks` installs, as it exists
/// on a booted target (the module stores it install-root relative).
const QUIRK_RULE: &str = "/etc/udev/rules.d/60-deploytix-handheld-controllers.rules";

/// USB vendors whose devices this report shows by default.
///
/// Restricting the default keeps the output readable on a machine with a
/// dock full of peripherals; `--all` widens it to every USB device.
const CONTROLLER_VENDORS: &[(&str, &str)] = &[
    ("17ef", "Lenovo (Legion Go family)"),
    ("1a86", "QinHeng (Legion Go S controller)"),
    ("0b05", "ASUS (ROG Ally)"),
    ("0db0", "MSI (Claw)"),
    ("2993", "TECNO (Pocket Go)"),
    ("28de", "Valve (Steam Deck / Steam Controller)"),
    ("054c", "Sony (DualShock / DualSense)"),
    ("045e", "Microsoft (Xbox)"),
    ("057e", "Nintendo"),
];

/// Human-readable name for a USB interface class code.
fn class_name(code: &str) -> &'static str {
    match code {
        "01" => "audio",
        "03" => "HID",
        "08" => "mass storage",
        "09" => "hub",
        "0a" => "CDC data",
        "0e" => "video",
        "e0" => "wireless",
        "ff" => "vendor-specific",
        _ => "",
    }
}

/// Wrap `text` to `width` columns, indenting continuation lines by
/// `indent` spaces.
///
/// The report is read on a handheld screen more often than a desktop one,
/// so the long explanatory notes are wrapped rather than left to the
/// terminal, which would reflow them hard against the left margin and lose
/// the bullet structure.
fn wrap(text: &str, width: usize, indent: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let budget = if lines.is_empty() {
            width
        } else {
            width - indent
        };
        if !current.is_empty() && current.len() + 1 + word.len() > budget {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// One USB interface of a controller, and the driver bound to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbInterface {
    pub number: String,
    pub class: String,
    pub sub_class: String,
    pub protocol: String,
    pub driver: Option<String>,
}

/// One HID device sitting underneath a controller's interfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidChild {
    pub name: String,
    pub driver: Option<String>,
}

/// A USB device and everything about it that bears on controller behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UsbController {
    pub sysfs_name: String,
    pub vendor: String,
    pub product: String,
    pub bcd_device: String,
    pub product_name: Option<String>,
    pub manufacturer: Option<String>,
    pub serial: Option<String>,
    /// Configuration `bmAttributes`. Bit 5 (0x20) is remote-wakeup support.
    pub bm_attributes: Option<u8>,
    pub power_control: Option<String>,
    pub runtime_status: Option<String>,
    pub autosuspend_delay_ms: Option<String>,
    pub interfaces: Vec<UsbInterface>,
    pub hid_children: Vec<HidChild>,
}

impl UsbController {
    /// `vendor:product`, the form used by `lsusb` and udev rules.
    pub fn id(&self) -> String {
        format!("{}:{}", self.vendor, self.product)
    }

    /// `bcdDevice` rendered the way `usb-devices` prints it (`0100` →
    /// `01.00`). This is the field to compare across hardware generations
    /// that share a product ID.
    pub fn revision(&self) -> String {
        let raw = self.bcd_device.trim();
        if raw.len() == 4 {
            format!("{}.{}", &raw[..2], &raw[2..])
        } else {
            raw.to_string()
        }
    }

    /// Whether the device says it can wake the host from USB suspend.
    ///
    /// `None` when `bmAttributes` could not be read. A device that answers
    /// `false` cannot signal a resume, so runtime-suspending it risks a
    /// drop-and-re-enumerate cycle instead of a clean wake.
    pub fn supports_remote_wakeup(&self) -> Option<bool> {
        self.bm_attributes.map(|attrs| attrs & 0x20 != 0)
    }

    /// Problems worth the reader's attention, most load-bearing first.
    ///
    /// Deliberately phrased as observations rather than verdicts: a split
    /// across HID drivers can be a driver declining an interface on
    /// purpose, and only the kernel log distinguishes that from a real gap.
    pub fn findings(&self) -> Vec<String> {
        let mut findings = Vec::new();

        if self.supports_remote_wakeup() == Some(false) {
            let control = self.power_control.as_deref().unwrap_or("unknown");
            if control == "auto" {
                findings.push(
                    "declares NO remote-wakeup capability but runtime PM is set to \
                     'auto' — if the kernel suspends it, it cannot signal a resume, \
                     which typically shows up as a disconnect immediately followed \
                     by a reconnect. This is what the deploytix quirk rule pins to \
                     'on'."
                        .to_string(),
                );
            } else {
                findings.push(format!(
                    "declares NO remote-wakeup capability; runtime PM is pinned to \
                     '{}', so it will not be suspended.",
                    control
                ));
            }
        }

        for iface in &self.interfaces {
            if iface.driver.is_none() {
                findings.push(format!(
                    "interface {} (class {}) has no driver bound.",
                    iface.number, iface.class
                ));
            }
        }

        // A vendor-specific HID driver on some children and hid-generic on
        // others is the shape of a missing match entry — or of a driver
        // that probed and declined. Report it, do not diagnose it.
        let generic: Vec<&HidChild> = self
            .hid_children
            .iter()
            .filter(|c| c.driver.as_deref() == Some("hid-generic"))
            .collect();
        let specific: Vec<&HidChild> = self
            .hid_children
            .iter()
            .filter(|c| {
                c.driver
                    .as_deref()
                    .is_some_and(|d| d != "hid-generic" && !d.is_empty())
            })
            .collect();
        if !generic.is_empty() && !specific.is_empty() {
            let specific_driver = specific[0].driver.as_deref().unwrap_or("?");
            findings.push(format!(
                "HID children are split across drivers: {} on hid-generic, {} on \
                 {}. That is either a driver declining an interface on purpose or \
                 a missing match entry — check `sudo dmesg | grep -i hid` to tell \
                 which.",
                generic.len(),
                specific.len(),
                specific_driver
            ));
        }

        findings
    }
}

/// Read a sysfs attribute, trimmed, treating an unreadable file as absent.
fn attr(dir: &Path, name: &str) -> Option<String> {
    let value = fs::read_to_string(dir.join(name)).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Name of the driver bound to a device directory, via its `driver` symlink.
fn bound_driver(dir: &Path) -> Option<String> {
    let target = fs::read_link(dir.join("driver")).ok()?;
    Some(target.file_name()?.to_string_lossy().to_string())
}

/// Collect the USB interfaces belonging to `device_name` (e.g. `1-1`).
///
/// Interface directories are siblings of the device, named `<device>:<cfg>.<n>`.
fn collect_interfaces(root: &Path, device_name: &str) -> Vec<UsbInterface> {
    let prefix = format!("{}:", device_name);
    let mut interfaces = Vec::new();

    let Ok(entries) = fs::read_dir(root) else {
        return interfaces;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(&prefix) {
            continue;
        }
        let dir = entry.path();
        interfaces.push(UsbInterface {
            number: attr(&dir, "bInterfaceNumber").unwrap_or_else(|| "?".to_string()),
            class: attr(&dir, "bInterfaceClass").unwrap_or_else(|| "??".to_string()),
            sub_class: attr(&dir, "bInterfaceSubClass").unwrap_or_default(),
            protocol: attr(&dir, "bInterfaceProtocol").unwrap_or_default(),
            driver: bound_driver(&dir),
        });
    }
    interfaces.sort_by(|a, b| a.number.cmp(&b.number));
    interfaces
}

/// Collect the HID devices that live underneath `device_path`.
///
/// Matched by resolving each HID device's real path and testing whether the
/// USB device's real path is a prefix of it — rather than by comparing
/// vendor/product, which would conflate two identical controllers.
fn collect_hid_children(hid_root: &Path, device_path: &Path) -> Vec<HidChild> {
    let mut children = Vec::new();
    let Ok(usb_real) = fs::canonicalize(device_path) else {
        return children;
    };
    let Ok(entries) = fs::read_dir(hid_root) else {
        return children;
    };
    for entry in entries.flatten() {
        let Ok(hid_real) = fs::canonicalize(entry.path()) else {
            continue;
        };
        if !hid_real.starts_with(&usb_real) {
            continue;
        }
        children.push(HidChild {
            name: entry.file_name().to_string_lossy().to_string(),
            driver: bound_driver(&hid_real),
        });
    }
    children.sort_by(|a, b| a.name.cmp(&b.name));
    children
}

/// Read one USB device directory into a [`UsbController`].
///
/// Returns `None` for entries that are not USB devices (interface
/// directories have no `idVendor`).
fn read_device(dir: &Path, hid_root: &Path) -> Option<UsbController> {
    let vendor = attr(dir, "idVendor")?;
    let product = attr(dir, "idProduct")?;
    let name = dir.file_name()?.to_string_lossy().to_string();
    let usb_root = dir.parent()?;

    Some(UsbController {
        interfaces: collect_interfaces(usb_root, &name),
        hid_children: collect_hid_children(hid_root, dir),
        sysfs_name: name,
        vendor,
        product,
        bcd_device: attr(dir, "bcdDevice").unwrap_or_default(),
        product_name: attr(dir, "product"),
        manufacturer: attr(dir, "manufacturer"),
        serial: attr(dir, "serial"),
        bm_attributes: attr(dir, "bmAttributes")
            .and_then(|raw| u8::from_str_radix(raw.trim_start_matches("0x"), 16).ok()),
        power_control: attr(dir, "power/control"),
        runtime_status: attr(dir, "power/runtime_status"),
        autosuspend_delay_ms: attr(dir, "power/autosuspend_delay_ms"),
    })
}

/// Gather controllers from a sysfs tree.
///
/// `all` includes every USB device rather than only the vendors in
/// [`CONTROLLER_VENDORS`]. Split from the rendering so tests can build a
/// synthetic tree.
pub fn collect_from(usb_root: &Path, hid_root: &Path, all: bool) -> Vec<UsbController> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(usb_root) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(device) = read_device(&path, hid_root) else {
            continue;
        };
        if all || CONTROLLER_VENDORS.iter().any(|(v, _)| *v == device.vendor) {
            found.push(device);
        }
    }
    found.sort_by(|a, b| a.sysfs_name.cmp(&b.sysfs_name));
    found
}

/// Render the full report for a set of controllers.
pub fn render(controllers: &[UsbController], quirk_rule_present: bool, all: bool) -> String {
    let mut out = String::new();

    out.push_str("Deploytix controller report\n");
    out.push_str("===========================\n\n");

    // ---- Host ------------------------------------------------------------
    out.push_str("Host\n");
    let dmi = fs::read_to_string("/sys/devices/virtual/dmi/id/product_name")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "(unreadable)".to_string());
    let _ = writeln!(out, "  DMI product_name : {}", dmi);
    let _ = writeln!(
        out,
        "  Detected model   : {}",
        handheld_quirks::detect_host_model()
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| "not a known handheld".to_string())
    );
    let _ = writeln!(
        out,
        "  Quirk rule       : {}",
        if quirk_rule_present {
            "installed"
        } else {
            "NOT installed"
        }
    );
    // On its own line: the full path does not fit beside the label on a
    // handheld screen.
    let _ = writeln!(out, "    {}", QUIRK_RULE);
    out.push('\n');

    // ---- Devices ---------------------------------------------------------
    if controllers.is_empty() {
        out.push_str(if all {
            "No USB devices found.\n"
        } else {
            "No controller-vendor USB devices found. Re-run with --all to list \
             every USB device.\n"
        });
        return out;
    }

    for device in controllers {
        let label = device
            .product_name
            .clone()
            .unwrap_or_else(|| "(no product string)".to_string());
        let _ = writeln!(out, "{}  {}  \"{}\"", device.sysfs_name, device.id(), label);

        let vendor_note = CONTROLLER_VENDORS
            .iter()
            .find(|(v, _)| *v == device.vendor)
            .map(|(_, name)| *name)
            .unwrap_or("");
        if !vendor_note.is_empty() {
            let _ = writeln!(out, "  vendor          : {}", vendor_note);
        }
        let _ = writeln!(
            out,
            "  bcdDevice       : {}   <- compare across hardware generations",
            device.revision()
        );
        let _ = writeln!(
            out,
            "  serial          : {}",
            device.serial.as_deref().unwrap_or("(none)")
        );
        let _ = writeln!(
            out,
            "  remote wakeup   : {}",
            match device.supports_remote_wakeup() {
                Some(true) => "supported".to_string(),
                Some(false) => format!(
                    "NOT supported  (bmAttributes=0x{:02x})",
                    device.bm_attributes.unwrap_or(0)
                ),
                None => "unknown".to_string(),
            }
        );
        let _ = writeln!(
            out,
            "  runtime PM      : control={}  status={}  autosuspend_delay={}ms",
            device.power_control.as_deref().unwrap_or("?"),
            device.runtime_status.as_deref().unwrap_or("?"),
            device.autosuspend_delay_ms.as_deref().unwrap_or("?"),
        );

        out.push_str("  interfaces      :\n");
        for iface in &device.interfaces {
            let class = class_name(&iface.class);
            let class_label = if class.is_empty() {
                iface.class.clone()
            } else {
                format!("{} ({})", iface.class, class)
            };
            let _ = writeln!(
                out,
                "    if {}  class {:<20}  driver {}",
                iface.number,
                class_label,
                iface.driver.as_deref().unwrap_or("(none)")
            );
        }

        if !device.hid_children.is_empty() {
            out.push_str("  HID children    :\n");
            for child in &device.hid_children {
                let _ = writeln!(
                    out,
                    "    {}  {}",
                    child.name,
                    child.driver.as_deref().unwrap_or("(none)")
                );
            }
        }

        let findings = device.findings();
        if !findings.is_empty() {
            out.push_str("  notes           :\n");
            for finding in findings {
                for (n, line) in wrap(&finding, 72, 6).into_iter().enumerate() {
                    if n == 0 {
                        let _ = writeln!(out, "    ! {}", line);
                    } else {
                        let _ = writeln!(out, "      {}", line);
                    }
                }
            }
        }
        out.push('\n');
    }

    out
}

/// Build the report against the live system.
pub fn report(all: bool) -> Result<String> {
    let controllers = collect_from(Path::new(USB_DEVICES), Path::new(HID_DEVICES), all);
    Ok(render(&controllers, Path::new(QUIRK_RULE).exists(), all))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legion_go_2() -> UsbController {
        // Values observed on a Legion Go 2 (17ef:61eb, Rev 01.00,
        // bmAttributes 0x80 => bus-powered, no remote wakeup).
        UsbController {
            sysfs_name: "1-1".to_string(),
            vendor: "17ef".to_string(),
            product: "61eb".to_string(),
            bcd_device: "0100".to_string(),
            product_name: Some("Legion Controller for Windows".to_string()),
            manufacturer: None,
            serial: Some("32869681".to_string()),
            bm_attributes: Some(0x80),
            power_control: Some("auto".to_string()),
            runtime_status: Some("active".to_string()),
            autosuspend_delay_ms: Some("2000".to_string()),
            interfaces: vec![
                UsbInterface {
                    number: "00".to_string(),
                    class: "ff".to_string(),
                    sub_class: "5d".to_string(),
                    protocol: "01".to_string(),
                    driver: Some("xpad".to_string()),
                },
                UsbInterface {
                    number: "01".to_string(),
                    class: "03".to_string(),
                    sub_class: "00".to_string(),
                    protocol: "00".to_string(),
                    driver: Some("usbhid".to_string()),
                },
            ],
            hid_children: vec![
                HidChild {
                    name: "0003:17EF:61EB.000A".to_string(),
                    driver: Some("hid-generic".to_string()),
                },
                HidChild {
                    name: "0003:17EF:61EB.000B".to_string(),
                    driver: Some("hid-lenovo-go".to_string()),
                },
            ],
        }
    }

    /// Notes are read on a handheld screen: no rendered line may exceed
    /// the width, and continuation lines stay indented under the bullet.
    #[test]
    fn notes_are_wrapped_for_a_narrow_screen() {
        let out = render(&[legion_go_2()], false, false);
        for line in out.lines() {
            assert!(
                line.chars().count() <= 78,
                "line too wide ({}): {:?}",
                line.chars().count(),
                line
            );
        }
        assert!(out.contains("    ! declares NO remote-wakeup"));
    }

    /// A word longer than the budget must still be emitted, not dropped or
    /// spun on forever.
    #[test]
    fn wrapping_passes_through_an_overlong_word() {
        let long = "x".repeat(50);
        assert_eq!(wrap(&long, 10, 2), vec![long]);
        assert!(wrap("", 10, 2).is_empty());
    }

    #[test]
    fn revision_is_rendered_the_way_usb_devices_prints_it() {
        assert_eq!(legion_go_2().revision(), "01.00");
    }

    /// A short or malformed bcdDevice must pass through rather than panic
    /// on a slice boundary.
    #[test]
    fn revision_tolerates_unexpected_widths() {
        let mut device = legion_go_2();
        device.bcd_device = "100".to_string();
        assert_eq!(device.revision(), "100");
        device.bcd_device = String::new();
        assert_eq!(device.revision(), "");
    }

    /// Bit 5 of bmAttributes is remote wakeup: 0x80 is bus-powered without
    /// it, 0xa0 is bus-powered with it.
    #[test]
    fn remote_wakeup_reads_bit_five_of_bm_attributes() {
        let mut device = legion_go_2();
        assert_eq!(device.supports_remote_wakeup(), Some(false));
        device.bm_attributes = Some(0xa0);
        assert_eq!(device.supports_remote_wakeup(), Some(true));
        device.bm_attributes = Some(0xe0);
        assert_eq!(device.supports_remote_wakeup(), Some(true));
        device.bm_attributes = None;
        assert_eq!(device.supports_remote_wakeup(), None);
    }

    /// The pairing that actually causes the flapping: cannot wake itself,
    /// yet is left eligible for runtime suspend.
    #[test]
    fn no_remote_wakeup_plus_auto_pm_is_flagged() {
        let findings = legion_go_2().findings();
        assert!(
            findings
                .iter()
                .any(|f| f.contains("NO remote-wakeup") && f.contains("'auto'")),
            "expected the auto-PM warning, got {:?}",
            findings
        );
    }

    /// Once the quirk rule has pinned control to "on", the same device must
    /// not still be reported as at risk.
    #[test]
    fn pinning_power_control_clears_the_warning() {
        let mut device = legion_go_2();
        device.power_control = Some("on".to_string());
        let findings = device.findings();
        assert!(findings.iter().any(|f| f.contains("will not be suspended")));
        assert!(!findings.iter().any(|f| f.contains("'auto'")));
    }

    #[test]
    fn a_split_across_hid_drivers_is_reported() {
        let findings = legion_go_2().findings();
        assert!(
            findings
                .iter()
                .any(|f| f.contains("split across drivers") && f.contains("hid-lenovo-go")),
            "expected the HID split note, got {:?}",
            findings
        );
    }

    /// All children on one driver is the healthy case and must stay quiet.
    #[test]
    fn a_uniform_hid_driver_is_not_reported() {
        let mut device = legion_go_2();
        device.hid_children = vec![HidChild {
            name: "0003:17EF:61EB.000B".to_string(),
            driver: Some("hid-lenovo-go".to_string()),
        }];
        assert!(!device
            .findings()
            .iter()
            .any(|f| f.contains("split across drivers")));
    }

    #[test]
    fn an_unbound_interface_is_reported() {
        let mut device = legion_go_2();
        device.interfaces[1].driver = None;
        assert!(device
            .findings()
            .iter()
            .any(|f| f.contains("interface 01") && f.contains("no driver bound")));
    }

    #[test]
    fn the_report_shows_the_identity_fields_and_drivers() {
        let out = render(&[legion_go_2()], true, false);
        assert!(out.contains("17ef:61eb"));
        assert!(out.contains("Legion Controller for Windows"));
        assert!(out.contains("bcdDevice       : 01.00"));
        assert!(out.contains("32869681"));
        assert!(out.contains("NOT supported"));
        assert!(out.contains("driver xpad"));
        assert!(out.contains("hid-lenovo-go"));
        assert!(out.contains("Quirk rule       : installed"));
    }

    #[test]
    fn a_missing_quirk_rule_is_called_out() {
        let out = render(&[legion_go_2()], false, false);
        assert!(out.contains("NOT installed"));
    }

    /// Empty output must tell the reader how to widen the search rather
    /// than looking like a failure.
    #[test]
    fn an_empty_default_report_points_at_the_all_flag() {
        let out = render(&[], true, false);
        assert!(out.contains("--all"));
        let out_all = render(&[], true, true);
        assert!(!out_all.contains("--all"));
    }

    /// Build a synthetic sysfs mirroring the real layout: devices are real
    /// directories under a `devices` root, and the `bus/*/devices` roots
    /// hold symlinks into it. Exercises the tree walking, which cannot be
    /// covered against the host's own sysfs in CI.
    fn synthetic_sysfs(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let dev = root.join("devices/pci0000:00/usb1/1-1");
        let iface0 = dev.join("1-1:1.0");
        let iface1 = dev.join("1-1:1.1");
        let hid = iface1.join("0003:17EF:61EB.000B");
        let drivers = root.join("drivers");

        for d in [&iface0, &iface1, &hid, &dev.join("power"), &drivers] {
            fs::create_dir_all(d).unwrap();
        }
        for name in ["xpad", "usbhid", "hid-lenovo-go"] {
            fs::create_dir_all(drivers.join(name)).unwrap();
        }

        for (name, value) in [
            ("idVendor", "17ef"),
            ("idProduct", "61eb"),
            ("bcdDevice", "0100"),
            ("product", "Legion Controller for Windows"),
            ("serial", "32869681"),
            ("bmAttributes", "80"),
        ] {
            fs::write(dev.join(name), format!("{}\n", value)).unwrap();
        }
        for (name, value) in [
            ("control", "auto"),
            ("runtime_status", "active"),
            ("autosuspend_delay_ms", "2000"),
        ] {
            fs::write(dev.join("power").join(name), format!("{}\n", value)).unwrap();
        }

        for (dir, num, class, driver) in [
            (&iface0, "00", "ff", "xpad"),
            (&iface1, "01", "03", "usbhid"),
        ] {
            fs::write(dir.join("bInterfaceNumber"), format!("{}\n", num)).unwrap();
            fs::write(dir.join("bInterfaceClass"), format!("{}\n", class)).unwrap();
            std::os::unix::fs::symlink(drivers.join(driver), dir.join("driver")).unwrap();
        }
        std::os::unix::fs::symlink(drivers.join("hid-lenovo-go"), hid.join("driver")).unwrap();

        // The bus roots hold symlinks, exactly as sysfs does.
        let usb_root = root.join("bus/usb/devices");
        let hid_root = root.join("bus/hid/devices");
        fs::create_dir_all(&usb_root).unwrap();
        fs::create_dir_all(&hid_root).unwrap();
        std::os::unix::fs::symlink(&dev, usb_root.join("1-1")).unwrap();
        std::os::unix::fs::symlink(&iface0, usb_root.join("1-1:1.0")).unwrap();
        std::os::unix::fs::symlink(&iface1, usb_root.join("1-1:1.1")).unwrap();
        std::os::unix::fs::symlink(&hid, hid_root.join("0003:17EF:61EB.000B")).unwrap();

        (usb_root, hid_root)
    }

    #[test]
    fn collection_walks_a_realistic_sysfs_layout() {
        let root = std::env::temp_dir().join(format!("deploytix-sysfs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (usb_root, hid_root) = synthetic_sysfs(&root);

        let found = collect_from(&usb_root, &hid_root, false);
        assert_eq!(found.len(), 1, "expected one controller, got {:?}", found);
        let device = &found[0];

        assert_eq!(device.sysfs_name, "1-1");
        assert_eq!(device.id(), "17ef:61eb");
        assert_eq!(device.revision(), "01.00");
        assert_eq!(device.serial.as_deref(), Some("32869681"));
        assert_eq!(device.bm_attributes, Some(0x80));
        assert_eq!(device.supports_remote_wakeup(), Some(false));
        assert_eq!(device.power_control.as_deref(), Some("auto"));
        assert_eq!(device.autosuspend_delay_ms.as_deref(), Some("2000"));

        // Interface directories are siblings in the bus root, not children
        // of the device — the walk must not miss them.
        assert_eq!(device.interfaces.len(), 2);
        assert_eq!(device.interfaces[0].driver.as_deref(), Some("xpad"));
        assert_eq!(device.interfaces[1].driver.as_deref(), Some("usbhid"));

        // HID children are matched by resolved path, not by vendor/product.
        assert_eq!(device.hid_children.len(), 1);
        assert_eq!(
            device.hid_children[0].driver.as_deref(),
            Some("hid-lenovo-go")
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// A vendor outside the known list must be hidden by default and shown
    /// under --all, so the default view stays readable on a docked machine.
    #[test]
    fn the_vendor_filter_gates_the_default_view() {
        let root = std::env::temp_dir().join(format!("deploytix-filter-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (usb_root, hid_root) = synthetic_sysfs(&root);
        fs::write(root.join("devices/pci0000:00/usb1/1-1/idVendor"), "1d6b\n").unwrap();

        assert!(collect_from(&usb_root, &hid_root, false).is_empty());
        assert_eq!(collect_from(&usb_root, &hid_root, true).len(), 1);

        let _ = fs::remove_dir_all(&root);
    }

    /// Collection must survive a sysfs path that does not exist, so the
    /// command still prints the host section on a non-Linux or sandboxed
    /// host instead of erroring.
    #[test]
    fn collection_tolerates_a_missing_sysfs_tree() {
        let found = collect_from(
            Path::new("/nonexistent/usb"),
            Path::new("/nonexistent/hid"),
            true,
        );
        assert!(found.is_empty());
    }
}
