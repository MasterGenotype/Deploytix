# Handheld Controller Quirks

Fixes the Lenovo Legion Go family failure mode where the controllers
repeatedly disconnect and reconnect: the pads vanish and reappear in Steam
every few seconds, input drops mid-game, and `dmesg` fills with
`usb …: USB disconnect` immediately followed by a fresh enumeration of the
same device.

Reported on the **Legion Go 2** in particular, where the three causes below
compound. The Legion Go and Legion Go S share enough of the same hardware and
driver situation that they get the same treatment.

## What Deploytix installs

One udev rule file on the target system:

```
/etc/udev/rules.d/60-deploytix-handheld-controllers.rules
```

Every rule matches vendor `17ef` (Lenovo) with product `61??`. The Legion
controllers enumerate across `0x6182`, `0x6183`, `0x6184`, `0x6185` and
`0x61eb`, and the Legion Go 2 adds further IDs in the same range — matching
the range rather than a fixed list means new SKUs and firmware revisions are
covered without a table to keep current.

### 1. USB runtime power management pinned off

```
SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="17ef", ATTR{idProduct}=="61??", TEST=="power/control", ATTR{power/control}="on"
SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="17ef", ATTR{idProduct}=="61??", TEST=="power/autosuspend_delay_ms", ATTR{power/autosuspend_delay_ms}="-1"
```

The controllers hang off an internal USB hub. At the kernel default
(`power/control = auto`) the link is runtime-suspended as soon as the pad
goes briefly idle, and the controller firmware re-enumerates instead of
resuming — which userspace sees as a disconnect immediately followed by a
reconnect.

Holding `power/control` at `on` keeps the port awake. Runtime-PM references
propagate to the parent, so this also stops the internal hub suspending
underneath the controllers; there is no separate rule for the hub.

This is deliberately narrower than the `usbcore.autosuspend=-1` kernel
parameter often suggested for this symptom: that disables runtime PM for
every USB device on the machine and costs idle battery life on a handheld.

### 2. `xpad` binding on older kernels

```
SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="17ef", ATTR{idProduct}=="61??", RUN+="/bin/sh -c '/usr/bin/modprobe xpad; echo 17ef %s{idProduct} > /sys/bus/usb/drivers/xpad/new_id 2>/dev/null || true'"
```

The Legion Go controller IDs landed in `xpad` upstream. With no `xpad` match
the vendor-specific interface falls through to `hid-generic`, which misreads
the report descriptor and makes the pad flap between present and gone.

**On a kernel that already knows the ID this rule does nothing.** An observed
Legion Go 2 binds `xpad` to interface 0 natively (class `ff`, subclass `5d`,
protocol `01` — the Xbox 360 signature), so the rule is a no-op there. It
earns its place only on kernels predating the entry.

Writing the ID to `xpad`'s `new_id` is a no-op when the running kernel
already knows it — the write fails with `EEXIST`, which the `|| true`
swallows. A device already bound to another driver is not stolen, so this
cannot regress a kernel that handles the pad correctly. Handheld Daemon
ships the same `new_id` trick for the MSI Claw, TECNO Pocket Go and Legion
Go S; this extends it to the Legion Go and Legion Go 2.

### 3. hidraw access for the session user

```
KERNEL=="hidraw*", SUBSYSTEM=="hidraw", ATTRS{idVendor}=="17ef", ATTRS{idProduct}=="61??", MODE="0660", GROUP="input", TAG+="uaccess"
```

Handheld Daemon and Steam both open the raw HID interface. Upstream HHD's
`83-hhd-user.rules` grants access only to product IDs matching `618*`, which
misses `0x61eb` and the Legion Go 2 range. A daemon that cannot open the node
retries in a loop, tearing down and rebuilding its emulated pad on every
attempt — which Steam reports as yet another disconnect/reconnect cycle.

## When it is applied

`packages.handheld_controller_quirks` is a tri-state:

| Value | Behaviour |
|-------|-----------|
| *(absent — the default)* | **Auto.** Rules are written only when the installing host's DMI identifies it as a Legion Go family machine. |
| `true` | Always write the rules. |
| `false` | Never write the rules. |

Auto reads `/sys/devices/virtual/dmi/id/product_name` and matches it against
the machine types below — the same file and the same identifiers Handheld
Daemon keys its own device detection off.

| DMI `product_name` | Model |
|--------------------|-------|
| `83E1` | Legion Go |
| `83N0`, `83N1` | Legion Go 2 |
| `83L3`, `83N6`, `83Q2`, `83Q3` | Legion Go S |

Detection reads the machine Deploytix is *running on*. That is the target
device in the normal case — booted from live media on the handheld itself —
but not when deploying to removable media from a desktop. Set the flag to
`true` explicitly for that case:

```toml
[packages]
handheld_controller_quirks = true
```

The rules are inert on hardware that has no `17ef:61xx` device, so forcing
them on is harmless if the media is later booted elsewhere.

Both the interactive wizard and the GUI's *Handheld Gaming* panel offer the
setting with the detected hardware as the default, so a Legion Go family
machine picks it up without the user needing to know the flag exists.

## Verifying on the target

After first boot on the handheld:

```bash
deploytix controllers
```

That reports, for every attached pad: its `vendor:product`, `bcdDevice`,
serial, whether the device declares remote-wakeup capability, its runtime
power state, the driver bound to each USB interface, and the driver bound to
each HID child — plus whether this rule file is installed. It reads sysfs
directly, so it needs neither root nor `usbutils`. Add `--all` to widen the
report from known controller vendors to every USB device.

### Reading the output

The pairing that causes the flapping is **`remote wakeup: NOT supported`**
together with **`control=auto`** — a device that cannot signal a resume, left
eligible for runtime suspend. The report flags that combination explicitly.
With this rule file in place `control` reads `on` instead, and the note
changes to say the device will not be suspended.

An observed Legion Go 2 pad reports `bmAttributes=0x80`: bus-powered, bit 5
clear, so no remote-wakeup capability. That is the direct evidence for rule 1.

The report also flags a **split across HID drivers** — some children on
`hid-generic` while others hold a vendor driver. That is not automatically a
fault: a driver may probe an interface and decline it on purpose. Only the
kernel log separates that from a missing match entry:

```bash
sudo dmesg | grep -i 'hid\|xpad\|17ef'
```

### Raw checks

```bash
# udev accepted the whole file (no output means it parsed cleanly).
udevadm verify /etc/udev/rules.d/60-deploytix-handheld-controllers.rules

# Everything usb-devices knows, if it is installed.
usb-devices | grep -B3 -A9 61eb

# Watch for flapping live.
sudo dmesg -w | grep -i 'usb\|xpad'
```

## Telling the two generations apart

The Legion Go 2 controller enumerates as **`17ef:61eb`** with the product
string "Legion Controller for Windows" — the same identity the Legion Go 1
controller reports. Vendor and product ID therefore **cannot** distinguish
the two generations, and neither can any udev rule or driver match table
keyed on them.

`bcdDevice` is the remaining descriptor field that could differ, which is why
`deploytix controllers` prints it prominently. An observed Legion Go 2 pad
reports `01.00`. If a Legion Go 1 pad also reports `01.00`, then nothing in
the USB descriptors separates them, and generation-specific handling would
have to come from the host's DMI or from probing the vendor protocol in
userspace.

## Source

`src/configure/handheld_quirks.rs`. The rules are applied in installer phase
5.68, ahead of Handheld Daemon (phase 5.7) — they govern how the controllers
bind and whether their hidraw nodes are reachable, which is what HHD builds
its emulated pad on top of.
