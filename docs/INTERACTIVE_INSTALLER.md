# Interactive Installer — Design

Design for two new interactive features in the Deploytix installer:

1. **Per-call pacman editing** — review and edit every upcoming pacman / basestrap / yay invocation before it runs.
2. **Post-install extras step** — install additional pacman (and optionally AUR) packages after the configured selection has finished, before unmount/finalize.

Both features are available in the CLI and GUI front-ends and share a single backend abstraction.

## Goals

- Keep config-driven runs (`deploytix install -c config.toml`) silent by default — adding interactivity must not change automation behaviour.
- Make every package-installing step in the pipeline reviewable, but never require a UI to run.
- Persist user-entered extras back into the deployment config so a second run with the saved config reproduces them non-interactively.

## Non-goals

- Editing arbitrary commands — only pacman/basestrap/yay invocations are intercepted.
- Editing pacman flags that affect base behaviour (`-S`, `--needed`). Only *additional* flags are editable.
- Mid-install rollback. Cancel = abort the whole install (the existing emergency cleanup path runs).

## Architecture

A single new abstraction — `InteractivePolicy` — plugged into `CommandRunner` and reused by both UIs. Same wiring as the existing `progress_cb`. When no policy is set the runner behaves exactly as today.

```rust
// src/utils/interactive.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacmanKind {
    /// `basestrap -i <root> ...`
    Basestrap,
    /// `pacman -S ...` (chroot or host)
    Pacman,
    /// `sudo -u <user> yay -S ...` (chroot)
    Yay,
}

#[derive(Debug, Clone)]
pub struct PacmanInvocation {
    pub kind: PacmanKind,
    /// Human-readable label shown to the user (e.g. "Basestrap base system",
    /// "Install yay build deps", "AUR: hhd-git").
    pub label: String,
    pub install_root: Option<String>,
    /// Editable: the package list.
    pub packages: Vec<String>,
    /// Editable: pacman flags beyond the core `-S --needed` (e.g. `--overwrite`,
    /// `--ignore`, `--asdeps`). Mandatory flags (`-S`, `--needed`,
    /// `--noconfirm`) are NOT editable and not exposed here.
    pub extra_flags: Vec<String>,
    /// AUR/yay only — the user account that runs makepkg.
    pub run_as_user: Option<String>,
}

#[derive(Debug, Clone)]
pub enum PacmanDecision {
    /// Run the invocation as-is.
    Approve,
    /// Run with the edited package list / flags.
    EditedTo {
        packages: Vec<String>,
        extra_flags: Vec<String>,
    },
    /// Skip this invocation and continue the install.
    Skip,
    /// Abort the entire install.
    Cancel,
}

#[derive(Debug, Clone, Default)]
pub struct ExtraPackages {
    pub pacman: Vec<String>,
    pub aur: Vec<String>,
}

pub trait InteractivePolicy: Send + Sync {
    fn confirm_pacman(&self, inv: &PacmanInvocation) -> PacmanDecision;
    /// Called from phase 5.95 (after all configured installs, before
    /// finalize). Returns `(extras, save_to_config)`.
    /// Implementations may run their own internal loop (CLI does, GUI
    /// surfaces a step).
    fn prompt_extras(&self, can_use_yay: bool) -> (ExtraPackages, bool);
}
```

`CommandRunner` is extended with:

```rust
policy: Option<Arc<dyn InteractivePolicy>>,

pub fn with_policy(self, policy: Arc<dyn InteractivePolicy>) -> Self;
pub fn pacman_install(&self, inv: PacmanInvocation) -> Result<()>;  // dispatches through policy
```

`pacman_install()` is the **only** public entry point for pacman/basestrap/yay calls. All existing call sites are migrated to it. With no policy attached, it builds the command string and runs it directly (today's behaviour).

### Cancel semantics

`PacmanDecision::Cancel` causes `pacman_install()` to return a new error variant:

```rust
DeploytixError::UserCancelled
```

`Installer::run()` already has emergency cleanup that fires when any phase returns `Err`; we wire the new variant through that path so a cancel cleanly unmounts and closes LUKS.

## Feature 1 — Per-call pacman editing

Every pacman invocation gets its own prompt. Approximate prompt count for a full install with all options on:

| # | Label | Source |
|---|---|---|
| 1 | Basestrap base system | `install/basestrap.rs` |
| 2 | yay build deps (`go git base-devel`) | `configure::packages::install_yay` |
| 3 | AUR: zen-browser-bin | `configure::packages::install_aur_packages` |
| 4 | AUR: iwd frontend | `configure::packages::install_iwd_frontend` |
| 5 | AUR: btrfs tools (snapper, btrfs-assistant) | `configure::packages::install_btrfs_tools` |
| 6 | AUR: hhd-git | `configure::packages::install_hhd` |
| 7 | AUR: decky-loader-bin | `configure::packages::install_decky_loader` |
| 8 | AUR: evdevhook2-git | `configure::packages::install_evdevhook2` |

Optional installs not selected produce no prompt.

### Edit semantics

The editor receives the package list (one per line) and a separator block listing extra flags. Lines starting with `#` are comments and ignored. The fixed core (`-S --needed --noconfirm` for pacman/yay; `-i <root>` for basestrap) is shown but commented out and stripped on save:

```
# Pacman invocation: AUR: hhd-git
# Run by: yay (sudo -u alice)
# Core flags (NOT editable): -S --needed --noconfirm
# Add one package per line. Lines starting with # are ignored.

hhd-git

# === Extra flags (one per line) ===
```

The basestrap editor prepends a high-visibility warning:

```
# !! WARNING: editing the basestrap package list can brick the install.
# !! Removing `base`, `linux`, the configured init's package, or the
# !! bootloader will leave you with an unbootable system.
```

### Activation

- CLI: new `-i` / `--interactive` flag on `deploytix install`. When set, an `InteractivePolicy` is attached to the runner. Default is **on** for the no-arg wizard (`deploytix`) and `deploytix install` without `-c`; **off** for `deploytix install -c config.toml`.
- GUI: a checkbox on the summary panel (`Review pacman commands`). Default off; toggling on attaches the GUI policy.

### CLI implementation

Uses `dialoguer` (already a dependency):

```
[8/8] AUR: evdevhook2-git
  yay -S --needed --noconfirm evdevhook2-git
  (run as alice, in chroot)

[A]pprove · [E]dit · [S]kip · [C]ancel ›
```

Edit mode writes the editable view to a temp file and opens `$EDITOR` (falling back to `vi`). On save: parse, strip comments, reconstruct the invocation, return `EditedTo`.

### GUI implementation

A modal dialog. The install thread blocks on a oneshot channel while the modal is rendered.

- Worker thread: builds the prompt, sends `GuiPromptRequest::ConfirmPacman(inv, oneshot::Sender<PacmanDecision>)` over an mpsc to the main thread, then blocks on the oneshot.
- Main thread: when a `ConfirmPacman` request is queued, opens a modal egui window with:
  - The label and command preview at the top.
  - A multiline `TextEdit` for the package list (one per line).
  - A multiline `TextEdit` for extra flags.
  - Buttons: `Approve`, `Save edits`, `Skip`, `Cancel install`.
- On click: send the decision back via the oneshot, close the modal.

This mirrors the existing pattern used for `progress_cb` — same channel ergonomics, just bidirectional.

## Feature 2 — Post-install extras step

A new pipeline phase **5.95** in `installer.rs`, fired only when an `InteractivePolicy` is attached. Runs after every other configured optional install (autostart, btrfs tools, evdevhook2, …) and before phase 6 (`Finalize`).

```rust
// installer.rs run_phases() — at the end of phase 5
if let Some(policy) = self.cmd.policy() {
    self.report_progress(0.92, "Optional: install extra packages...");
    let can_use_yay = self.config.packages.install_yay;
    let (extras, save) = policy.prompt_extras(can_use_yay);
    if !extras.pacman.is_empty() {
        configure::packages::install_extras_pacman(&self.cmd, INSTALL_ROOT, &extras.pacman)?;
    }
    if can_use_yay && !extras.aur.is_empty() {
        configure::packages::install_extras_aur(&self.cmd, &self.config, INSTALL_ROOT, &extras.aur)?;
    }
    if save {
        self.config.packages.extra_packages = extras.into();
        // Existing save path — write the merged config to a known location
        // so the user can re-run with -c later.
        self.config.save_to(&extras_save_path()?)?;
    }
}
```

### CLI flow

A single sequential prompt loop:

```
== Optional: install extra packages ==

Repository packages (pacman -S):
  Type space-separated package names, or empty to skip:
  > htop neofetch ripgrep

Installing 3 package(s) via pacman in chroot...
  ✓ htop installed
  ✓ neofetch installed
  ✓ ripgrep installed

AUR packages (yay -S, run as alice):
  Type space-separated package names, or empty to skip:
  > 

Save these extras to your config so re-runs install them non-interactively? [y/N]
  > y

Saved to /home/alice/.config/deploytix/last-install.toml
```

If `install_yay = false` the AUR prompt is silently skipped.

### GUI flow

A new wizard step "Install Extras", visible only after main installation succeeds (gated by a flag set in the worker thread). The step contains:

- Two text fields: `Pacman packages` and `AUR packages` (greyed out if `install_yay = false`).
- An "Install" button per field that triggers the install in the worker thread and streams output to a scrollable log view in the same panel.
- A "Save these extras to my config" checkbox.
- A `Continue` button that proceeds to Finalize.

Re-running an `Install` button after an earlier failure is allowed — failures don't abort the install.

### Persistence

Add to `DeploymentConfig`:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtraPackagesConfig {
    #[serde(default)]
    pub pacman: Vec<String>,
    #[serde(default)]
    pub aur: Vec<String>,
}

// In PackagesConfig:
#[serde(default)]
pub extra_packages: ExtraPackagesConfig,
```

When `prompt_extras()` returns `save = true`, the merged config (including any `extra_packages`) is written to `~/.config/deploytix/last-install.toml`. A user can then `deploytix install -c ~/.config/deploytix/last-install.toml` and skip every prompt.

When `extra_packages` is non-empty in a config-driven (`-c`) run, those packages are installed automatically in phase 5.95 even with no `InteractivePolicy` attached — the policy gates *prompting*, not *installing*.

## Activation matrix

| Invocation | Per-call prompts | Extras step |
|---|---|---|
| `deploytix` (no-arg interactive wizard) | **on** | **on** |
| `deploytix install` (no `-c`) | **on** | **on** |
| `deploytix install -c FILE` | off | runs if `extra_packages` is set in FILE |
| `deploytix install -c FILE -i` | **on** | **on** |
| GUI default | off | off |
| GUI with "Review pacman commands" checked | **on** | **on** |

Per-call prompts and the extras step are bound to the same activation flag — there's no separate toggle for one without the other in v1. A future flag (`--no-extras-prompt`, etc.) can split them later if needed.

## Implementation order

Two commits, each independently reviewable:

### Commit A — backend + CLI

1. `src/utils/interactive.rs` — trait, types, error variant.
2. `CommandRunner::with_policy()`, `pacman_install()` dispatcher.
3. Migrate every pacman/basestrap/yay call site to `pacman_install()`.
4. CLI policy impl (`src/cli/interactive.rs`): stdin prompts, `$EDITOR` for edit mode, extras loop.
5. New phase 5.95 in `installer.rs`.
6. `extra_packages` field in `DeploymentConfig`; phase 5.95 honours it in non-interactive mode.
7. `-i` / `--interactive` flag on the `install` subcommand; default activation rules from the matrix above.

### Commit B — GUI

1. `gui::interactive::GuiPolicy` — implements `InteractivePolicy`, holds an mpsc channel into the main thread.
2. Modal pacman-confirm dialog wired into the existing app event loop.
3. New wizard step "Install Extras" (only reachable after a successful main install).
4. Summary-panel checkbox to enable interactive mode.
5. Save-to-config wiring.

## Risks

- **Per-call prompt fatigue.** A full install with all options is ~8 prompts. Approve-all keyboard shortcut would help; deferred to a follow-up.
- **Editor collisions.** If the user has `$EDITOR=code` (non-blocking), the CLI flow stalls. We detect known offenders (`code`, `subl`) and fall back to `vi` with a one-line warning.
- **Cancel during chroot.** `Cancel` between phases is clean; cancel mid-pacman during the actual transfer is not — we don't try to interrupt, we let it complete and abort at the next phase boundary.
- **Saved config secrets.** The deployment config currently stores `user.password` and `disk.encryption_password` in plain text. Saving it to `~/.config/deploytix/last-install.toml` after an interactive run inherits that risk. The save prompt's confirmation text must call this out: "**includes your user and encryption passwords in plain text**".
