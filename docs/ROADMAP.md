# Deploytix Roadmap

**Status: August 2026.** Written after auditing an external feature-advancement
study against the actual tree. The study's priorities are broadly right; its
*sequencing* was wrong for this codebase, and several of its premises about
current state were inaccurate. This document records the corrected picture and
the order the work should land in.

---

## The sequencing argument

The study led with a **Deployment Graph + plan/apply/resume engine** as its top
priority — a structural refactor of a 26,588-line installer.

At the time it was written, this repository's only merge gate was `fmt` +
`clippy`. `cargo test` ran nowhere in CI. Landing a whole-pipeline refactor of
the *boot path* under those conditions means rewriting the code that decides
whether a machine boots, with nothing automated to catch a regression.

So the safety net went in first. That work is now done (see below), and the
graph work is correspondingly less dangerous to attempt.

The general principle worth keeping: **for a project whose failure mode is an
unbootable machine, verification capacity gates architectural ambition.** Each
phase below should leave behind more ability to detect regressions than it
consumed.

---

## Completed — the foundation slice

| Area | What landed |
|---|---|
| Secret handling | `utils::secret::Secret` — a `#[serde(transparent)]` newtype whose `Debug` is `<redacted>` and which has no `Display`. Committed credentials scrubbed from `deploytix.toml`; `save_to()` creates 0600. |
| Privileged execution | `run_in_chroot_argv`, `run_in_chroot_argv_stdin`, `run_with_stdin` on `CommandRunner`. Chroot commands needing no shell moved to argv; credentials moved to stdin pipes. |
| sudo policy | `/etc/sudoers` is no longer rewritten. A `/etc/sudoers.d` drop-in is staged, validated with `visudo -cf`, then activated. New `system.sudo_policy` defaults to `password`. |
| Config validation | `validate()` split into `validate_device()` + pure `validate_rules()`; 43 tests; three new rules (sudoer↔wheel, POSIX username, RFC-1123 hostname). |
| CI | `.github/workflows/ci.yml` gates fmt + clippy + `cargo test --all-features` on push and PR. `cargo test` added to the release workflow. |
| `--dry-run` | The `-n`/`--dry-run` flag CLAUDE.md had documented for some time now exists. |

Test count went from 163 to 233.

### Two bugs this work surfaced

Both were found *by* the safety net, which is the argument for building it
first:

1. **`configure_sudoers` could destroy `/etc/sudoers`.** It did
   `read_to_string(path).unwrap_or_default()` and then wrote the result back,
   so an unreadable or not-yet-installed sudoers file became a one-byte file
   containing a newline — and the function returned `Ok(())`. It also granted
   `NOPASSWD: ALL` to all of `%wheel` unconditionally.

2. **`generate-config` emitted a config that `validate` rejected.**
   `DeploymentConfig::sample()` pairs `NetworkBackend::Iwd` with
   `install_yay = false`, which its own iwd→yay rule forbids. The device check
   ran first and masked it. `docs/deploytix-validation.md` T0c asserts the
   opposite; that assertion is now true and test-enforced.

---

## Corrections to the study's premises

Worth recording, because acting on the uncorrected version would waste effort:

| Study claim | Actual |
|---|---|
| `-n`/`--dry-run` is an existing global flag | It was documented but never wired up. The `CommandRunner` plumbing was complete; only the flag was missing. |
| Test coverage is sparse (~68 tests) | It was 163 before this work, 233 after. `docs/test-coverage-proposal.md` was stale by ~165. |
| `docs/IMPROVEMENTS.md` describes current risks | Written February 2026; most P0 items were already fixed. Now marked historical. |
| `docs/LOGICAL_ERRORS_REPORT.md` lists open bugs | All four Critical findings were already fixed. Now marked historical. |
| Rehearsal is Deploytix's dry-run | Rehearsal is the *opposite* — it performs a real install then wipes. The two are complementary. |

**Accurate and still open:** the Artix-host requirement, absent aarch64 support,
no plan/apply separation, no recovery generations, and the combinatorial testing
gap.

---

## Next — in recommended order

### 1. Consolidate validation into one rule table

Validation currently exists in **three** places that drift:

- `DeploymentConfig::validate_rules()` — ~33 rules, fail-fast, single error.
- GUI panel validators (`gui/panels/disk_config.rs`, `user_config.rs`,
  `system_config.rs`) — ~8 hand-rolled rules duplicating a subset.
- `from_wizard()` — enforces prerequisites by not asking the question.

The GUI calls the real `validate()` only *after* the user clicks Install
(`gui/app.rs`), so a config that cannot possibly succeed is caught on the
progress screen rather than next to the offending control.

Replace all three with one rule table carrying `severity`, `message`,
`remediation`, and the config paths each rule touches. Then the CLI prints
them, and the GUI can disable an option *with the reason attached*. This is the
study's P0 #2 and the substance of community issue #69.

Prerequisite: the rules must have tests first — which they now do.

### 2. Accumulate errors instead of failing fast

`validate_rules()` returns on the first violation, so fixing a config by
running `deploytix validate` repeatedly is one error per run. Return a
`Vec<Diagnostic>`. Cheap once rules are a table rather than a function body.

### 3. Automate the T0–T18 matrix

`docs/deploytix-validation.md` already specifies a full manual validation
matrix — 19 groups, with pass criteria and `file:line` references for each
failure. **It is a written specification for the automated install matrix the
study asks for**, and nobody needs to design it from scratch.

Automate it against QEMU with virtual disks, tiered: a couple of VM boots per
PR, all four init systems nightly, both architectures at release. Success must
mean *booted and verified*, never installer exit code — several historical issue
reports describe systems that installed cleanly and then failed to boot.

### 4. Non-Artix host backend

`docs/NON_ARTIX_HOST_PLAN.md` and `NON_ARTIX_HOST_TASKS.md` contain a detailed,
milestone-broken-down design with `file:line` references. **None of it is
implemented** — there is no `src/host/`, no `HostEnvironment`, and every
checkbox in the task list is unticked. `basestrap` remains an unconditional host
requirement (`utils/deps.rs`), and the exact failure the plan targets (a bare
`pacman` exec on Ubuntu) is still live.

M1–M3 are marked independently shippable. This is the largest adoption barrier
with the least design uncertainty remaining.

### 5. Deployment Graph

Now reasonable to attempt. Keep the existing six-phase pipeline as an adapter
that emits graph nodes, diff the generated command streams against the legacy
path in CI, and only then remove phase-level orchestration.

Note `src/pkgdeps/` already resolves dependency closures and emits Graphviz —
graph-shaped output is not foreign to this codebase.

### 6. Recovery generations

`grub-btrfs` snapshot boot already works. The open question is what constitutes
a *coherent* generation: root snapshot, package state, kernel, initramfs and
boot config must roll back together. Success criterion is "booted the selected
generation and passed health checks", not "snapshot command returned zero".

---

## Known gaps not yet scheduled

- **aarch64.** No architecture detection exists anywhere. `x86_64-efi` and
  `BOOTX64` are hardcoded across `configure/bootloader.rs` and
  `configure/secureboot.rs`, and `/boot/efi` is assumed throughout. Needs an
  architecture-capability abstraction, not scattered `if arch ==` branches.
  Compiling on ARM would not mean supporting it.
- **BIOS/legacy boot.** UEFI-only; noted as far back as `IMPROVEMENTS.md`.
- **Root account policy.** There is no `root_password` config field at all, and
  `set_root_password`/`lock_root_account` are dead code with no call sites. The
  installed system's root account is left in whatever state `basestrap` leaves
  it. This should be an explicit, validated choice.
- **The pacman path is still shell-string-based.** `pacman_install_chroot` takes
  a composed command string because `InteractivePolicy` renders it for user
  review and may rewrite it, and `inject_config_flag` splices `--config` into it
  during signature-error recovery. Moving it to argv means reworking the policy
  and retry machinery together. Package names there come from internal constants
  or operator-reviewed lists, so this is a cleanup, not an active hole.
- **`--dry-run` is best-effort, not guaranteed.** Every destructive site is
  guarded (verified by execution against a loop device), but the guarantee is
  convention plus `CommandRunner`, not a sandbox. Making it structural would
  mean routing the remaining raw `std::process::Command` sites — mostly
  `cryptsetup` calls that need stdin — through `CommandRunner::run_with_stdin`,
  which now exists.
- **Secrets are not zeroized in memory.** `Secret` prevents *logging* leaks. It
  does not scrub the heap; that needs `zeroize` and careful handling of
  `String` reallocation.
- **`deny_unknown_fields` is not set.** A typo'd or removed config key is
  silently ignored rather than reported. `preserve_home` sat in the reference
  config long after the field was deleted. Fixing this needs a deprecation
  path, since it would reject configs that parse today.

---

## Sizing

The study estimated 38–48 developer-months over 15 months for its full
programme, at roughly three engineers. That estimate looks reasonable for the
scope described. A smaller team should cut the plugin registry, fleet and PXE
work first and keep validation, testing, recovery and configuration — not
compress the testing.
