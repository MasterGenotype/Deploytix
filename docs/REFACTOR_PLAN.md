# Architecture refactor plan

Status: **all twelve phases are done.** The document is kept as the record of
why the code is shaped the way it is; the sections below describe what was
built, not what is planned. Where reality diverged from the plan, the
divergence is noted in the phase.

Two things the plan got wrong, both worth knowing before trusting any other
part of it:

- **Phase 9's premise was stale.** The plan said `pkgdeps` reached into the
  rest of the codebase for exactly one thing, the shared error type. By the
  time the work happened, the AUR feature had added a second edge — and it
  pointed the wrong way, from `pkgdeps::cli` up into `crate::aur`, while
  `aur` already depended on `pkgdeps`. A cycle is fine inside one crate and
  impossible across two, so the split had to break it first.
- **Phase 6's folder names collide.** `configure/encryption/` cannot hold
  `encryption.rs` without `configure::encryption` becoming ambiguous. The
  crypto group is `configure/crypto/`.

## Why this document exists

A mindmap of the codebase (`~/Downloads/deploytix-flow-and-architecture.minder`,
generated from commit `ce49fdd` plus the current working tree) ends with a
list of ten places where the code works fine today but will make the *next*
change harder than it needs to be. None of these are bugs. They're just spots
where the current shape will push back the next time someone wants to add a
feature, a third backend, or a second host.

This document turns that list into twelve concrete, independent pieces of work
— the mindmap's ten, in the order it put them (roughly biggest payoff for
least effort, first), plus two more (phases 11 and 12) that apply the same
idea to the two places it pays off most. The file/line references below were
checked against the real source, not just the mindmap's notes — but the tree
is actively changing, so re-check line numbers before actually touching code.

**How to read a phase now that they are all done.** Each one opens with a
bolded **Done** note describing what was built and where it lives. Everything
after that note is the original plan text, left in the present tense it was
written in: it describes the problem *as it stood* and the reasoning that
picked the fix. That rationale is the reason the document survives — the code
says what the shape is, and this says why it is that shape rather than another
one. Line numbers in the older text are from before the work and will not
match.

The phases were done one at a time, each followed by
`cargo build --workspace && cargo clippy --workspace --all-features -- -D warnings && cargo test --workspace --all-features`.

---

## The shape half these phases share: insertable modules

Phases 1, 2, 3, 11 and 12 are all the same complaint wearing different
clothes. In each one, a thing deploytix supports *several of* — pipeline
steps, snapshot backends, host targets, desktop environments, init systems —
is spread across the codebase as a `match` arm here, an `if` there, and a
hardcoded list somewhere else. Nothing is wrong with any individual one of
them. The problem is arithmetic: adding the fifth of anything means finding
every place that knew there were four.

The shape that fixes all five is the same, and it's worth naming once here so
the individual phases don't each have to re-derive it. Call it an **insertable
module**: one implementation, in one file, reachable through one registration
point, that the core code finds by lookup rather than by knowing its name.

Concretely, in Rust, that's three pieces:

1. **A trait or a descriptor struct** that says what the core needs from an
   implementation of this kind of thing. A trait when implementations need
   behaviour and state (`SnapshotBackend`, `HostAdapter`); a plain descriptor
   struct of data and function pointers when they mostly need to answer
   questions (`DesktopModule { packages, display_manager, session_units, … }`).
   Prefer the descriptor — it's `const`-constructible, trivially testable, and
   doesn't drag lifetimes into the `Installer` struct.
2. **A registry**: one function, in one file, that maps an identity to its
   implementation. A single `match` here is fine and often better than
   machinery — the win isn't zero `match`es, it's *one* `match` instead of
   seventy-six. Reach for a crate like `inventory`/`linkme` (distributed
   compile-time registration, so a module registers itself in its own file)
   only if the single lookup function genuinely becomes a bottleneck for
   contributors. It probably won't.
3. **Call sites that ask the registry**, not the enum. `de.module().packages()`
   instead of `match de { Kde => …, Gnome => … }` repeated in thirteen files.

Three rules keep this from turning into architecture for its own sake:

- **The config-facing enum stays.** `DesktopEnvironment` and `InitSystem`
  derive `Serialize`/`Deserialize` with `rename_all = "lowercase"`, so they
  *are* the TOML surface. Deleting them in favour of strings would break every
  existing config file and every validation error message for no gain. Keep
  the enum as the identity/key; move only the *behaviour* behind it into
  modules. Adding a variant is then one enum line plus one file plus one
  registry line — not a bisect through the tree.
- **No dynamic loading.** Insertable means insertable at compile time, by
  someone editing this repo. Deploytix runs as root against real disks;
  `dlopen`-ing third-party code into that process is not a trade this project
  should make. If out-of-tree extension ever matters, the honest mechanism is
  a declarative package/service manifest that deploytix reads, not a plugin
  ABI.
- **One kind of thing at a time.** Each of these phases is independently
  shippable and independently revertible. Don't build a generic
  "registry framework" that all five share — they have genuinely different
  shapes, and a common abstraction over them would be a fourth thing to
  understand rather than a simplification.

---

## 1. Turn the install pipeline into a list, not a wall of code

**Done.** `run_phases()` is a loop over `pipeline()`, a `Vec<Step>` of
`{kind, status, weight}`. Progress is the running sum of the weights of the
steps this config actually calls for, mapped into the range `prepare()` leaves
(it owns the first tenth). Four tests in `installer.rs` pin the step list, the
storage dispatch, the yay-dependent steps and progress monotonicity — the
by-hand check against the old progress table, made repeatable.

`Installer::run_phases()` (`src/install/installer.rs:244`) is about 300 lines
of `self.some_step()?` calls, roughly 30 of them, each one hand-annotated with
a progress percentage someone typed in by hand (0.10, 0.15, ... 0.985). On top
of that, the question "which storage backend are we using" gets asked three
separate times — once to decide which setup/format/mount functions to call,
then again to decide which fstab generator to use, then again for crypttab.

The fix is to describe the pipeline as data instead of code: a list of steps,
each with a name, a rough weight, and a condition for whether it applies to
the current config. `run_phases()` then just loops over that list. This means
the "which backend" question gets answered once per step instead of three
times total, progress bars come from adding up weights instead of guessing
fractions, and — down the road — things like a GUI preview of what's about to
happen, or resuming a failed install partway through, become realistic rather
than a rewrite.

The catch: this is the biggest single function in the codebase, and the
natural first instinct — wrapping each step as a boxed closure — runs into the
Rust borrow checker hard, because almost every step needs `&mut self`. Using
a plain enum to tag each step kind and dispatching on it in a `match` sidesteps
that fight.

This change is contained entirely inside `src/install/installer.rs`. Done
right, the order and conditions under which things run don't change at all —
only how that sequence is expressed. The existing installer tests should pass
unmodified, and the new step list can be checked by hand against the current
progress table to make sure nothing got dropped or reordered.

---

## 2. Give the two snapshot backends a common interface

**Done.** `src/immutable/backend.rs`. `main.rs` no longer names a backend;
`backend::active()` answers once and `remove_refusal()` carries the A/B
refusal, verbatim. Phase 8 later added `staged_change()` to the trait, which
is what a rehearsed transaction needs in order to undo itself.

Deploytix has two ways of doing transactional updates/rollback: btrfs
snapshots (spread across `src/immutable/snapshot.rs`, `boot.rs`, `rollback.rs`,
`update.rs`) and LVM A/B slots (`src/immutable/lvm_ab.rs`). They each grew
their own create/activate/rollback functions independently, and nothing ties
them together — there's no shared trait at all.

The place this shows up concretely is `src/main.rs`. The `remove` command
knows about the LVM A/B backend by name and refuses to run on it:

```rust
if is_lvm_ab() {
    return Err(DeploytixError::ConfigError(
        "`deploytix remove` is not implemented for the LVM A/B backend. ...".to_string(),
    ).into());
}
```

while `update` and `rollback` both branch on that same `is_lvm_ab()` check to
decide which backend's functions to call. Every capability difference between
the two backends is a fact main.rs has to know about.

The fix is a small trait — something like `SnapshotBackend` with `activate`,
`rollback`, and `supports_remove` — implemented once for btrfs and once for
LVM A/B, each mostly just calling the existing functions that already do the
work. Then `main.rs` asks "does this backend support remove?" instead of
knowing the answer is hardcoded to "no, if it's LVM A/B." Adding a third
backend later becomes writing one more implementation, not hunting down every
place that currently says `is_lvm_ab()`.

Worth noting: the two backends aren't fully symmetric today (LVM A/B has no
standalone "create" step — it's inlined directly in the installer), so the
first version of this trait shouldn't try to force symmetry that doesn't
exist yet. Leave snapshot *creation* out of the trait for now; just cover
activate/rollback/remove-support.

This touches a new `src/immutable/backend.rs` and the three command handlers
in `src/main.rs`. To check it worked: `deploytix remove` should still refuse
on LVM A/B with the same message as before, and update/rollback should behave
exactly as before on both backends.

This is the trait-shaped version of the insertable-module pattern described
above: `backend.rs` holds the trait plus the one `detect()`-driven lookup that
returns the live backend, and `main.rs` stops naming backends entirely. The
registry here is a two-arm `match` inside a single function — that's the whole
mechanism, and it's enough.

---

## 3. Put a seam between "run a command" and "run it on this particular host"

**Done.** `src/utils/host.rs`: `HostAdapter` with `chroot_cmd`,
`has_binary` and `missing_binaries`, and one implementation, `LocalHost`.
`CommandRunner` holds `&'static dyn HostAdapter` and `deps.rs` asks the
adapter instead of shelling out to `which` itself. Nothing behaves
differently, which was the bar.

Right now, running something inside the target's chroot is done by calling
one of two concrete functions directly — `run_in_artix_chroot` and
`CommandRunner::run_in_chroot`, both in `src/utils/command.rs`. There's no
abstraction in between; every caller talks straight to "the local machine."
Likewise, `src/utils/deps.rs` checks for required binaries by shelling out to
`which` on the host running deploytix.

If deploytix ever needs to target something other than "the machine it's
running on" — a container, a remote host, whatever — every one of those call
sites would need to change. The cheap insurance policy is to introduce a small
`HostAdapter` trait now, while there's only one implementation, rather than
retrofitting it later when there are two and every caller needs updating at
once. Concretely: a trait with something like `chroot_cmd` and
`required_binaries`, and one implementation, `LocalHost`, that's just today's
code moved behind the trait.

This phase is explicitly not about enabling multi-host support — it's just
laying the groundwork so that if it's ever needed, it's an addition instead of
a rewrite. Nothing should behave differently before and after; the bar for
success is a clean build and unchanged test results.

---

## 4. Move a safety check to where the danger actually lives

**Done.** `SHARED_LIVE_MOUNTS` and `UNRECOVERABLE_SHARED_MOUNTS` sit beside
`INITRAMFS_OWNED_MOUNTPOINTS` in `immutable/mod.rs`; `mount_set_cmd` builds
its rbind list from the first, and `remove.rs` asks
`unrecoverable_shared_mount()` instead of testing for `/boot/` itself. The
two constants are deliberately separate: `/var` and `/home` are shared but
hold data a package owns, and blocking every package that owns a file under
them would refuse most of the repository.

When removing a package on an immutable root, `check_protected()`
(`src/immutable/remove.rs:331-353`) refuses to let you delete the kernel image
or (on encrypted setups) anything cryptsetup/initramfs needs to unlock the
disk. The reason this check exists: `/boot` and `/var` are bind-mounted rather
than snapshotted, so deleting the wrong file there affects the *live* system,
not just the throwaway snapshot being worked in.

But that hazard isn't really about removal — it's a property of the shared
mount itself (`mount_set_cmd` in `src/immutable/update.rs:71`, which both
update and remove route through). Update just happens to be safe today because
it only ever adds files there, never deletes them. If that ever changes, the
danger is back and nothing would catch it, because the check lives next to
`remove`, not next to the mount.

The fix is just to move the "these paths are shared and dangerous to touch"
knowledge to sit beside `INITRAMFS_OWNED_MOUNTPOINTS` in
`src/immutable/mod.rs`, where the mount model itself is described, and have
`remove.rs` read it from there instead of hardcoding its own copy. This is a
relocation, not a rewrite — the existing tests around `check_protected` should
produce exactly the same allow/block decisions afterward.

---

## 5. Move the "build software from source" code out of "configure"

**Done.** `configure/packages.rs` is now `install/packages.rs`.

`src/configure/packages.rs` has grown to over 3,000 lines and now does things
like compiling a custom kernel, building a browser from source, and installing
AUR packages inside the chroot — 26 separate chroot commands, far more than
any other file in `configure/` (the next-busiest file has 7). Everything else
in `configure/` is about setting up a system that's already installed
(locale, users, network); this file is actually about installing more
software, which is a different kind of job.

The fix is simply to move it — to `src/install/packages.rs`, next to the rest
of the installation logic, updating the module declaration and the handful of
call sites (like the TKG kernel install call in `installer.rs`) that reference
it by its old path. No logic changes, just a relocation. `cargo build` will
catch every import that needs updating.

---

## 6. Split `configure/` along the lines it already has

**Done**, as `configure/{crypto,system,boot,gaming}/` — `crypto` rather
than `encryption` for the name-collision reason at the top of this file. The
individual modules are re-exported flat from `configure/mod.rs`, so call sites
still say `configure::services`.

`configure/` is 19 files and about 14,000 lines, and it's actually doing four
unrelated jobs stacked on top of each other: encryption setup (`encryption.rs`,
`keyfiles.rs`, `secureboot.rs`, `verity.rs`), general system config
(`locale.rs`, `users.rs`, `network.rs`, `services.rs`, `swap.rs`,
`mkinitcpio.rs`, `hooks.rs`), boot setup (`bootloader.rs`, `grub_btrfs.rs`),
and the gaming/handheld stack (`session_switching.rs`, `handheld_quirks.rs`,
`gamescope_update.rs`, `display_manager.rs`, `greetd.rs`). Both the original
architecture diagram and the mindmap ended up drawing these as separate
clusters just to stay readable — that's a decent sign the folder itself should
be four folders.

The fix is to create `configure/{encryption,system,boot,gaming}/` and sort the
existing files into them, keeping `configure/mod.rs`'s re-exports working so
call sites elsewhere don't all need to change. This is pure organization —
files move, nothing inside them changes. `cargo build` will flag any import
that needs adjusting, and every test moves with its file.

---

## 7. Make cleanup automatic instead of something you have to remember to call

**Done**, as a backstop rather than a replacement. `cleanup::guards`
provides a `ResourceStack` that the installer registers mounts, LUKS
containers and the volume group with as it opens them, and that releases them
in reverse on drop. `emergency_cleanup()` still runs first on an ordinary
failure — its rescan catches cryptsetup's own temporary mappings, which no
guard can know about, and its commands go through the `CommandRunner` so a
rehearsal records them — and disarms the stack when it is done. The stack is
what covers the paths nobody thought to add a cleanup call to.

Per-mount guards were considered and rejected: filesystems are mounted from
half a dozen places across `disk/` and `install/` that take a
`&CommandRunner` rather than a `&mut Installer`, so tracking each one would
have meant changing every one of those signatures. The mount *tree* under the
install root is the resource that actually has to come off.

*(The mindmap suggests doing this alongside phase 1, since both touch how the
install pipeline is structured. Listed separately here to keep phases
independent, but worth designing together if picked up at the same time.)*

`Cleaner` (`src/cleanup/mod.rs`) tears down mounts and closes encrypted
volumes by re-scanning `/proc/mounts` and `/dev/mapper` fresh each time, and
someone has to remember to actually call `.cleanup()` — nothing happens
automatically if that call gets skipped. Compare that to `DiskWipeGuard`
(`src/rehearsal/guard.rs`), which already does this the other way in the same
codebase: it's tied to a variable's lifetime, and if that variable goes out of
scope while still "armed," Rust's `Drop` mechanism cleans up automatically,
no matter how execution got there (including a panic).

The fix is to build a few small guard types along the same lines — one for
mounts, one for LUKS containers, one for volume groups — and have the
installer pick one up each time it opens one of those resources. Then cleanup
falls out of normal Rust scoping instead of relying on `emergency_cleanup()`
correctly guessing what needs tearing down after the fact. This doesn't
replace `Cleaner` — the standalone `deploytix cleanup` command still needs the
live-rescan approach, since by definition it's cleaning up state left over
from a process that's no longer running and has no guards to drop.

The real cost here isn't the guard types themselves — it's threading their
lifetimes through the `Installer` struct correctly. Existing
signal-handling/emergency-cleanup tests are the way to confirm unmount and
LUKS-close ordering didn't change.

---

## 8. Let rehearsal cover updates and removals, not just fresh installs

**Done.** `run_rehearsal` takes a `RehearsalOp` — `Install`, `Update` or
`Remove` — and `deploytix rehearse update|remove` drives the last two. The
undo differs by operation, which the report now carries as a `Restoration`
rather than a `disk_wiped` bool: an install is wiped, a transaction is
discarded by `StagedTransactionGuard`, which asks the live backend what is
staged and rolls it back. Nothing staged is a success, not a failure — a
rehearsal that short-circuited before staging has nothing to undo.

The standalone `deploytix-rehearsal` binary stays install-only; a transaction
rehearsal acts on a live deployed system, which is the CLI's territory.

*(This depends on phase 2's shared backend trait existing first.)*

`deploytix rehearse` — a full dry run of the installer with everything logged,
followed by a guaranteed wipe — only knows how to rehearse a fresh install,
because `run_rehearsal()` builds an `Installer` directly and nothing else.
That's exactly backwards from a safety standpoint: an update or a package
removal against a live, already-deployed system is the operation where a
mistake is hardest to walk back, and it's the one kind of operation you
currently can't rehearse.

The underlying machinery is already reusable — the command recorder is a
generic feature of `CommandRunner`, and the GUI update tool
(`src/gui_update/state.rs`) already proves this by attaching a recorder to a
bare `CommandRunner` and handing it straight to the update/remove code with no
`Installer` involved at all. The fix is to let the rehearsal harness take
"which operation" as a parameter — install, update, or remove — and dispatch
to whichever one applies, using that same recorder pattern.

One real wrinkle: rehearsal's current safety net, `DiskWipeGuard`, restores
the disk by wiping it — which makes sense for a fresh install, but doesn't
make sense for a transaction against a system that's already live. Updates
and removals need their own kind of "undo" (delete the snapshot that was just
created, or deactivate the inactive slot that was just built) rather than a
full wipe. If that turns out to be nontrivial for removal specifically, it's
fine to ship install + update rehearsal first and treat remove as a follow-up.

---

## 9. Pull `pkgdeps` out into its own crate

**Done**, after breaking the cycle described at the top of this file:
`pkgdeps::cli::build_source` no longer composes the AUR on top of pacman. It
returns the local source and says whether `--aur` was asked for
(`wants_aur`); `main.rs` does the stacking, because knowing that the AUR
exists is the command layer's business. `MetadataSource` gained an impl for
`Box<dyn MetadataSource>` so a boxed source can still be composed.

The crate is `crates/pkgdeps`, with its own `Error` (four variants — the only
four it can produce) converted at the boundary by
`impl From<pkgdeps::Error> for DeploytixError`. Its tests moved with it. The
Makefile and CI now run the workspace-aware commands.

*(This depends on phase 3 — pulling `pkgdeps` out only works cleanly once
`utils` is something a second crate can depend on, which is exactly what
phase 3 sets up.)*

`src/pkgdeps/` (dependency resolution, tree/reverse-dependency queries, graph
output — about 2,800 lines across 7 files) is functionally its own little
tool bolted onto the side of the installer. It only reaches into the rest of
the codebase for one thing: the shared error type. Everything else it needs,
it already has internally.

Turning the root `Cargo.toml` into a Cargo workspace and moving `pkgdeps` into
its own member crate would let it have its own tests and its own release
cadence, and would stop the main installer binary from having to carry a
whole dependency-query tool inside it. The cheapest way to handle the one
remaining coupling (the shared error type) is to give `pkgdeps` its own small
error type with conversions at the boundary, rather than immediately building
a shared error crate — that can happen later if it turns out to be worth it.

---

## 10. Settle on one error-handling convention

**Done.** `anyhow` is gone from `Cargo.toml`; `main.rs` and the rehearsal
binary use the crate's own `Result`, and the one `anyhow!` call is a
`ConfigError`. The `.into()`s that had been converting `DeploytixError` into
`anyhow::Error` were removed with it.

The codebase depends on both `thiserror` (used almost everywhere,
`DeploytixError` shows up in 35 files) and `anyhow` (used in exactly two spots,
both in `src/main.rs` — the top-level `Result` alias, and a single
`anyhow::anyhow!(...)` call inside argument validation). Having both around
means every new piece of code has to guess which convention to follow.

Since `anyhow` is doing almost nothing here, the simplest fix is to drop it:
swap `main.rs`'s `Result` alias for the crate's own `DeploytixError`-based one,
and replace that one `anyhow::anyhow!` call with the existing
`DeploytixError::ConfigError` variant (already used for exactly this kind of
"bad CLI argument" error elsewhere). Then remove `anyhow` from `Cargo.toml`
entirely. This is the smallest, lowest-risk item on the list — worth doing
whenever there's a spare few minutes, even out of order.

---

## 11. Make desktop environments insertable

**Done.** `desktop/mod.rs` holds `DesktopModule`, the `module()` lookup and
`ALL`; each desktop is one `const MODULE` in its own file carrying its
packages, service packages, `.xinitrc` command, session facts and sddm
stanza. The four per-desktop `install()` functions collapsed into one generic
routine, because with the descriptor in place the only thing that differed
between them was data. `session_switching`, `display_manager`, the installer
and the GUI dropdown all read the registry; the headless module is a module
like any other rather than a special case.

Adding a fifth desktop environment today means finding all 76 places that
mention `DesktopEnvironment::`, spread across 13 files: the enum and its
`Display` impl in `src/config/deployment.rs`, the four-arm dispatch in
`src/desktop/mod.rs`, package selection in `src/install/basestrap.rs`,
display-manager and greeter config (`src/configure/display_manager.rs`,
`greetd.rs`), service enablement (`services.rs`), session switching
(`session_switching.rs`), and four GUI files that hardcode the dropdown
contents (`src/gui/panels/network_desktop.rs:143-156` lists all four by hand).
`src/desktop/` already has the right skeleton — one file per DE — but those
files only own the `.desktop` file content; every other DE-specific decision
lives out in the codebase next to the subsystem it affects.

The fix is to give each DE module the rest of its own decisions. A
`DesktopModule` descriptor — display server and DE packages, display-manager
default, theme quirks (the `Current=breeze` line at
`src/configure/display_manager.rs:73-75` is KDE knowledge sitting in a
display-manager file), session-switching units, extra service names — with one
`const` per module in `kde.rs`/`gnome.rs`/`xfce.rs`/`none.rs`, and a single
lookup in `desktop/mod.rs` replacing `generate_desktop_file`'s match. Call
sites ask the descriptor.

Two details worth planning for. First, `None` is not a degenerate case to be
special-cased away — a good chunk of the 76 sites are `!= DesktopEnvironment::None`
guards, and most become "the None module contributes no packages and no
display manager," which falls out naturally. A few are genuinely structural
(skip display-manager configuration entirely) and should stay as explicit
guards rather than being contorted into the descriptor. Second, the GUI should
iterate the registry to build its dropdown instead of listing variants — that
is the single change that makes a new DE actually show up everywhere at once.

Do this one before phase 12; DEs are the messier of the two and will shake out
the descriptor's shape. Behaviour must not change: the same config should
produce the same package list and the same generated files, which the existing
`basestrap`/`display_manager` tests already assert.

---

## 12. Make init systems insertable

**Done.** `src/init/` holds `InitModule` and one file per init, each
carrying its base package, service and enabled directories, the services that
have no `{base}-{init}` package for it, its `enable` function, and the
optional `sync_repository`/`commit_database` hooks that only s6 uses. The
three methods on `InitSystem` moved onto the descriptor — two of them had no
callers at all outside their own tests — and `configure::services` is now
init-agnostic: it asks the module rather than matching on which init it is.

Same complaint, different axis: 84 mentions of `InitSystem::` across 11 files.
This one is less tangled than the DEs, because Artix's `{package}-{init}`
naming convention means most of the variation is already mechanical — the enum
already carries `base_package()`, `service_dir()` and `enabled_dir()`. What's
scattered is the exceptions: per-init service-package blacklists in
`src/configure/services.rs`, the greeter wiring in `greetd.rs`, swap unit
handling in `swap.rs`, `grub-btrfs` unit naming in `grub_btrfs.rs`, and a KDE
interaction in `src/desktop/kde.rs`.

The fix is to finish the job the enum's three methods started: an `InitModule`
descriptor carrying the full per-init surface — base package, service and
enabled directories, service-name mangling, the blacklist, and the handful of
unit files a given init needs written differently — with the existing methods
folded into it. `configure/services.rs` becomes init-agnostic: it asks the
module how to name and enable a service instead of matching on which init it
is.

Because the mechanical parts are already centralised, the real value here is
narrower than phase 11 and mostly shows up the day someone adds a fifth init
system. Sequence it accordingly — it's the least urgent of the insertable-module
phases, and it's the natural place to *stop* if the pattern turns out to cost
more than it saves in phases 2 and 11. That's a legitimate outcome; note it in
this file and move on rather than pushing the pattern into a place it isn't
earning its keep.

---

## Checking the work

Now that the workspace split has landed, these are the workspace-aware forms,
and they are what the Makefile and CI run:

```bash
cargo build --workspace
cargo clippy --workspace --all-features -- -D warnings
cargo test --workspace --all-features
```

All three were clean after every phase.

One check the tests cannot make on their own: an install writes to real disks
and needs root, so `deploytix install --dry-run -c <config>` was never run
end-to-end here. What stands in for it is the set of pipeline tests in
`installer.rs`, which assert the step list, its order, and the conditions
under which each step is included — against a config rather than against a
disk. Before trusting a change to `pipeline()` on real hardware, run the
dry-run install as root and read the command sequence.

The config surface is the other thing worth re-checking by hand after touching
phases 11 or 12: `DesktopEnvironment` and `InitSystem` are what `deployment.toml`
serialises, so a regression there is a regression in every saved config, not
just in the code.
