# AUR packages and dependency management in the Updater GUI

Status: **partly implemented.** Phases 1, 2 and 4 have landed, along with the
dependency preview from Phase 5. Phases 3, 6 and 7 — the build transaction,
the review step and the history record — are still design.

## What this is for

`deploytix-update-gui` today can do three things: upgrade everything, install
named repo packages, and install local `.pkg.tar.zst` files
(`src/gui_update/state.rs:87` and `:252`). All three end up in the same
transaction — `run_in_new_set` in `src/immutable/update.rs:259` — which
snapshots the running trio, runs pacman in a chroot on the new set, and moves
the boot pointer on success.

What it cannot do is anything with the AUR, and it cannot tell you what a
package will drag in before you commit to it. Both gaps matter more on an
immutable system than on a normal one, because `/usr` is read-only: a user
cannot fix a bad outcome afterwards with a quick `pacman -R`. The whole point
of the transactional model is that you find out *before* the reboot.

Two thirds of the machinery for this already exists and is unused by the GUI:

| Already in tree | Where | Currently used by |
|---|---|---|
| Dependency resolution, closures, reverse deps, install plans | `src/pkgdeps/` (2,830 lines) | `deploytix deps` CLI only |
| A pluggable metadata backend trait | `src/pkgdeps/source.rs:17` | pacman backend + `MockSource` |
| Chroot-scoped planning (`--root`, `--dbpath`) | `src/pkgdeps/cli.rs` | CLI only |
| A yay invocation that runs as a named user in a chroot | `src/configure/packages.rs:67` | installer only |
| An approve/edit/skip review hook, with a `Yay` kind | `src/utils/interactive.rs:26`, `:173` | installer GUI |
| Protected-package refusal (kernel, `/boot`, cryptsetup) | `src/immutable/remove.rs:331` | `deploytix remove` |
| A generic transaction taking a `body` closure | `src/immutable/update.rs:259` | update, remove |

So this is mostly a wiring job across existing seams, plus one genuinely new
component (an AUR metadata source) and one real bug that has to be fixed first.

---

## The five constraints that shape the design

These were established by reading the code, and they are the reason the phases
are ordered the way they are. None of them are visible from the GUI layer.

### 1. The update chroot's `/tmp` is a RAM-backed tmpfs

`chroot_api_setup_cmd` mounts a fresh tmpfs over the target's `/tmp`
(`src/utils/command.rs:92`), and `mount_set_cmd` does not bind the host's
`/tmp` into the set at all (`src/immutable/update.rs:71` binds only `var`,
`home`, `boot`, plus the writable binds).

Anything that builds under `/tmp` in that chroot therefore gets **half of
physical RAM** of scratch space — the incident `docs/TMP_DISK_BACKED.md` was
written about: a linux-tkg tree filling an 11 GiB tmpfs on a 21 GiB machine
while the root volume had 67 GiB free. The disk-backed `@tmp` subvolume solved
that for the *booted* system and does nothing for the *update chroot*.

Being precise about the exposure, because an earlier draft of this document
overstated it: `makepkg`'s default `BUILDDIR` is the directory holding the
PKGBUILD, so a helper that clones into the user's cache already builds on disk.
What lands on the tmpfs is anything naming `/tmp` explicitly — which included
`install_yay`, building in `/tmp/yay-build`, and includes PKGBUILDs and vendor
tools that do the same.

**Landed.** `src/aur/build.rs` puts the scratch on `/var/cache/deploytix/build`
and exports `BUILDDIR`, `SRCDEST`, `PKGDEST`, `SRCPKGDEST`, `LOGDEST` and
`TMPDIR` from `makepkg`'s own environment — one mechanism that works for every
helper, rather than per-helper build-directory flags that differ or do not
exist. `install_yay` builds there instead of `/tmp/yay-build`.

### 2. `makepkg` refuses to run as root, and the updater is root

`deploytix-update-gui` is launched through polkit with `auth_admin`
(`com.deploytix.update-gui.policy`), so the whole process is root. `makepkg`
exits rather than build as root, which is why the installer's path takes a
`username` and shells out via `sudo -u`
(`src/configure/packages.rs:67`, `:734`).

The installer knows that username because it is in the deployment config. The
updater, running on an already-deployed system months later, has no config and
no equivalent. Picking the build user is an open decision (below), not
something to infer silently.

### 3. yay may simply not be there

`install_yay` is opt-in and defaults to false. Nothing in the tree did a
runtime check for it. So the updater cannot assume a helper exists, and
"install yay for me" is itself a transaction (it needs `go`, `git`,
`base-devel` and a build user).

**Landed.** `src/aur/helper.rs` detects paru, yay, pikaur, trizen and aura and
drives whichever is present, preferring them in that order. `aura` is handled
separately throughout because it runs as root and takes `-A`, where the rest
must drop privileges and take `-S`.

### 4. The pacman DB and the build cache are on shared, non-snapshotted storage

`docs/IMMUTABLE_SYSTEM.md` already documents the database caveat: `/var` is
shared across sets, so a rolled-back transaction leaves the DB describing files
that are no longer there. AUR makes this worse in a second way — yay's cache
lives in `~/.cache/yay`, and `/home` is shared too (`@home` is not part of the
`{@, @usr, @etc}` set).

This is not fixable within this work, and pretending otherwise would be worse
than stating it. The plan's answer is honesty: record what was installed, and
say plainly in the rollback UI that an AUR package's DB entry survives a
rollback even though its files do not.

### 5. `pkgdeps` is deliberately sync-DB-only

`src/pkgdeps/mod.rs` says it resolves "from the pacman/libalpm sync database —
never by scraping the Artix website." AUR packages are in no sync DB, so they
were invisible to every existing resolver path. The trait boundary
(`MetadataSource`, `src/pkgdeps/source.rs:17`) is the right place to add them,
and its contract — "implementations MUST NOT mutate system state" — is exactly
what an AUR RPC client should honour.

**Landed.** `src/aur/rpc.rs` and `src/aur/source.rs`. The AUR is a second
`MetadataSource`, and `CompositeSource` asks the repositories first and the AUR
only for what they lack — so one closure spans both universes and an all-repo
selection still makes no network calls.

---

## Phases

Each phase builds, lints and tests clean on its own, and each is useful even if
the next one never lands. Do them in order; only 4 and 5 can overlap.

### Phase 1 — Give the update chroot disk-backed build scratch

Fix constraint 1 before anything depends on it. Add a build directory on
writable, disk-backed storage and bind it into the set, rather than letting
builds land on the tmpfs.

`/var/cache/deploytix/build` is the natural home: `/var` is already rbound into
every set by `mount_set_cmd`, so it needs no new mount plumbing and it is
shared across sets (which is right for a build cache — it should survive the
transaction that produced it). The alternative, binding the `@tmp` subvolume
in, couples the update path to the btrfs backend for no extra benefit.

Then point the builder at it explicitly: `makepkg` honours `BUILDDIR`, and yay
honours `--builddir`. Do not rely on `TMPDIR` alone — the whole lesson of
`TMP_DISK_BACKED.md` is that things which hardcode `/tmp` are the ones that
break.

Verify by building something large in a chroot and watching `df` on the root
volume move rather than on tmpfs. This phase alone also helps the existing
`install_tkg_kernel` path.

### Phase 2 — Runtime capability detection

Answer, on the live system, before any transaction: is a helper installed and
which one; is there a plausible build user; which backend is running; is
`base-devel` present. Surface it in the **System** tab, which already renders
backend and boot-pointer facts (`src/gui_update/panels/system.rs`).

Model it as a `struct AurCapability` with an explicit "why not" for each
missing piece, so the Update tab can grey out the AUR field with a reason
rather than failing at transaction time. Pure detection, no mutation — cheap to
test with fixtures.

### Phase 3 — The AUR transaction body

`run_in_new_set` already takes a `body: FnOnce(&CommandRunner, &str, &str)`
and handles snapshot, mount, activate, prune and cleanup-on-failure. An AUR
install is a new `body`, not a new transaction type.

The body should:

1. Refuse on the LVM A/B backend, matching what `deploytix remove` already does
   (`src/main.rs`) — running the btrfs path against a root that is not the one
   that boots is the same hazard here.
2. Run the resolved set through `check_protected` (`src/immutable/remove.rs:331`)
   before building. An AUR package that replaces a kernel or writes into
   `/boot` is exactly as dangerous as a removal that takes one out, and `/boot`
   is shared across every set.
3. Build and install as the chosen user via the existing
   `yay_install_chroot_reviewed` shape, with `BUILDDIR` from Phase 1.
4. Return `history::PackageChanges` so the Snapshots tab shows what the set
   contains.

Expose it as `deploytix aur <pkg>...` first. A CLI entry point is testable in
the rehearsal harness and in a VM without touching egui, and it keeps the GUI a
thin caller — the same shape `update` and `remove` already have.

### Phase 4 — An AUR metadata source for `pkgdeps` — **landed**

`src/aur/rpc.rs` is a read-only client for `/rpc/v5`, mapping the endpoint's
`Depends`/`MakeDepends`/`OptDepends`/`Provides`/`Conflicts` onto the existing
`Package` and `Dep` types. No new model types were needed, which was the signal
that the mapping was right. Version constraints and optdepend descriptions come
through the existing `Dep::parse`, since AUR tokens use pacman's syntax.

Fetching is `curl` in a subprocess behind an `HttpGet` trait, not a new crate:
the tree already fetches that way (Warp, linux-tkg), and adding `reqwest` would
pull an async runtime and a TLS stack into a binary that is otherwise a
collection of process invocations. The trait is what makes the whole thing
testable against canned JSON with no network.

`src/aur/source.rs` composes it with the pacman source, repositories first.
That ordering matters three ways: it matches what a helper actually installs
(a name in both places comes from the repo), it keeps the local source on the
hot path so all-repo resolution makes no requests, and it means an unreachable
AUR degrades to the previous behaviour rather than breaking repo resolution.
Lookups are memoised including negatives — "the AUR does not have `glibc`"
is otherwise asked once per package that depends on it — and roots are
prefetched in one batched request.

Deferred from the original sketch: the on-disk cache with a TTL. The cache
lives as long as the source, and callers build one per resolve, so there is no
staleness window to reason about and nothing to invalidate. Add it if repeated
resolves prove slow in practice.

Exposed on the CLI as `deploytix deps <cmd> --aur`, off by default because it
turns a local query into a networked one.

### Phase 5 — Search and dependency preview in the GUI

With Phase 4 in place the Update tab can stop being a bare text field:

- A search box querying both universes, with results tagged `repo` or `AUR`.
- For a selected package, the resolved closure from `pkgdeps` — what is already
  installed, what is new, total download and installed size, and which
  dependencies come from the AUR and therefore need building.
- Explicit optdepends, opt-in rather than silently pulled.

Run every query on the worker-thread pattern the app already uses (`Msg`
channels, `start_refresh`, `src/gui_update/state.rs:276`); the UI thread must
never block on the network. Resolution is read-only, so it needs no polkit
escalation and no transaction.

### Phase 6 — Review before commit

`InteractivePolicy` (`src/utils/interactive.rs:173`) already exists, the
installer GUI already implements it, and `review_pacman`
(`src/utils/command.rs:337`) already routes every user-facing invocation
through it with approve / edit / skip / cancel. Implement the same trait in the
updater GUI and attach it to the `CommandRunner` the transaction builds.

That gives a final "this is what will be built and installed" confirmation for
free, consistent with the installer, with no new confirmation machinery.

### Phase 7 — Honest history and rollback

Record AUR packages in the update history distinctly from repo packages, so the
Snapshots tab can show which restore points contain built packages. Then state
the constraint-4 caveat where the user will actually meet it: in the rollback
confirmation dialog, for sets whose changes include AUR packages.

---

## Open decisions

These need a human answer; do not guess them in code.

1. **Which user builds?** Options: the first non-root user with a login shell
   and a `wheel` membership; the owner of the invoking desktop session, which
   polkit knows (`PKEXEC_UID`); or an explicit setting persisted at install
   time. `PKEXEC_UID` is the most honest — it is the person who clicked the
   button — but it fails for a headless invocation.
2. **yay, or paru, or neither?** The installer builds yay from source. paru
   needs no Go. A third option is calling `makepkg` directly and letting
   `pkgdeps` do the resolution, which removes the helper dependency entirely at
   the cost of implementing build ordering.
3. **Does the updater offer to install a missing helper?** It is a transaction
   in its own right, and it pulls `base-devel` into the immutable root
   permanently.
4. **LVM A/B: refuse, or implement?** Refusing matches `deploytix remove` and is
   the safe default. Implementing means an rsync-into-inactive-slot build path.
5. **Network trust.** The AUR RPC is plaintext metadata about arbitrary
   user-submitted PKGBUILDs. The immutable model's value is that a bad update is
   reversible, which is a mitigation, not an answer. Decide whether AUR support
   is opt-in per system.

---

## Checking your work

After each phase:

```bash
cargo build --all-features
cargo clippy --all-features -- -D warnings
cargo test --all-features
```

Beyond the gates, each phase has a specific proof:

| Phase | Proof it worked |
|---|---|
| 1 | A large build in the update chroot consumes root-volume free space, not tmpfs; `df` inside the chroot shows the bind, not a half-RAM `tmpfs` on `/tmp` |
| 2 | Detection reports correctly on a system with yay, one without, and one with no non-root user |
| 3 | `deploytix -n aur <pkg>` prints a plan and changes nothing; a forced build failure leaves the running system and the set count untouched |
| 4 | ✅ `deploytix deps resolve <aur-pkg> --aur` returns a closure spanning both universes; resolver tests pass offline against `MockSource`, and a loopback HTTP server exercises the real curl path |
| 5 | Search and preview never block the UI thread; results match Phase 4's CLI output for the same package |
| 6 | Cancelling at review runs no build and creates no set |
| 7 | A set containing AUR packages is labelled as such, and its rollback dialog names the shared-database caveat |

Phase 3 is the one that must be exercised in a VM or through the rehearsal
harness rather than trusted to unit tests: it moves the boot pointer, and a
mistake there is a machine that does not come back.
