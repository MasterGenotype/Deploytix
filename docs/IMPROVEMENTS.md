# Deploytix: Recommended Improvements

An audit of the current codebase surfaced the following improvement areas,
organized by priority. Each item includes the relevant source location and a
concrete suggestion.

---

## P0 -- Security & Correctness (fix before any release)

### 1. Command injection in user/password handling

**Files:** `src/configure/users.rs:37`, `src/configure/users.rs:98`

The password and username are interpolated directly into a shell string passed to
`run_in_chroot`:

```rust
let chpasswd_cmd = format!("echo '{}:{}' | chpasswd", username, password);
```

A password containing `'; rm -rf /; echo '` would execute arbitrary commands
inside the chroot. The same pattern appears in `set_root_password`.

**Recommendation:** Write the credentials to a temporary file (or pipe via
stdin) instead of embedding them in a shell command string. For example, write
`username:password` to a file inside the chroot, then run
`chpasswd < /tmp/deploytix-cred && rm /tmp/deploytix-cred`. Alternatively,
refactor `CommandRunner` to support stdin piping so the value never touches the
shell.

### 2. Command injection in `useradd` arguments

**File:** `src/configure/users.rs:30-34`

`username` and `groups_str` are interpolated into a shell string without
sanitization. A username like `foo; reboot` would execute the injected command.

**Recommendation:** Validate usernames against `^[a-z_][a-z0-9_-]*$` (POSIX
standard) and group names against a similar pattern before use. Better yet, pass
them as discrete arguments to `CommandRunner::run` instead of building a shell
string.

### 3. Sudoers file modified directly without validation

**File:** `src/configure/users.rs:60-79`

The code reads `/etc/sudoers`, performs string replacement, and writes it back
without running `visudo -c` to validate the result. A malformed sudoers file
will lock all users out of `sudo`.

**Recommendation:**
- Write to `/etc/sudoers.d/deploytix-wheel` instead (drop-in directory).
- Or, after writing, run `visudo -c` in the chroot and revert on failure.

### 4. Hardcoded root partition numbers

**File:** `src/configure/bootloader.rs:50-60`, `src/configure/bootloader.rs:255-264`

Root partition is determined by a `match` on the layout variant (`Standard => 4`,
`Minimal => 3`, `Custom => 4`). If layout definitions ever change, the
bootloader will point to the wrong partition and the system will not boot.

**Recommendation:** Derive the root partition number from `ComputedLayout` by
looking for the partition marked as root (e.g., add an `is_root` flag to
`PartitionDef` or look up by mount point). The `install_grub_with_layout`
function already receives the layout -- extend this pattern to the non-encrypted
path.

### 5. Input validation missing across the board

**Files:** `src/config/deployment.rs`, `src/configure/locale.rs`

User-supplied values for hostname, timezone, locale, and keymap are passed
through without validation:
- Hostnames should be validated against RFC 1123 (alphanumeric + hyphens, 1-63
  chars per label).
- Timezones should be checked against `/usr/share/zoneinfo`.
- Locales should be verified in `/etc/locale.gen`.
- Keymaps should be checked against `localectl list-keymaps` or the keymaps
  directory.

**Recommendation:** Add a `validate()` method on `DeploymentConfig` that runs
all checks before the installation begins. Return `DeploytixError::ValidationError`
with a descriptive message for each failure.

---

## P1 -- Reliability & Code Quality

### 6. Nearly zero test coverage

**Current state:** 1 test across ~7,300 lines of code.

The following are high-value, low-effort test targets that do not require root
or real disks:

| Target | File | What to test |
|--------|------|-------------|
| Partition layout math | `src/disk/layouts.rs` | `compute_layout` returns correct sizes/counts for various disk sizes and layout types |
| sfdisk script generation | `src/disk/partitioning.rs` | Generated script has correct partition types, sizes, and alignment |
| Config parsing | `src/config/deployment.rs` | Round-trip TOML serialization, default values, missing field handling |
| fstab generation | `src/install/fstab.rs` | Correct UUID references, mount options, ordering |
| Dry-run workflows | `src/install/installer.rs` | Full `Installer::run()` in dry-run mode completes without error |

**Recommendation:** Start with unit tests for `compute_layout` and config
parsing -- these are pure functions with no system dependencies. Add integration
tests for dry-run installation next.

### 7. No CI/CD pipeline

There are no GitHub Actions workflows, no `clippy` or `rustfmt` checks, and no
automated test runs on commit.

**Recommendation:** Add a `.github/workflows/ci.yml` that runs:
```yaml
- cargo fmt --check
- cargo clippy -- -D warnings
- cargo test
- cargo build --release --target x86_64-unknown-linux-musl
```

### 8. Inconsistent error handling strategy

The codebase mixes `DeploytixError` (via `thiserror`) and `anyhow::Result`.
Some functions return one, some the other, and conversions between them are
implicit.

**Recommendation:** Pick one strategy. For a library/CLI hybrid, the cleanest
approach is:
- Use `DeploytixError` for all public-facing errors (installer, config, disk).
- Reserve `anyhow` for the `main()` entry point and GUI top-level, where you
  just need to display the error.
- Remove `anyhow` from internal modules.

### 9. `unwrap()` calls in critical paths

**File:** `src/install/installer.rs:203`, `:213`, `:228`, `:247`, `:351`, `:401`, `:446`, `:551`, `:577`

Multiple `self.layout.as_ref().unwrap()` calls exist. While the layout is always
set after `prepare()`, a logic change could introduce a panic path.

**Recommendation:** Replace with:
```rust
let layout = self.layout.as_ref().ok_or(DeploytixError::ConfigError(
    "Layout not computed -- call prepare() first".to_string()
))?;
```

Or compute the layout in the constructor so it is always available.

### 10. Duplicate unmount logic

**Files:** `src/cleanup/mod.rs`, `src/install/chroot.rs`

Unmount operations are implemented in two places with slightly different
approaches.

**Recommendation:** Consolidate into a single `unmount` module that both cleanup
and chroot can call.

---

## P2 -- Usability & Developer Experience

### 11. `indicatif` dependency is unused

**File:** `Cargo.toml:17`

The `indicatif` crate (progress bars) is listed as a dependency but never used
in the code. The `ProgressCallback` in `installer.rs` is a custom
implementation.

**Recommendation:** Either:
- Remove `indicatif` from `Cargo.toml` to reduce compile time and binary size.
- Or integrate it for CLI progress display (spinners during basestrap, progress
  bars during partitioning).

### 12. No installation logging to file

All output goes to stdout/stderr via `tracing`. If the installation fails
partway through, the user has no log file to review or share for debugging.

**Recommendation:** Add a file appender to the `tracing-subscriber` setup that
writes to `/tmp/deploytix-<timestamp>.log`. This is a small change in
`main.rs`.

### 13. Error messages lack actionable guidance

When operations fail, the error messages describe what went wrong but not what
the user should do about it. Example: `"Device not found: /dev/sdx"` -- the user
doesn't know if they should replug the device, check permissions, or run as
root.

**Recommendation:** Add a `hint` field or method to key error variants:
```rust
DeviceNotFound(String),  // hint: "Check the device is plugged in and run `lsblk` to list available devices"
NotRoot,                 // hint: "Run deploytix with sudo or as root"
```

### 14. Dead code markers scattered throughout

Multiple `#[allow(dead_code)]` annotations exist across the codebase, including
on the main error enum (`src/utils/error.rs:6`), utility functions, and
partially implemented features.

**Recommendation:** Audit each `#[allow(dead_code)]` item:
- If it is planned for near-term use, add a `// TODO:` comment explaining when.
- If it is speculative, remove it to reduce maintenance burden.
- Never put `#[allow(dead_code)]` on the error enum itself -- all variants
  should be constructible and testable.

---

## P3 -- Feature Gaps & Architecture

### 15. Encryption feature is partially implemented

Users can set `encryption = true` in the config, but only the
`CryptoSubvolume` layout actually implements encryption. The `Standard` and
`Minimal` layouts silently ignore the flag.

**Recommendation:** Either:
- Gate the `encryption` flag behind layout validation so that setting
  `encryption = true` with a non-crypto layout returns an error at config
  validation time.
- Or implement single-partition LUKS for Standard/Minimal layouts.

### 16. No BIOS/Legacy boot support

The bootloader code assumes UEFI exclusively (`--target=x86_64-efi`). Many USB
deployment targets use legacy BIOS.

**Recommendation:** Add a `BootMode` enum (`Uefi | Bios | Both`) and adjust
partition layouts (add BIOS boot partition) and GRUB install commands
accordingly. Detect the host boot mode with `[ -d /sys/firmware/efi ]`.

### 17. systemd-boot entry hardcodes `linux-zen` kernel

**File:** `src/configure/bootloader.rs:296-298`

The systemd-boot entry references `vmlinuz-linux-zen` and
`initramfs-linux-zen.img`. If a user installs a different kernel, the system
will not boot.

**Recommendation:** Derive the kernel name from `config.system.kernel` (add
field if missing) or detect installed kernels by listing
`/boot/vmlinuz-linux-*` in the chroot.

### 18. `once_cell` dependency is unnecessary on Rust 2021+

**File:** `Cargo.toml:39`

`std::sync::LazyLock` and `std::sync::OnceLock` are stable in Rust 1.80+. The
`once_cell` crate is no longer needed if targeting recent stable Rust.

**Recommendation:** Replace `once_cell::sync::Lazy` with `std::sync::LazyLock`
and remove the dependency.

### 19. GUI state management is fragile

**File:** `src/gui/app.rs`

The GUI manages installation state through a flat struct with many `Option`
fields and manual step tracking. Invalid state transitions (e.g., starting
installation before disk selection) are only prevented by UI disabling, not by
the type system.

**Recommendation:** Model the wizard as a state machine with distinct types per
step:
```rust
enum WizardState {
    DiskSelection { ... },
    ConfigReview { config: DeploymentConfig, ... },
    Installing { handle: JoinHandle<Result<()>>, ... },
    Complete { success: bool, log: String },
    Failed { error: String },
}
```

This makes invalid states unrepresentable.

---

## Summary

| Priority | Count | Theme |
|----------|-------|-------|
| P0 | 5 | Security vulnerabilities and correctness bugs |
| P1 | 5 | Testing, CI, error handling consistency |
| P2 | 4 | UX polish, dead code cleanup |
| P3 | 5 | Feature gaps, dependency hygiene, architecture |

The recommended order of work is P0 items first (especially #1 and #2, the
command injection issues), then P1 #6 and #7 (tests and CI) to prevent
regressions, followed by the remaining items.
