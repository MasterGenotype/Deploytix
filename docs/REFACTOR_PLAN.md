# Architecture refactor plan

Status: **not started — this is a reference document for future work, not an
active plan.**

## Why this document exists

A mindmap of the codebase (`~/Downloads/deploytix-flow-and-architecture.minder`,
generated from commit `ce49fdd` plus the current working tree) ends with a
list of ten places where the code works fine today but will make the *next*
change harder than it needs to be. None of these are bugs. They're just spots
where the current shape will push back the next time someone wants to add a
feature, a third backend, or a second host.

This document turns that list into ten concrete, independent pieces of work,
in the same order the mindmap put them (roughly biggest payoff for least
effort, first). The file/line references below were checked against the real
source, not just the mindmap's notes — but the tree is actively changing, so
re-check line numbers before actually touching code.

**How to use this when the time comes:** pick it up on top of whatever the
working tree looks like then — don't wait for other in-flight work to land
first. Do one phase at a time: make the change, run
`cargo build && cargo clippy --all-features -- -D warnings && cargo test --all-features`,
confirm it's clean, then move to the next phase. Don't try to do several at
once.

---

## 1. Turn the install pipeline into a list, not a wall of code

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

---

## 3. Put a seam between "run a command" and "run it on this particular host"

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

## Checking your work

After each phase:

```bash
cargo build
cargo clippy --all-features -- -D warnings
cargo test --all-features
```

All three should stay clean the whole way through — each phase is scoped
narrowly enough that it shouldn't need to break the build to get to the next
one. If phase 9 (the workspace split) has already landed, use the
workspace-aware versions of these commands instead (`cargo build --workspace`,
etc.).
