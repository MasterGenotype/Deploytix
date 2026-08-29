# Home-Preserving Recovery Install — Design

Reinstall the system onto an existing disk while keeping the existing
`/home` volume and its data. On a multi-LUKS encrypted install the user
supplies the keyfile (or passphrase) that unlocks the existing HOME
container; deploytix adopts that container instead of recreating it.

Status: **design only**. Nothing in this document is implemented.

---

## 1. Findings

### 1.1 `preserve_home` is documented but does not exist

`README.md:200`, `CLAUDE.md:46` and `deploytix.toml:20` all reference a
`preserve_home` flag, and `docs/deploytix-validation.md` cites two functions
that implement it — `verify_existing_partitions` (installer.rs:605-664) and
`mount_partitions_preserve` (chroot.rs:27-43). Neither function exists, and
`grep -rn preserve_home src/` returns nothing. The stale
`preserve_home = false` key in `deploytix.toml` is silently ignored by serde,
which does not deny unknown fields.

This is greenfield work, not an extension of something already partly built.
Reconciling those docs is part of the task.

### 1.2 Five places assume a blank disk

| # | Location | Behaviour |
|---|---|---|
| 1 | `apply_partitions` (partitioning.rs:112) | `wipefs -a <device>`, then `sfdisk` with a whole-disk script |
| 2 | `generate_sfdisk_script` (partitioning.rs:36) | Lays partitions sequentially from LBA 2048; HOME is last with `size_mib = 0` taking the remainder, so any size change upstream shifts its start sector |
| 3 | `setup_multi_volume_encryption` (encryption.rs:508) | Unconditionally `luksFormat`s every LUKS partition — destroys the existing header and all keyslots |
| 4 | `format_multi_volume_partitions` (installer.rs:1334) | `mkfs` on every mapped container |
| 5 | `mount_multi_volume_with_subvolumes` (installer.rs:1458) | Calls `create_btrfs_subvolumes` (formatting.rs:536), a bare `btrfs subvolume create` that fails with EEXIST on an existing filesystem |

### 1.3 Both credential paths are password-only

- `open_luks` / `luks_open` (encryption.rs:214, 228) pipe a password to
  `cryptsetup open` stdin. There is no `--key-file` path.
- `add_keyfile_to_luks` (keyfiles.rs:61) pipes a password to
  `cryptsetup luksAddKey`. Recovery needs `--key-file <supplied>` as the
  *unlocking* credential when adding the new generated keyfile.

### 1.4 No partition-table reader exists

`get_device_info` reads whole-disk sysfs attributes; `list_block_devices`
enumerates disks. Nothing parses an existing GPT. `blkid` is shelled out to
in exactly one place (`get_partition_uuid`, formatting.rs:259).

### 1.5 User creation collides with a populated home

`create_user` (users.rs:37) runs `useradd -m`. Against an already-populated
`/home/<user>`, `useradd -m` neither copies skel nor chowns the directory —
and if the new UID differs from the one owning the existing files, the user
cannot read their own data.

---

## 2. Config surface

A section rather than a bare bool, because recovery needs a credential and a
scope:

```toml
[disk.recovery]
# Reuse the existing /home volume instead of recreating it.
reuse_home = true
# Path (on the INSTALLER HOST) to the keyfile that unlocks the existing
# HOME LUKS container. Multi-LUKS installs only.
home_keyfile = "/run/media/usb/crypthome.key"
# Prompt for the HOME passphrase if the keyfile is absent or rejected.
allow_passphrase_fallback = true
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RecoveryConfig {
    #[serde(default)]
    pub reuse_home: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_keyfile: Option<String>,
    #[serde(default = "default_true")]
    pub allow_passphrase_fallback: bool,
}
```

Hung off `DiskConfig` as `#[serde(default)] pub recovery: RecoveryConfig`.

### Two different things are called "keyfile"

Conflating these is the main footgun in this feature:

- **Unlock keyfile** — user-supplied, lives on the *installer host* (USB
  stick), read once to `cryptsetup open` the existing HOME container.
- **Generated system keyfiles** — `/etc/cryptsetup-keys.d/crypt<name>.key`,
  created by `setup_keyfiles_for_volumes` (keyfiles.rs:118) and baked into the
  new initramfs by `construct_files` (mkinitcpio.rs:222).

Recovery must add a **fresh generated keyfile** to the preserved container,
unlocking with the supplied one. It must not install the supplied file as the
system keyfile — the new system's crypttab and initramfs then work exactly as
they do on a fresh install, with no special case downstream.

---

## 3. New module: `src/disk/existing.rs`

```rust
/// One partition as it exists on disk right now.
pub struct ExistingPartition {
    pub number: u32,
    pub start_sector: u64,
    pub size_sectors: u64,
    pub type_guid: String,
    pub part_uuid: String,
    pub name: Option<String>,     // GPT partition name
    pub fs_type: Option<String>,  // blkid TYPE: "crypto_LUKS", "btrfs", …
    pub fs_uuid: Option<String>,
}

pub fn read_partition_table(device: &str) -> Result<Vec<ExistingPartition>>;
pub fn find_home_partition(parts: &[ExistingPartition]) -> Option<&ExistingPartition>;
```

`read_partition_table` parses `sfdisk --json <device>`, enriched per-partition
with `blkid -o export`.

`find_home_partition` matches in order:

1. GPT partition name `HOME` — this is what `generate_sfdisk_script` writes
   (`name="{part.name}"`), so any disk deploytix installed is self-describing.
2. A `crypto_LUKS` partition whose UUID appears in an existing `/etc/crypttab`
   found on a mountable root.
3. Explicit user selection.

**Ambiguity is a hard error, never a guess.** This is the one decision where
being wrong destroys the data the feature exists to protect.

---

## 4. Pipeline changes

### Phase 1 — `prepare()`

When `reuse_home`:

1. Read the existing table; locate HOME. Hard-fail if absent, with the message
   the stale docs already promise:
   `preserve_home: expected partition HOME does not exist on /dev/sda`.
2. **Verify the credential before touching anything.**
   `cryptsetup open --test-passphrase --key-file <supplied> <home_partition>`.
   On failure, prompt for a passphrase if `allow_passphrase_fallback`; if that
   also fails, abort — *before* the confirmation prompt.
3. Read the existing HOME filesystem type. If it differs from
   `disk.filesystem`, hard-fail: that combination cannot work.
4. Compute the layout for every partition except HOME, then pin HOME to its
   existing extent.
5. Replace the confirmation text with one that says what actually happens:
   `This will ERASE ALL DATA on /dev/sda EXCEPT the existing HOME partition (/dev/sda6, 812 GiB).`

This requires `PartitionDef` to carry an optional pin:

```rust
/// Preserved from an existing table: exact placement, not recomputed.
pub pinned: Option<PinnedExtent>,   // { start_sector, size_sectors }
```

and `generate_sfdisk_script` to emit `start=` / `size=` from the pin, plus
**validate that no unpinned partition overlaps a pinned extent**. That check is
the safety net for "the user shrank ROOT and everything shifted".

### Phase 2 — `partition_disk()`

`apply_partitions` gains a `preserve: &[u32]` parameter:

- **Drop the whole-disk `wipefs -a`.** It destroys the GPT that carries the
  extent being preserved. Wipe each *non-preserved* partition individually
  instead.
- Keep sfdisk writing the full table, with HOME pinned to its original extent,
  part-UUID and type GUID. Rewriting a table entry with identical geometry is a
  no-op for that partition's contents.

*Rejected alternative:* `sfdisk --delete` the non-HOME partitions and re-add.
More round-trips, more failure modes, and sfdisk rewrites the whole header
anyway.

### Phase 2.5 — `setup_multi_volume_encryption()`

Split the per-volume work:

```rust
enum VolumeAction {
    Create,                       // luksFormat + open (current behaviour)
    Adopt { unlock: Credential }, // open only
}
enum Credential { Keyfile(String), Passphrase(String) }
```

HOME gets `Adopt` under `reuse_home`; every other volume gets `Create`.
`Adopt` calls a new `open_luks_with_keyfile` (`cryptsetup open --key-file`),
falling back to the existing `luks_open` for the passphrase case.

The returned `LuksContainer` is shape-identical either way, so
`format_multi_volume_partitions`, fstab, crypttab and the mountcrypt hook
generator need **no changes at all**. Keeping the branch at this layer is what
makes the rest of the feature small.

### Phase 2.6 — `format_multi_volume_partitions()`

Skip the adopted container. One `if` on the volume name.

### Phase 2.7 — `mount_multi_volume_with_subvolumes()`

Make `create_btrfs_subvolumes` idempotent: check `btrfs subvolume show` before
creating, and log `reusing existing subvolume @home`. Worth doing
unconditionally, not just under recovery — a bare `subvolume create` that dies
on EEXIST is a latent trap anywhere.

### Phase 3.5 / 3.6 — fstab and crypttab

**No code change.** `get_luks_uuid` (encryption.rs:393) reads the *existing*
container's UUID off disk, so both files come out correct automatically.

### Phase 3.6 — `setup_keyfiles()`

The adopted container needs a keyfile-unlock variant of `add_keyfile_to_luks`:

```
cryptsetup luksAddKey --key-file <supplied> <device> <new_generated_keyfile>
```

Everything downstream is unchanged.

**Keyslot accumulation:** each recovery install adds a keyslot. LUKS2 allows
32, so it takes many reinstalls to matter, but it is unbounded. Recommendation:
leave the old slots and log the current slot count. Killing a slot the user may
still depend on is worse than leaving a stale one.

### Phase 4 — `create_user()`

The real subtlety. With HOME mounted and `/home/<user>` already populated:

```
if reuse_home && /home/<user> exists:
    uid = stat -c %u /home/<user>
    gid = stat -c %g /home/<user>
    groupadd -g <gid> <user>                          # if that gid is free
    useradd -M -u <uid> -g <gid> -G <groups> -s /bin/bash <user>
    # -M: do not create or touch the home directory
else:
    useradd -m …                                      # current behaviour
```

Adopting the existing UID/GID is what makes the preserved files readable.
Without it the user gets a fresh UID — 1000 is usually free on a blank system,
so it *often* works by luck, and silently does not when it doesn't.

`configure_bashrc_path` (users.rs:87) already reads-then-appends and guards on
existing content, so it is safe against a populated home.

**Open decision — dotfile clobbering.** `install_autostart_entries`
(packages.rs:802) unconditionally writes
`~/.config/autostart/audio-startup.desktop`. The Steam bootstrap seeding is
already marker-guarded. Recommendation: under `reuse_home`, make the autostart
writes conditional on the file not existing, so a recovery install does not
overwrite a customised desktop.

---

## 5. Validation rules

Hard errors in `DeploymentConfig::validate`:

- `reuse_home` requires a `/home` entry in `disk.partitions` (or subvolumes
  including `@home`).
- `reuse_home` + `encryption` + `!use_lvm_thin` requires `home_keyfile` **or**
  `allow_passphrase_fallback`.
- `reuse_home` + `use_lvm_thin` → reject. The thin and A/B backends place
  `home` inside a VG, so preserving it means adopting an existing VG rather
  than a partition — a genuinely different problem, worth solving separately.
- `home_keyfile`, if set, must exist and be readable at validation time.
- `reuse_home` together with a cleanup `--wipe` → reject.

Filesystem mismatch between config and the existing HOME is a hard error in
`prepare()`, not `validate()`, since it needs the disk read.

---

## 6. CLI and GUI

```
deploytix install -c config.toml --reuse-home [--home-keyfile PATH]
```

Flags override the config file.

**`deploytix inspect <device>` — build this first.** Dump the existing
partition table, mark what a recovery install would preserve versus destroy,
and test the supplied keyfile against the HOME container. It is a thin wrapper
over `read_partition_table`, it lets the user dry-run their credential before
committing, and it is the debugging tool for everything else in this feature.

Wizard: a "reuse the existing /home?" question after device selection, shown
only when the device has a recognisable HOME partition, followed by a keyfile
path prompt.

---

## 7. Testing

Unit-testable with no disk:

- `read_partition_table` against captured `sfdisk --json` fixtures.
- `find_home_partition` selection order and ambiguity rejection.
- `generate_sfdisk_script` with a pinned extent — asserts exact `start=` /
  `size=` preservation, and that an overlapping unpinned partition is rejected.
- The validation rules above.
- The `useradd` command shape for adopted versus fresh UID.

End-to-end: the existing `src-rehearsal/` + `deploytix rehearse` harness writes
to a real disk and then wipes it, which is the right place. Install, write a
sentinel into `/home/<user>`, recovery-install, assert the sentinel survives
and is owned by the new user.

---

## 8. Implementation order

1. `src/disk/existing.rs` + `deploytix inspect` — read-only, useful alone.
2. Keyfile-unlock variants of `open_luks` and `add_keyfile_to_luks`.
3. Idempotent `create_btrfs_subvolumes`.
4. `RecoveryConfig` + validation rules.
5. Pinned extents in the layout, sfdisk, and selective `wipefs`.
6. `VolumeAction::Adopt`, skip-format, UID/GID adoption in `create_user`.
7. Reconcile the stale `preserve_home` references in `README.md`,
   `CLAUDE.md`, `deploytix.toml` and `docs/deploytix-validation.md` against
   what actually ships.

Steps 1–3 are independently valuable and land without changing any existing
install behaviour.

---

## 9. The main risk

Everything here depends on `prepare()` proving it can unlock the existing HOME
container **before** `partition_disk()` runs. If credential verification lands
anywhere later in the pipeline, a wrong keyfile means the disk is already
repartitioned and the data the feature exists to protect is gone.

That check is the feature. The rest is plumbing.
