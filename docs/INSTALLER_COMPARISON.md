# Deploytix: Linux Installer Comparison & Recommendations

A systematic comparison of Deploytix against established Linux installers,
with actionable recommendations derived from their strengths.

---

## Installers Compared

| Installer | Distro | Language | UI | Lines of Code (approx) |
|-----------|--------|----------|----|------------------------|
| **Deploytix** | Artix Linux | Rust | CLI wizard + egui GUI | ~7,300 |
| **Calamares** | Manjaro, KDE neon, EndeavourOS, etc. | C++/Qt + Python | GUI (Qt) | ~80,000+ |
| **Anaconda** | Fedora, RHEL, Rocky | Python + GTK | GUI + TUI + VNC | ~150,000+ |
| **Subiquity** | Ubuntu Server/Desktop | Python (+ Flutter frontend) | TUI + Flutter GUI | ~50,000+ |
| **archinstall** | Arch Linux | Python | TUI | ~15,000 |

---

## Feature Matrix

### Legend

- **Strong** -- feature is mature and well-tested
- **Present** -- feature exists but has gaps or is incomplete
- **Absent** -- feature is missing

| Feature | Deploytix | Calamares | Anaconda | Subiquity | archinstall |
|---------|-----------|-----------|----------|-----------|-------------|
| **Partitioning** | | | | | |
| Guided/Auto layout | Strong | Strong | Strong | Strong | Strong |
| Manual partitioning | Absent | Strong (KPMcore) | Strong (Blivet) | Present | Present |
| Resize existing partitions | Absent | Strong | Strong | Absent | Absent |
| LVM | Absent | Present | Strong | Strong | Present |
| Btrfs subvolumes | Absent | Present | Present | Absent | Present |
| ZFS | Absent | Absent | Absent | Present | Absent |
| Proportional sizing | Strong | Absent | Absent | Absent | Absent |
| **Encryption** | | | | | |
| LUKS full-disk | Strong | Strong | Strong | Strong | Present |
| Multi-volume LUKS | Strong | Absent | Absent | Absent | Absent |
| Encrypted /boot | Strong | Strong (GRUB) | Present | Absent | Present |
| Keyfile auto-unlock | Strong | Present | Present | Present | Absent |
| TPM-backed FDE | Absent | Absent | Absent | Strong (25.10) | Absent |
| **Init Systems** | | | | | |
| systemd | Absent | Strong | Strong | Strong | Strong |
| runit | Strong | Absent | Absent | Absent | Absent |
| OpenRC | Strong | Absent | Absent | Absent | Absent |
| s6 | Strong | Absent | Absent | Absent | Absent |
| dinit | Strong | Absent | Absent | Absent | Absent |
| **Bootloader** | | | | | |
| GRUB (UEFI) | Strong | Strong | Strong | Strong | Strong |
| GRUB (BIOS/Legacy) | Absent | Strong | Strong | Strong | Strong |
| systemd-boot | Present | Present | Present | Absent | Strong |
| Secure Boot / UKI | Absent | Absent | Present | Present | Present |
| **Desktop Environments** | | | | | |
| KDE Plasma | Strong | Strong | Strong | Strong | Strong |
| GNOME | Strong | Strong | Strong | Strong | Strong |
| XFCE | Strong | Strong | Present | Absent | Strong |
| Headless/Server | Strong | Present | Strong | Strong | Strong |
| **Network** | | | | | |
| WiFi config during install | Absent | Absent | Strong | Present | Present |
| Backend choice (iwd/NM) | Strong | Absent | Absent | Absent | Present |
| **Automation** | | | | | |
| Config file automation | Present (TOML) | Present (YAML) | Strong (Kickstart) | Strong (Autoinstall) | Present (JSON) |
| Unattended install | Present | Present | Strong | Strong | Present |
| Pre/post install scripts | Absent | Present | Strong | Strong | Absent |
| PXE/network boot deploy | Absent | Absent | Strong | Strong | Absent |
| **Quality/Safety** | | | | | |
| Dry-run mode | Strong | Absent | Absent | Present | Absent |
| Installation logging | Absent | Strong | Strong | Strong | Present |
| Test suite | Minimal (8 tests) | Present | Strong | Present | Present |
| CI/CD pipeline | Absent | Present | Strong | Strong | Present |
| **UI/UX** | | | | | |
| GUI installer | Present (egui) | Strong (Qt) | Strong (GTK) | Strong (Flutter) | Absent |
| TUI installer | Strong (dialoguer) | Absent | Present | Strong | Strong |
| Accessibility | Absent | Depends on DE | Present (Orca) | Depends on DE | Absent |
| Progress feedback | Present | Strong | Strong | Strong | Present |
| **Architecture** | | | | | |
| Modular/plugin system | Absent | Strong | Strong (D-Bus) | Present | Present |
| Portable single binary | Strong | Absent | Absent | Absent | Absent |
| Library API | Absent | Present | Absent | Present | Strong |

---

## What Deploytix Does Well (Unique Strengths)

### 1. Multi-init-system support

No other installer supports runit, OpenRC, s6, and dinit with full service
abstraction. This is Deploytix's primary differentiator and the reason it
exists. The `ServiceManager` trait cleanly abstracts init-specific operations,
and init-specific package naming (`grub-runit`, `iwd-openrc`, etc.) is handled
correctly throughout.

### 2. Multi-volume LUKS encryption

The `CryptoSubvolume` layout with independent LUKS containers per partition
(root, usr, var, home) is unique among all compared installers. Combined with
keyfile-based auto-unlock and custom mkinitcpio hooks (`crypttab-unlock`,
`mountcrypt`), this provides stronger security isolation than single-volume
encryption.

### 3. Proportional partition sizing

The weighted-ratio algorithm that scales partition sizes from 128 GiB to 2 TiB
drives is a genuinely good design. No other installer dynamically computes
partition ratios this way -- most use fixed sizes or simple percentages.

### 4. Portable single binary

The musl static build producing a single self-contained binary is ideal for
live USB/ISO distribution. No other installer achieves this level of
portability. Calamares requires Qt runtime, Anaconda requires Python + GTK,
archinstall requires Python.

### 5. Dry-run mode

Full dry-run simulation of the entire installation without touching disk is a
valuable safety and development feature. Only Subiquity offers something
comparable. Calamares and Anaconda have no equivalent.

---

## Gaps and Recommendations

The following recommendations are derived directly from capabilities present
in peer installers that Deploytix lacks. They are ordered by impact.

### HIGH PRIORITY -- Expected by users of any Linux installer

#### R1. Add installation log file output

**Gap:** All output goes to stdout/stderr. If installation fails mid-way, the
user has nothing to share for debugging.

**What peers do:**
- Anaconda writes 5 separate log files (`anaconda.log`, `storage.log`,
  `packaging.log`, `program.log`, `syslog`)
- Calamares writes `~/.cache/calamares/session.log` with configurable
  verbosity (levels 0-8) and a "toggle log" button in the UI
- Subiquity writes to `/var/log/installer/`, `/var/log/curtin/`,
  `/var/log/cloud-init*`

**Recommendation:** Add a `tracing` file appender that writes to
`/tmp/deploytix-<timestamp>.log`. Include all command invocations, their
stdout/stderr, and their exit codes. This is a small change in `main.rs` --
add a `tracing_subscriber::fmt::layer()` with a file writer to the existing
subscriber setup. The log path should be printed at the start and end of
installation.

#### R2. Add BIOS/Legacy boot support

**Gap:** Deploytix assumes UEFI exclusively. All four peer installers support
both UEFI and BIOS.

**What peers do:**
- Calamares, Anaconda, and archinstall all detect boot mode automatically
  and adjust partition layout (adding a BIOS boot partition) and GRUB
  install target accordingly
- archinstall checks `/sys/firmware/efi` at startup

**Recommendation:**
1. Detect boot mode: `Path::new("/sys/firmware/efi/efivars").exists()`
2. Add a `BootMode` enum (`Uefi`, `Bios`)
3. For BIOS: add a 1 MiB BIOS boot partition (type `21686148-6449-6E6F-744E-656564454649`), skip EFI partition
4. Adjust GRUB install: `grub-install --target=i386-pc /dev/sdX` for BIOS
5. Adjust layout computation to account for the different partition table

This removes a hard requirement that excludes older hardware and many VM
configurations.

#### R3. Add input validation for all user-supplied values

**Gap:** Hostnames, timezones, locales, keymaps, usernames, and passwords are
passed through without validation. (Also noted in IMPROVEMENTS.md #5.)

**What peers do:**
- Calamares validates hostnames (RFC 1123), checks timezone against
  zoneinfo, verifies locale against locale.gen
- Anaconda enforces password strength policies and username restrictions
- archinstall validates against system files

**Recommendation:** Implement a `validate()` method on `DeploymentConfig`
that runs before installation begins:
- Hostname: `^[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$`
- Username: `^[a-z_][a-z0-9_-]{0,31}$`
- Timezone: check `<chroot>/usr/share/zoneinfo/<value>` exists
- Locale: check value appears in `/etc/locale.gen`
- Keymap: check against available keymaps

#### R4. Implement installation rollback / cleanup on failure

**Gap:** If installation fails mid-way (e.g., during basestrap), the system is
left in a partially configured state with mounted filesystems and open LUKS
containers. The user must manually clean up.

**What peers do:**
- Anaconda tracks all operations and can revert partitioning changes on
  failure
- Subiquity's Curtin engine has transactional install stages
- Deploytix has a `cleanup` command, but it is not automatically invoked on
  failure

**Recommendation:** Wrap `Installer::run()` in a cleanup guard:
```rust
let result = installer.run().await;
if result.is_err() {
    tracing::error!("Installation failed, running cleanup...");
    cleanup::unmount_and_close(&config, &runner)?;
}
result
```
The existing `cleanup` module already handles unmounting and LUKS closure --
it just needs to be called automatically on failure paths.

### MEDIUM PRIORITY -- Significant quality-of-life improvements

#### R5. Add a CI/CD pipeline

**Gap:** No automated testing, linting, or build verification on commit.

**What peers do:**
- Anaconda has nightly kickstart test suites, extensive CI
- Calamares runs CI on every commit (formatting, linting, build)
- archinstall has GitHub Actions CI

**Recommendation:** Add `.github/workflows/ci.yml`:
```yaml
name: CI
on: [push, pull_request]
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: x86_64-unknown-linux-musl
          components: clippy, rustfmt
      - run: cargo fmt --check
      - run: cargo clippy -- -D warnings
      - run: cargo test
      - run: cargo build --release --target x86_64-unknown-linux-musl
```

#### R6. Add pre/post-install hook support

**Gap:** No mechanism for users to run custom scripts before or after
installation phases.

**What peers do:**
- Anaconda: `%pre` and `%post` sections in Kickstart files
- Subiquity: `early-commands` and `late-commands` in Autoinstall YAML
- Calamares: `shellprocess` module for arbitrary commands at any pipeline
  stage

**Recommendation:** Add optional fields to the TOML config:
```toml
[hooks]
pre_install = ["/path/to/script.sh"]
post_basestrap = ["/path/to/another.sh"]
post_configure = []
post_install = ["/path/to/finalize.sh"]
```
Execute each script list at the corresponding phase boundary in
`Installer::run()`. Scripts run in the host context; use
`artix-chroot <mountpoint> <cmd>` for chroot execution.

#### R7. Expand the test suite

**Gap:** 8 unit tests across ~7,300 lines of code.

**What peers do:**
- Anaconda has hundreds of unit tests + nightly integration tests
- archinstall has unit tests for core functions
- Calamares tests each module independently

**Recommendation:** Priority test targets (no root or real disk required):

| Module | What to test | Estimated tests |
|--------|-------------|----------------|
| `disk/layouts.rs` | `compute_layout` for all layout types at various disk sizes (128G, 256G, 512G, 1T, 2T) | 15-20 |
| `disk/partitioning.rs` | sfdisk script generation correctness | 5-10 |
| `config/deployment.rs` | TOML round-trip, default values, validation | 10-15 |
| `install/fstab.rs` | Correct UUIDs, mount options, ordering | 5-8 |
| `configure/services.rs` | Service enablement for all 4 init systems | 8-12 |
| `configure/bootloader.rs` | GRUB config generation, kernel param construction | 5-8 |

Target: 50+ unit tests covering all pure-function logic.

#### R8. Add progress reporting with structured stages

**Gap:** The CLI shows log lines but no structured progress. The GUI has a
progress callback but the CLI does not consume it effectively.

**What peers do:**
- Calamares shows a progress bar with percentage and current module name
- Anaconda shows a hub with spoke completion status
- Subiquity shows step-by-step progress with spinners

**Recommendation:** Use the existing `indicatif` dependency (currently unused)
to display:
1. A multi-progress bar showing overall phase (1/6 Partitioning, 2/6
   Base System, etc.)
2. A spinner for the currently running command
3. Elapsed time per phase

Wire this to the existing `ProgressCallback` in `Installer`.

#### R9. Support Btrfs subvolumes as a layout option

**Gap:** Deploytix uses Btrfs as a filesystem but creates traditional
partitions, not subvolumes. Btrfs subvolumes enable snapshots and rollback.

**What peers do:**
- archinstall creates `@`, `@home`, `@log`, `@cache` subvolumes by default
  when Btrfs is selected
- Calamares supports Btrfs subvolume configuration
- openSUSE/YaST creates subvolumes with snapshot rollback via snapper

**Recommendation:** Add a `BtrfsSubvolume` layout variant that:
1. Creates a single Btrfs partition (plus EFI and swap)
2. Creates subvolumes: `@` (root), `@home`, `@var`, `@log`, `@snapshots`
3. Mounts with `subvol=@,compress=zstd` options
4. Generates correct fstab entries with subvolume mount options
5. Optionally integrates with `snapper` for automatic snapshots

This is the modern standard for Btrfs-based systems and enables system
rollback -- a significant reliability improvement.

### LOWER PRIORITY -- Nice-to-have features

#### R10. Add LVM support

**Gap:** No LVM support. Deploytix uses raw partitions only.

**What peers do:**
- Anaconda defaults to LVM for guided partitioning
- Subiquity uses LVM as its default guided layout
- Calamares has full LVM support

**Recommendation:** Add an LVM layout variant that creates a volume group
spanning one or more physical volumes, with logical volumes for root, home,
var, and swap. LVM provides online resize capability and pairs well with LUKS
(LUKS-on-LVM or LVM-on-LUKS patterns).

#### R11. Add Secure Boot / Unified Kernel Image (UKI) support

**Gap:** No Secure Boot support. UKI is the emerging standard.

**What peers do:**
- Anaconda supports Secure Boot out of the box
- archinstall documents UKI setup for Secure Boot
- Subiquity supports Secure Boot with signed kernels

**Recommendation:** This is architecturally significant. For initial support:
1. Detect Secure Boot status via `/sys/firmware/efi/efivars/SecureBoot-*`
2. Install signed GRUB and shim packages
3. For UKI: generate unified kernel images with `ukify` and install to the
   ESP directly, bypassing GRUB entirely

#### R12. Add a plugin/module system

**Gap:** All installer behavior is compiled into the binary. Distro
maintainers who want to customize must fork and modify Rust code.

**What peers do:**
- Calamares: Python/C++ modules loaded from config -- the primary reason it
  is adopted by 50+ distros
- Anaconda: D-Bus modules as independent processes + Python spoke addons
- archinstall: `--plugin <url>` loads remote Python plugins

**Recommendation:** For a Rust binary, a practical approach is:
1. Shell-script hooks at phase boundaries (see R6) for simple customization
2. TOML-driven configuration for package lists, service lists, and locale
   defaults (already partially implemented)
3. Long-term: WASM plugin support via `wasmtime` for safe, sandboxed
   extensions

#### R13. Add accessibility support

**Gap:** No screen reader integration, no high-contrast mode, no keyboard-only
navigation guarantees.

**What peers do:**
- Anaconda (Workstation): Orca screen reader available during install
- Calamares: inherits desktop accessibility stack
- All installers: generally poor accessibility

**Recommendation:** For the TUI:
1. Ensure all prompts are screen-reader-friendly (linear text, no visual-only
   indicators)
2. Replace emoji status indicators (✓, ⚠️, 🚀) with text equivalents when
   a `--accessible` flag is set

For the GUI (egui):
1. egui has limited accessibility support currently
2. Monitor egui's accessibility roadmap; consider AccessKit integration when
   mature

#### R14. Add WiFi configuration during installation

**Gap:** Network configuration only sets up the post-install backend (iwd or
NetworkManager). There is no way to connect to WiFi during the installation
itself, which is required if basestrap needs to download packages.

**What peers do:**
- Anaconda has a full network configuration spoke
- archinstall can copy WiFi credentials from the live session
- Subiquity has network configuration screens

**Recommendation:** Add an optional `--wifi` flag or config section that:
1. Scans for available networks via `iwctl station wlan0 scan`
2. Prompts for SSID and passphrase
3. Connects before basestrap begins
4. Copies the credentials to the installed system

---

## Architectural Comparison

### Deploytix: Monolithic orchestrator

```
CLI/GUI → DeploymentConfig → Installer::run() → sequential phases
                                                  ├── disk ops
                                                  ├── basestrap
                                                  ├── configure (chroot)
                                                  ├── desktop
                                                  └── finalize
```

**Pros:** Simple, predictable, easy to reason about, fast compilation.
**Cons:** Not extensible without code changes. No plugin system.

### Calamares: Module pipeline

```
settings.conf → [module₁] → [module₂] → ... → [moduleₙ]
                 (SHOW)      (EXEC)             (EXEC)
```

**Pros:** Highly customizable. Distros swap modules without patching core.
**Cons:** Complex configuration. Module interactions can be fragile.

### Anaconda: Hub-and-spoke with D-Bus modules

```
Hub ←→ Spoke₁ (Storage D-Bus module)
   ←→ Spoke₂ (Network D-Bus module)
   ←→ Spoke₃ (Users D-Bus module)
   ...
```

**Pros:** Non-linear navigation. Modules are independent processes.
**Cons:** Very complex. D-Bus adds overhead and debugging difficulty.

### Subiquity: Three-layer stack

```
Flutter/TUI frontend → Subiquity API → Curtin engine → cloud-init
```

**Pros:** Clean separation. Multiple frontends share one backend.
**Cons:** Three moving parts. Complex debugging across layers.

### archinstall: Library + scripts

```
archinstall library → guided.py / minimal.py / custom scripts
```

**Pros:** Importable as a Python library. Easy scripting.
**Cons:** No formal plugin system. TUI only.

### Recommendation for Deploytix

Deploytix's monolithic approach is appropriate for its current scope and
single-distro focus. The two changes that would provide the most
architectural value are:

1. **Phase hooks** (R6) -- enables customization without code changes
2. **Separating the config/validation layer from the execution layer** --
   the `DeploymentConfig` + `Installer` split already exists; formalizing
   it as a stable API would allow other tools to drive Deploytix as a
   library (similar to archinstall's approach)

A full module/plugin system (R12) is not justified at current scale.

---

## Automation Format Comparison

| Format | Installer | Syntax | Maturity | Mass Deploy |
|--------|-----------|--------|----------|-------------|
| TOML config | Deploytix | TOML | Early | No |
| Kickstart | Anaconda | Custom DSL | 20+ years | Yes (PXE) |
| Autoinstall | Subiquity | YAML | 5+ years | Yes (PXE/MAAS) |
| Module configs | Calamares | YAML | 8+ years | No |
| JSON config | archinstall | JSON | 3+ years | No |

Deploytix's TOML format covers the basics but lacks:
- Pre/post script sections (R6)
- Package list overrides (let users add/remove packages from defaults)
- Conditional logic (e.g., "if UEFI then X, else Y")
- Config generation from a completed install (Anaconda auto-generates
  `anaconda-ks.cfg` after every install)

**Recommendation:** Add a `generate-config --from-install` command that
captures the effective configuration after a successful installation. This
enables the "install once, replay many" workflow that makes Kickstart and
Autoinstall so valuable for fleet deployment.

---

## Summary of Recommendations

| ID | Recommendation | Priority | Effort | Inspired By |
|----|---------------|----------|--------|-------------|
| R1 | Installation log file | High | Small | Anaconda, Calamares, Subiquity |
| R2 | BIOS/Legacy boot support | High | Medium | All peers |
| R3 | Input validation | High | Small | Calamares, Anaconda |
| R4 | Auto-cleanup on failure | High | Small | Anaconda, Subiquity |
| R5 | CI/CD pipeline | Medium | Small | All peers |
| R6 | Pre/post-install hooks | Medium | Medium | Anaconda, Subiquity, Calamares |
| R7 | Expanded test suite | Medium | Medium | Anaconda |
| R8 | Structured progress reporting | Medium | Small | Calamares, Anaconda |
| R9 | Btrfs subvolume layout | Medium | Medium | archinstall, openSUSE |
| R10 | LVM support | Lower | Large | Anaconda, Subiquity |
| R11 | Secure Boot / UKI | Lower | Large | Anaconda, archinstall |
| R12 | Plugin/module system | Lower | Large | Calamares |
| R13 | Accessibility | Lower | Medium | Anaconda |
| R14 | WiFi during installation | Lower | Medium | Anaconda, archinstall |

### Recommended implementation order

**Phase 1 (foundations):** R1, R3, R4, R5 -- logging, validation, safety,
and CI. These are table-stakes for any installer and have small
implementation cost.

**Phase 2 (hardware coverage):** R2, R8 -- BIOS support removes a hard
blocker for many users; progress reporting improves the installation
experience.

**Phase 3 (power features):** R6, R7, R9 -- hooks, tests, and Btrfs
subvolumes add significant capability and reliability.

**Phase 4 (advanced):** R10, R11, R12, R13, R14 -- LVM, Secure Boot,
plugins, accessibility, and WiFi are valuable but architecturally
significant undertakings.
