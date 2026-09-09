# Composing Staged Updates Between Reboots

Status: **design / not implemented**
Applies to: `deploytix update`, `deploytix rollback`, both immutable backends

---

## 1. The problem

> Installing or updating once sets the system to reboot into that set. If a
> second install or update is performed before the next reboot, the first one is
> forgotten.

Confirmed in code on both backends. They fail differently, and the LVM A/B
failure is considerably worse than "forgotten".

### 1.1 btrfs backend — the staged set is orphaned

`src/immutable/update.rs`, `run_in_new_set`:

```rust
let running = boot::running_set_id();
let source  = boot::running_subvols();      // read from /proc/cmdline
let id = snapshot::create_set(cmd, &devices, &source, /* readonly = */ false)?;
…
boot::activate_target(cmd, &devices, &snapshot::set_root_subvol(&id))?;
prune_sets(cmd, &devices, opts.keep_sets, &running, &id)?;
```

`running_subvols()` reads `rootflags=subvol=` from `/proc/cmdline` — that is
what the initramfs actually mounted, so it names the **booted** set and cannot
name a set staged since boot.

Two updates in one session therefore go:

| Step | Booted | Source snapshotted | Set built | Boot pointer |
|---|---|---|---|---|
| install | `@` | — | — | `@` |
| update 1 (`pacman -Syu`) | `@` | `@` | `set-1` | `set-1` |
| update 2 (`install foo`) | `@` | **`@`** | `set-2` | `set-2` |
| reboot | `set-2` | | | |

`set-2` was branched from `@`, so it never contained update 1's packages. The
system boots with `foo` installed and the `-Syu` undone.

The module header states the intended behaviour:

> once an update has been activated the running trio is a snapshot set, and
> snapshotting it is what makes successive updates stack rather than each one
> rebasing onto the install-time base

That is true only *across* reboots. Within one session, updates do not stack —
they race, and the last one wins.

A second-order effect: `prune_sets(…, &running, &id)` protects the running set
and the newly built one. `set-1` is neither, so once `keep_sets` is exceeded it
is not merely bypassed but deleted.

### 1.2 LVM A/B backend — the update writes to the running slot

`src/immutable/lvm_ab.rs`, `run_update`:

```rust
let state  = read_state()?;
let active = state.active.clone();
let target = ab::other_slot(&active).to_string();
```

`state.active` is written by `activate_slot` at the *end* of an update, so it
means **"the slot that boots next"**, not "the slot running now". Nothing on
this backend ever reads `deploytix.slot=` from `/proc/cmdline`, so the running
slot is never established.

| Step | Running | `state.active` | Target chosen | rsync source |
|---|---|---|---|---|
| install | A | A | — | — |
| update 1 | A | A → **B** | B | `/` (= A) ✅ |
| update 2 | **A** | B → **A** | **A** | `/` (= A) ❌ |

Update 2 picks `other_slot("B")` = **A**, the slot the machine is running from.
It then:

1. `mount /dev/<vg>/root_a /run/deploytix-slot/A` — mounting the dm-verity
   **data device** of the live root read-write, while the live root is mounted
   read-only on top of it;
2. `rsync -aHAX --delete / /run/deploytix-slot/A/` — source and destination are
   the same filesystem;
3. runs `pacman` and `mkinitcpio` against it;
4. `veritysetup format`s a new hash over a device that is being written to;
5. repoints the boot pointer back to A.

Update 1's slot-B work is discarded, and every write in steps 1–3 invalidates
the running system's verity tree. Any subsequent read of a changed block on the
live root fails integrity verification. This is a data-integrity bug, not just a
lost update.

### 1.3 Why this is not a rollback problem

`rollback` shares the same conflation (`state.active` on A/B,
`current_boot_pointer` on btrfs) but is safe: it only moves a pointer and writes
nothing to either slot.

---

## 2. What "compose" has to mean

The requested behaviour:

> Snapshot sets with written updates/installs set for next boot should be
> composed into one set continuously, to be exchanged for another set during the
> next session, on next boot.

Restated as an invariant:

> **At most one staged set exists at a time. Every update between two reboots
> extends that same staged set. The running set is never written to.**

This gives:

- update 1 then update 2 then reboot → one set containing both;
- rollback still returns to the set that was running, untouched;
- exactly one boot-pointer target at all times, so grub.cfg never accumulates
  half-applied history.

Three states, per session:

```
  ┌── no staged set ──┐   first update      ┌── staged set S ──┐
  │  running = R      │ ──────────────────> │  running = R     │
  │  pointer  = R     │                     │  pointer  = S    │
  └───────────────────┘                     └──────────────────┘
            ^                                     │      │
            │  reboot (R := S, S := none)         │      │ further updates
            └─────────────────────────────────────┘      │  extend S in place
                                                         └──────┘
```

The distinction the code must make, and currently does not, is between:

- **running** — what the initramfs mounted; authoritative source `/proc/cmdline`
  (`rootflags=subvol=` / `deploytix.slot=`);
- **staged** — what boots next; authoritative source `/etc/default/grub` +
  grub.cfg (btrfs) or `STATE_FILE` (A/B).

`boot::running_root_subvol()` and `boot::current_boot_pointer()` already
implement exactly this distinction on the btrfs side, with a doc comment warning
that confusing them "is how the running system ends up unprotected". `run_update`
simply does not use the second one. The A/B backend has no equivalent at all.

---

## 3. Design

### 3.1 A shared notion of session state

Add to `src/immutable/mod.rs` (backend-independent):

```rust
/// What is running now versus what boots next.
pub struct SessionState {
    /// The set/slot the initramfs actually mounted (from /proc/cmdline).
    pub running: String,
    /// The set/slot selected for the next boot.
    pub staged: String,
}

impl SessionState {
    /// A set staged this session that has not been booted yet.
    /// `None` when the pointer still names the running set.
    pub fn pending(&self) -> Option<&str> {
        (self.staged != self.running).then_some(self.staged.as_str())
    }
}
```

`pending()` is the whole decision: `Some(s)` means compose into `s`, `None`
means branch a fresh set from `running`.

### 3.2 btrfs backend

Change `run_in_new_set` to pick its source and its target from `pending()`:

```rust
let running = boot::running_set_id();
let staged  = boot::pointer_set_id(&boot::current_boot_pointer(cmd)?)
                  .unwrap_or_else(|| crate::immutable::ROOT_SUBVOL.into());

let (id, source) = match SessionState { running, staged }.pending() {
    // Compose: reuse the staged set, writing into it directly.
    Some(pending) => (pending.to_string(), None),
    // Fresh: branch from the running trio, as today.
    None => {
        let src = boot::running_subvols();
        (snapshot::create_set(cmd, &devices, &src, false)?, Some(src))
    }
};
```

The staged set is already writable (`create_set(…, readonly = false)`), already
carries a `.deploytix-pair` marker, and is already mounted by `mount_set_cmd`
the same way — so composing is *fewer* operations than branching, not more.
After the transaction, `activate_target` is re-run on the same subvolume, which
is idempotent.

Pruning must protect both ends: `prune_sets(…, &running, &id)` already keeps the
running set and the set just written; when composing, those two arguments are
the pending set and the running set, which is exactly right.

**Set identity.** Ids are epoch seconds and are used for ordering. A composed
set keeps its original id, so an update at 10:00 composed into at 10:30 still
sorts as 10:00. That is correct — it is the same staged generation — but it
means the id no longer records when the set last changed. Record composition
events in the history file (§3.4) rather than renaming the set, since renaming a
subvolume that the boot pointer names is a needless risk.

### 3.3 LVM A/B backend

Two changes, the first of which is a bug fix regardless of composition:

**(a) Establish the running slot.** Add:

```rust
/// The slot the initramfs actually booted, from `deploytix.slot=` on the
/// kernel cmdline. Falls back to the state file's `active` only when the
/// cmdline says nothing (an install predating the parameter).
pub fn running_slot() -> String { … }
```

mirroring `boot::running_root_subvol()`, including "last occurrence wins".

**(b) Choose the target from the running slot, never from `state.active`:**

```rust
let running = running_slot();
let target  = match state.active != running {
    // A set is already staged in the other slot — compose into it.
    true  => state.active.clone(),
    // Nothing staged — build into the slot we are not running from.
    false => ab::other_slot(&running).to_string(),
};
```

This makes `target != running` an invariant. Assert it and refuse to proceed
otherwise:

```rust
if target == running {
    return Err(DeploytixError::ConfigError(
        "refusing to build into the running slot".into(),
    ));
}
```

That guard alone prevents the integrity bug in §1.2 even if the selection logic
is later changed.

**(c) rsync only when the slot is fresh.** Composing must not re-rsync: the
staged slot already holds update 1's result, and `rsync --delete` from the
running root would revert it. Skip the rsync when composing and go straight to
`pacman` in the chroot. The verity re-seal (`veritysetup format`) and pointer
rewrite then run as they do today.

State-file semantics need a matching clarification: `active` should be renamed
or documented as `staged`/`next`, because it has never meant "running".

### 3.4 History

`history::UpdateRecord` gains a field distinguishing a fresh set from a
composition, so `deploytix` can report "3 updates composed into set 1739…"
rather than three records that look like they each produced a boot target.

### 3.5 Interaction with the shared `/boot`

`/boot` is shared and not snapshotted on either backend, so the kernel and
initramfs are already "last write wins" across sets — composing does not change
that. It does make it *more* correct: today, two updates in a session leave
`/boot` holding update 2's initramfs while the pointer names a set built from
neither; after composing, `/boot` and the staged set agree.

---

## 4. Edge cases

| Case | Required behaviour |
|---|---|
| Update fails while composing | The staged set is left as it was *before this transaction*, not deleted — deleting it would discard the earlier successful update. Needs a pre-transaction btrfs snapshot of the staged set as an undo point, or an explicit "partially composed" marker. **This is the one place composing is strictly harder than branching.** |
| `rollback` with a set staged | Should discard the staged set and return the pointer to `running` — that is what a user means by "undo what I staged". Today it moves the pointer backwards from the staged set, which can select a set older than the one running. |
| Reboot between updates | `pending()` returns `None` (pointer == running), so the next update branches fresh. No special handling. |
| Pointer names a pruned/missing set | Treat as no staged set and branch fresh; warn. |
| Two `deploytix update` runs concurrently | Neither backend takes a lock today. Composing makes an overlap actively destructive (two writers in one set), so a lockfile — `/run/deploytix-update.lock`, `flock` — becomes a prerequisite, not a nicety. |
| A/B: only two slots | Composing needs no third slot; that is a point in its favour. |

---

## 5. Implementation plan

| WP | Change | Risk |
|---|---|---|
| **WP1** | A/B: add `running_slot()`; select target from it; assert `target != running`. Ship this alone — it fixes the integrity bug without changing update semantics. | Low, high value |
| **WP2** | `SessionState` + `pending()` in `immutable/mod.rs`, with unit tests. | Low |
| **WP3** | btrfs: compose in `run_in_new_set`. | Medium |
| **WP4** | A/B: compose (skip rsync when the target is already staged). | Medium |
| **WP5** | Undo point for a failed composition (§4 row 1). | Medium |
| **WP6** | `flock` around the whole update. | Low |
| **WP7** | `rollback` discards a staged set rather than stepping past it. | Low |
| **WP8** | History records composition; `deploytix update`'s final message says "composed into" vs "staged". | Low |

WP1 should not wait for the rest.

---

## 6. Tests

**Unit**

- `pending()` returns `None` when pointer == running, `Some` otherwise.
- A/B target selection: fresh → other slot; staged → the staged slot; never the
  running slot, for all four (running, active) combinations.
- The `target != running` assertion fires when state is inconsistent.
- btrfs: composing reuses the staged id; branching mints a new one.
- Pruning never removes the running set or the staged set.

**Integration** (VM, both backends)

1. `update` → `update` → reboot → both updates present. *This is the reported
   bug; it must fail before WP3/WP4 and pass after.*
2. `update` → reboot → `update` → reboot → both present (no regression in the
   across-reboot path that works today).
3. A/B: `update` → `update` → verify `root_<running>` was never mounted rw
   (check the running slot's verity hash still validates).
4. `update` → `rollback` → the staged set is discarded, pointer == running.
5. Failed second update → the first update's staged set survives intact.
6. Concurrent `deploytix update` → second exits on the lock, does not corrupt.

---

## 7. Summary

The btrfs backend loses the first update because it always branches from the
booted set. The LVM A/B backend does that *and* selects the running slot as its
build target, writing to a live dm-verity image. Both follow from one missing
distinction — "what is running" versus "what boots next" — which the btrfs
boot module already documents and provides, and which nothing on the A/B side
has at all.

Composing is the fix, and it is mostly a matter of *reusing* the staged set
instead of ignoring it. The A/B target-selection guard (WP1) is a small,
self-contained change that removes a data-integrity hazard and should land
first.
