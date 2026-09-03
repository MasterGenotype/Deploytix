# grub-btrfs generator fixtures

Verbatim copies of upstream grub-btrfs's `/etc/grub.d/41_snapshots-btrfs`,
used by the `patch-grub-btrfs-integrity` tests in `src/configure/bootloader.rs`
to prove the compat patch applies to the real script rather than to a
hand-written approximation.

| File | Source |
|------|--------|
| `41_snapshots-btrfs-4.13` | https://github.com/Antynea/grub-btrfs, tag `4.13` (the release packaged by Arch/Artix; `GRUB_BTRFS_VERSION=4.12-master-2023-04-28`) |
| `41_snapshots-btrfs-master` | https://github.com/Antynea/grub-btrfs, `master` as of `GRUB_BTRFS_VERSION=master-2026-08-24T12:56:33+00:00` |

Upstream is GPL-3.0-or-later, the same licence as deploytix. Do not edit these
files; refresh them from upstream instead.
