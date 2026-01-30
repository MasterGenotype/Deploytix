# Deploytix Project Overview

## What is Deploytix?

Deploytix is a portable Rust CLI application for **automated deployment of Artix Linux** to removable media and disks. It aims to provide a streamlined, configuration-driven approach to installing Artix Linux with support for multiple init systems, filesystems, and desktop environments.

### Key Characteristics

- **Single Static Binary**: Built with musl for full portability—no external dependencies required
- **Configuration-Driven**: TOML-based configuration for reproducible, unattended installations
- **Modular Architecture**: Clean separation between disk operations, system configuration, and desktop setup
- **Dry-Run Support**: Preview all operations before making destructive changes

## Current Capabilities

### Supported Options

| Category | Options |
|----------|---------|
| **Init Systems** | runit, OpenRC, s6, dinit |
| **Filesystems** | ext4, btrfs, xfs, f2fs |
| **Bootloaders** | GRUB, systemd-boot |
| **Desktop Environments** | KDE Plasma, GNOME, XFCE, headless/server |
| **Network Backends** | iwd, NetworkManager, ConnMan |
| **DNS** | dnscrypt-proxy (optional) |

### Partition Layouts

- **Standard (7-partition)**: EFI, boot, swap, root, /usr, /var, /home
- **Minimal (3-partition)**: EFI, swap, root

### Installation Workflow

1. Disk detection and selection
2. Partition table creation (GPT)
3. Filesystem formatting
4. Base system installation via `basestrap`
5. System configuration (locale, timezone, users)
6. Bootloader installation
7. Desktop environment setup
8. Service enablement

## Project Architecture

```
src/
├── main.rs              # CLI entry point (clap-based)
├── lib.rs               # Module exports
├── config/              # Configuration parsing and validation
├── disk/                # Disk detection, partitioning, formatting
├── install/             # basestrap, chroot, fstab generation
├── configure/           # System config: bootloader, users, network, mkinitcpio
├── desktop/             # DE-specific package lists and setup
├── cleanup/             # Unmount and cleanup operations
├── resources/           # Embedded resources
└── utils/               # Command execution, error handling, prompts
```

## Goals to Accomplish

### Priority 0 — Critical Fixes (Pre-Release Blockers)

These issues must be resolved before any production use:

1. **Fix Command Injection Vulnerabilities** (`src/configure/users.rs`)
   - User-provided username/password are interpolated directly into shell commands
   - Must use proper argument escaping or pass credentials via stdin

2. **Fix Hardcoded Root Partition Number** (`src/configure/bootloader.rs`)
   - Currently assumes partition 4 is root (only valid for standard layout)
   - Minimal layout has root at partition 3, causing unbootable systems

3. **Fix systemd-boot UUID Placeholder** (`src/configure/bootloader.rs`)
   - `<ROOT_UUID>` literal string is never replaced with actual UUID
   - Systems installed with systemd-boot will not boot

4. **Address Encryption No-Op** (`src/configure/encryption.rs`)
   - Users can enable encryption, but the feature does nothing
   - Either implement LUKS support or return an explicit error

### Priority 1 — Stability & Quality

1. **Add Unit and Integration Tests**
   - Config parsing tests
   - Partition layout calculation tests
   - sfdisk script generation tests
   - Dry-run workflow tests

2. **Fix Sudoers Modification**
   - Use `/etc/sudoers.d/` drop-in or validate with `visudo -c`

3. **Read Sector Size from Device**
   - Support 4096-byte sector NVMe drives

4. **Add CI/CD Pipeline**
   - Build, test, clippy, rustfmt checks

### Priority 2 — Usability Improvements

1. **Add Progress Indicators**
   - `indicatif` is already a dependency but unused
   - Add progress bars for long operations (basestrap, formatting)

2. **Add Installation Logging**
   - Write logs to `/tmp/deploytix-{timestamp}.log` for debugging

3. **Input Validation**
   - Validate timezone, locale, keymap against available options
   - RFC 1123 hostname validation
   - Full POSIX username compliance

4. **BIOS/Legacy Boot Support**
   - Detect boot mode and support both UEFI and BIOS installations

### Priority 3 — Feature Expansion

1. **Full LUKS Encryption Support**
   - LUKS2 container creation
   - crypttab generation
   - mkinitcpio encrypt/sd-encrypt hooks
   - GRUB cryptodisk support

2. **BTRFS Subvolume Support**
   - Modern subvolume layout (@, @home, @snapshots)
   - Snapper/Timeshift integration
   - Bootable snapshots

3. **Additional Desktop Environments**
   - Cinnamon, MATE, LXQt, Sway/i3, Budgie

4. **Installation Profiles**
   - Pre-configured profiles: Minimal, Desktop, Gaming, Development, Server

5. **Post-Install Hooks**
   - User-defined scripts at various installation stages

6. **Package Customization**
   - Allow adding/excluding packages from base installation

7. **Mirror Selection**
   - Auto-detect fastest mirrors or manual selection

8. **Recovery/Rollback**
   - Checkpoint system for failed installations
   - Ability to rollback to last known good state

## Technical Debt

| Issue | Location |
|-------|----------|
| Duplicate unmount logic | `cleanup/mod.rs` and `install/chroot.rs` |
| Dead code with `#[allow(dead_code)]` | Multiple files |
| Inconsistent error types | Mix of `DeploytixError` and `anyhow::Result` |
| Silent error handling | `partitioning.rs`, `chroot.rs`, `cleanup/mod.rs` |
| Custom layout not implemented | `disk/layouts.rs` |
| Hibernation support incomplete | Resume parameter not added to cmdline |

## Development Workflow

### Building

```bash
# Development build
cargo build

# Release build
cargo build --release

# Static portable binary (musl)
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

### Testing (Once Implemented)

```bash
cargo test                    # Run all tests
cargo test --lib              # Unit tests only
cargo test --test integration # Integration tests
```

### Code Quality

```bash
cargo clippy -- -D warnings   # Linting
cargo fmt -- --check          # Formatting check
```

## Documentation

- `README.md` — User-facing documentation and usage examples
- `CODEBASE_REVIEW.md` — Detailed technical review with specific code locations
- `docs/CRYPTTAB_HOOKS_DOCUMENTATION.md` — Encryption subsystem reference
- `docs/INTEGRATION_GUIDE_CRYPTO_BTRFS.md` — LUKS + BTRFS implementation guide
- `docs/artix-runit-crypto-install-spec.md` — Encrypted runit installation specification

## Success Criteria

The project will be considered production-ready when:

1. All P0 critical bugs are fixed
2. Core installation workflow works reliably for both partition layouts
3. Basic test coverage exists for critical paths
4. CI/CD pipeline validates builds and tests
5. Users can install a bootable Artix Linux system via both interactive and config-file modes

---

*Document generated: 2026-01-30*
