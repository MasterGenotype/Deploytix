# Plan: Suricata IDS/IPS Automation for Artix Linux

## Overview

Add a new optional package collection to Deploytix that automates building, installing, and configuring Suricata IDS/IPS on Artix Linux deployments. This follows the existing patterns established by GPU drivers, Wine, Gaming, and yay package collections.

Suricata is not available in official Artix repos — it's AUR-only (per Suricata's own Arch-based docs). Since the project already has a `yay` AUR helper integration, Suricata will be installed via yay from the AUR. The automation covers: AUR package installation, init-system service enablement, YAML configuration generation, rules management (`suricata-update`), and NIC tuning.

---

## Step 1: Extend `PackagesConfig` with Suricata options

**File:** `src/config/deployment.rs`

Add new fields to `PackagesConfig`:

```rust
/// Install Suricata IDS/IPS. Requires: install_yay = true (AUR package).
#[serde(default)]
pub install_suricata: bool,

/// Suricata capture mode.
#[serde(default)]
pub suricata_mode: SuricataMode,

/// Network interface for Suricata to monitor.
#[serde(default)]
pub suricata_interface: Option<String>,

/// Secondary interface for AF_PACKET inline IPS bridge mode.
#[serde(default)]
pub suricata_bridge_interface: Option<String>,
```

Add a new enum:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SuricataMode {
    #[default]
    AfPacketIds,       // AF_PACKET passive IDS (recommended baseline)
    AfPacketInline,    // AF_PACKET L2 inline IPS (copy-mode: ips, two interfaces)
    Nfqueue,           // NFQUEUE L3 inline IPS (netfilter integration)
}

impl std::fmt::Display for SuricataMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AfPacketIds => write!(f, "af_packet_ids"),
            Self::AfPacketInline => write!(f, "af_packet_inline"),
            Self::Nfqueue => write!(f, "nfqueue"),
        }
    }
}
```

Add validation in `DeploymentConfig::validate()`:
- `install_suricata` requires `install_yay = true`
- `AfPacketInline` requires both `suricata_interface` and `suricata_bridge_interface`
- `suricata_interface` required when `install_suricata = true`

Add interactive prompt in `DeploymentConfig::interactive()` (gated behind `install_yay`):

```rust
let install_suricata = if install_yay {
    prompt_confirm("Install Suricata IDS/IPS?", false)?
} else {
    false
};

let (suricata_mode, suricata_interface, suricata_bridge_interface) = if install_suricata {
    let mode_idx = prompt_select(
        "Suricata capture mode",
        &["AF_PACKET IDS (passive, recommended)", "AF_PACKET Inline IPS (L2 bridge)", "NFQUEUE IPS (L3 netfilter)"],
        0,
    )?;
    let mode = match mode_idx {
        1 => SuricataMode::AfPacketInline,
        2 => SuricataMode::Nfqueue,
        _ => SuricataMode::AfPacketIds,
    };
    let iface = prompt_input("Monitor interface", "eth0")?;
    let bridge_iface = if mode == SuricataMode::AfPacketInline {
        Some(prompt_input("Bridge output interface", "eth1")?)
    } else {
        None
    };
    (mode, Some(iface), bridge_iface)
} else {
    (SuricataMode::default(), None, None)
};
```

**TOML example:**

```toml
[packages]
install_yay = true
install_suricata = true
suricata_mode = "af_packet_ids"
suricata_interface = "eth0"
```

---

## Step 2: Create `src/configure/suricata.rs` module

**File:** `src/configure/suricata.rs` (new)

This is the main implementation module. It contains:

### 2a. Package installation via yay

```rust
const SURICATA_AUR_PACKAGE: &str = "suricata";

const SURICATA_DEPS: &[&str] = &[
    "libpcap", "libyaml", "pcre2", "jansson", "zlib",
    "rust", "cargo", "cbindgen",
    "libnetfilter_queue",  // for NFQUEUE support
    "hwloc",               // CPU affinity autopin
];
```

Function `install_suricata()` — the top-level entry point:
1. Install build dependencies from official repos via `pacman -S --noconfirm --needed`
2. Install `suricata` from AUR via `sudo -u {user} yay -S --noconfirm --needed suricata`
3. Create suricata system user/group
4. Create required directories (`/var/lib/suricata/rules`, `/var/log/suricata`, `/var/run/suricata`)
5. Generate `suricata.yaml` based on mode/interface config
6. Generate NIC tuning script
7. Generate and install init-system service files
8. Enable the suricata service
9. Initialize rules via `suricata-update`
10. Validate config with `suricata -T`

### 2b. Generate `suricata.yaml` configuration

Function `generate_suricata_yaml()` returns a `String` with the full YAML config based on `SuricataMode`:

**AF_PACKET IDS mode:**
```yaml
%YAML 1.1
---

vars:
  address-groups:
    HOME_NET: "[192.168.0.0/16,10.0.0.0/8,172.16.0.0/12]"
    EXTERNAL_NET: "!$HOME_NET"
  port-groups:
    HTTP_PORTS: "80"
    SHELLCODE_PORTS: "!80"
    SSH_PORTS: "22"

runmode: autofp
max-pending-packets: 8192
mpm-algo: auto
spm-algo: auto

af-packet:
  - interface: {interface}
    threads: auto
    cluster-id: 99
    cluster-type: cluster_flow
    tpacket-v3: yes
    mmap-locked: yes
    ring-size: 100000
    block-size: 1048576
    defrag: yes

outputs:
  - eve-log:
      enabled: yes
      filetype: regular
      filename: /var/log/suricata/eve.json
      community-id: true
      types:
        - alert:
            tagged-packets: yes
        - anomaly:
            enabled: yes
        - http:
            extended: yes
        - dns
        - tls:
            extended: yes
        - flow
        - stats:
            totals: yes
            threads: no

  - fast:
      enabled: yes
      filename: /var/log/suricata/fast.log

  - stats:
      enabled: yes
      filename: /var/log/suricata/stats.log
      append: yes
      totals: yes
      threads: no

logging:
  default-log-level: notice
  outputs:
    - console:
        enabled: yes
    - file:
        enabled: yes
        filename: /var/log/suricata/suricata.log
        level: info

default-rule-path: /var/lib/suricata/rules
rule-files:
  - suricata.rules

run-as:
  user: suricata
  group: suricata

detect:
  sgh-mpm-caching:
    enabled: yes
    path: /var/lib/suricata/mpm-cache
```

**AF_PACKET Inline mode:** Same base but with two interface stanzas using `copy-mode: ips` and `copy-iface` pointing at each other:

```yaml
af-packet:
  - interface: {interface}
    copy-mode: ips
    copy-iface: {bridge_interface}
    cluster-id: 99
    cluster-type: cluster_flow
    tpacket-v3: yes
    ring-size: 100000

  - interface: {bridge_interface}
    copy-mode: ips
    copy-iface: {interface}
    cluster-id: 99
    cluster-type: cluster_flow
    tpacket-v3: yes
    ring-size: 100000
```

**NFQUEUE mode:** Replace `af-packet` section with:

```yaml
nfq:
  mode: accept
  repeat-mark: 1
  repeat-mask: 1
```

### 2c. System user creation

```rust
fn create_suricata_user(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    cmd.run_in_chroot(install_root,
        "getent group suricata >/dev/null 2>&1 || groupadd -r suricata")?;
    cmd.run_in_chroot(install_root,
        "id suricata >/dev/null 2>&1 || useradd -r -M -s /usr/bin/nologin -g suricata -d /var/lib/suricata suricata")?;
    Ok(())
}
```

### 2d. Directory setup

```rust
fn setup_directories(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    let dirs = [
        "/var/lib/suricata/rules",
        "/var/lib/suricata/update",
        "/var/lib/suricata/mpm-cache",
        "/var/log/suricata",
        "/var/run/suricata",
    ];
    for dir in &dirs {
        cmd.run_in_chroot(install_root, &format!("install -d -m 0750 -o suricata -g suricata {}", dir))?;
    }
    Ok(())
}
```

### 2e. Rules initialization

```rust
fn initialize_rules(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    cmd.run_in_chroot(install_root, "suricata-update")?;
    Ok(())
}
```

### 2f. NIC offload tuning script

Generate `/usr/local/bin/suricata-nic-setup.sh`:

```bash
#!/bin/bash
# Disable NIC offloads that break Suricata IDS correctness
# GRO/LRO merge packets into "super packets" breaking rule semantics
# and TCP state tracking.
IFACE="${1}"
[ -z "$IFACE" ] && exit 0
ethtool -K "$IFACE" gro off lro off tso off 2>/dev/null || true
ethtool -K "$IFACE" rx-checksumming off tx-checksumming off 2>/dev/null || true
```

### 2g. Configuration validation

```rust
fn validate_config(cmd: &CommandRunner, install_root: &str) -> Result<()> {
    cmd.run_in_chroot(install_root, "suricata -T -c /etc/suricata/suricata.yaml")?;
    Ok(())
}
```

---

## Step 3: Init system service integration

**File:** `src/configure/suricata.rs` (same module)

Generate init-system-appropriate service files. The AUR `suricata` package ships systemd units only, so we must generate our own for Artix init systems.

Function `install_suricata_service()` that dispatches based on `InitSystem`:

### OpenRC (`/etc/init.d/suricata`)

```bash
#!/sbin/openrc-run

name="suricata"
description="Suricata IDS/IPS"

SURICATA_CONF="${SURICATA_CONF:-/etc/suricata/suricata.yaml}"
SURICATA_IFACE="{interface}"

command="/usr/bin/suricata"
command_args="-c ${SURICATA_CONF} {capture_args}"
command_background="yes"
pidfile="/run/${name}.pid"
command_args_background="--pidfile ${pidfile}"

depend() {
    need net
    after firewall
}

start_pre() {
    /usr/local/bin/suricata-nic-setup.sh "${SURICATA_IFACE}"
    checkpath --directory --owner suricata:suricata --mode 0750 /run/suricata
}

stop_post() {
    rm -f "${pidfile}"
}
```

Where `{capture_args}` is:
- AF_PACKET IDS: `--af-packet`
- AF_PACKET Inline: `--af-packet`
- NFQUEUE: `-q 0`

### runit (`/etc/runit/sv/suricata/run`)

```bash
#!/bin/sh
exec 2>&1
/usr/local/bin/suricata-nic-setup.sh {interface}
mkdir -p /run/suricata
chown suricata:suricata /run/suricata
exec /usr/bin/suricata -c /etc/suricata/suricata.yaml {capture_args}
```

Plus a `log/run` for svlogd:

```bash
#!/bin/sh
exec svlogd -tt /var/log/suricata/runit/
```

### s6 (`/etc/s6/sv/suricata-srv/`)

Create the s6 service directory structure:
- `run` — main exec script
- `type` — `longrun`
- `dependencies.d/` — dependency directory
- `log/` subdirectory with its own `run` for s6-log

### dinit (`/etc/dinit.d/suricata`)

```ini
type = process
command = /usr/bin/suricata -c /etc/suricata/suricata.yaml {capture_args}
run-as = root
logfile = /var/log/suricata/suricata.log
depends-on = net
smooth-recovery = true
restart = true
```

After creating the service files, enable the service using the existing `enable_service()` function from `services.rs` (runit: symlink in runsvdir/default, OpenRC: rc-update add, s6: touch in contents.d, dinit: symlink in boot.d).

---

## Step 4: Wire into the installer pipeline

**File:** `src/install/installer.rs`

Add a new phase after AUR packages (Phase 5.36) and before btrfs tools:

```rust
// Phase 5.36: Suricata IDS/IPS (after yay, needs AUR)
if self.config.packages.install_suricata {
    self.report_progress(0.876, "Installing and configuring Suricata IDS/IPS...");
    self.install_suricata()?;
}
```

Add the installer method:

```rust
/// Install and configure Suricata IDS/IPS
fn install_suricata(&self) -> Result<()> {
    info!("Installing and configuring Suricata IDS/IPS");
    configure::suricata::install_suricata(&self.cmd, &self.config, INSTALL_ROOT)
}
```

---

## Step 5: Register the module

**File:** `src/configure/mod.rs`

Add:

```rust
pub mod suricata;
```

---

## Step 6: NFQUEUE firewall rules (conditional)

**File:** `src/configure/suricata.rs`

For NFQUEUE mode only, generate `/usr/local/bin/suricata-nfqueue-setup.sh`:

```bash
#!/bin/bash
# Enable NFQUEUE rules for Suricata IPS
# Run this manually or integrate with your firewall service
set -e

nft add table inet suricata_ips
nft add chain inet suricata_ips forward '{ type filter hook forward priority 0 ; policy accept ; }'
nft add rule inet suricata_ips forward queue num 0 bypass
echo "NFQUEUE rules active: forwarded traffic sent to Suricata queue 0"
```

And a teardown script `/usr/local/bin/suricata-nfqueue-teardown.sh`:

```bash
#!/bin/bash
nft delete table inet suricata_ips 2>/dev/null || true
echo "NFQUEUE rules removed"
```

These are generated as helper scripts rather than auto-enabled, since NFQUEUE inline enforcement requires careful operator validation before going live.

---

## File change summary

| File | Action | Description |
|------|--------|-------------|
| `src/config/deployment.rs` | Edit | Add `SuricataMode` enum, extend `PackagesConfig`, add validation, add interactive prompts |
| `src/configure/mod.rs` | Edit | Add `pub mod suricata;` |
| `src/configure/suricata.rs` | **Create** | Main Suricata module: install, configure YAML, service files, rules, NIC tuning, validation |
| `src/install/installer.rs` | Edit | Add Phase 5.36 Suricata installation step + method |

---

## Design decisions

1. **AUR via yay** — Suricata is AUR-only on Arch-based systems per Suricata's own docs. This matches the existing yay/AUR pattern in the codebase (`install_aur_packages`, `install_btrfs_tools`).

2. **Custom init scripts** — The AUR package ships systemd units only. We generate init-specific service files matching the exact patterns used by Artix packages for each supported init system (runit/OpenRC/s6/dinit). This mirrors how the codebase already handles service enablement.

3. **AF_PACKET IDS as default** — Safest baseline mode. Doesn't require firewall integration, works passively. Users can opt into inline IPS modes explicitly.

4. **Generated YAML, not templated** — Write the complete `suricata.yaml` programmatically (as a Rust string) rather than shipping a template. This follows the pattern used for greetd config generation in `src/configure/greetd.rs`.

5. **Build deps from pacman, package from AUR** — Install known build dependencies from official repos first (fast, reliable), then build the AUR package which will find them already present. This reduces AUR build time and failure modes.

6. **NIC tuning script** — Offload disabling (GRO/LRO/TSO) is critical for IDS correctness per Suricata's own performance guidance. A separate script allows re-running after reboot via the init service's pre-start hook.

7. **suricata-update for rules** — Official rules management tool. Fetches ET/Open by default, which is sufficient for most deployments. More sources can be added post-install.

8. **Privilege dropping** — Create a `suricata` system user and configure `run-as` in the YAML. Suricata binds the capture interface as root, then drops to the unprivileged user.

9. **NFQUEUE scripts as opt-in** — NFQUEUE firewall rules are generated as helper scripts rather than auto-enabled, since inline IPS enforcement requires careful operator validation before going live. This avoids accidentally blocking traffic.

10. **Hyperscan auto-detection** — We don't explicitly install Hyperscan (it may not be in Artix repos), but configure `mpm-algo: auto` so Suricata uses it if available. The AUR build may include it depending on the PKGBUILD.
