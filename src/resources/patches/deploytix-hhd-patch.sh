#!/bin/sh

# deploytix-hhd-patch — (re-)apply deploytix's local patches to the installed
# Handheld Daemon.
#
# hhd is installed from the AUR (`hhd-git`), so its files are package-owned:
# every rebuild of that package overwrites them and reverts the patches. This
# script re-applies them, and deploytix runs it once at install time.
#
# Run it again after any `yay -S hhd-git` / `pacman -Syu` that rebuilt hhd:
#
#     sudo deploytix-hhd-patch
#
# Self-disabling by design. Each patch is dry-run first, and a patch that no
# longer applies is skipped, not forced — so once a fix lands upstream and the
# rebuilt package already contains it, this quietly becomes a no-op and the
# patch file can be deleted. It never exits non-zero for an unapplied patch:
# it is a best-effort repair, not a gate.
#
# Patches are unified diffs taken against the upstream *source* tree
# (a/src/hhd/...), while the installed tree is <site-packages>/hhd/..., hence
# -p2.

set -u

PATCH_DIR="${DEPLOYTIX_HHD_PATCH_DIR:-/usr/share/deploytix/patches}"

if ! command -v patch >/dev/null 2>&1; then
    echo >&2 "deploytix-hhd-patch: 'patch' not found (install base-devel)"
    exit 1
fi

# The installed hhd package, wherever this Python happens to put it.
hhd_dir=""
for candidate in /usr/lib/python3*/site-packages/hhd /usr/lib64/python3*/site-packages/hhd; do
    if [ -d "$candidate" ]; then
        hhd_dir="$candidate"
        break
    fi
done

if [ -z "$hhd_dir" ]; then
    echo >&2 "deploytix-hhd-patch: no installed hhd found under /usr/lib/python3*/site-packages"
    exit 1
fi

site_packages="$(dirname "$hhd_dir")"
echo "deploytix-hhd-patch: target $site_packages"

applied=0
skipped=0
for p in "$PATCH_DIR"/hhd-*.patch; do
    [ -f "$p" ] || continue
    name="$(basename "$p")"
    if ! patch -p2 -d "$site_packages" --forward --dry-run <"$p" >/dev/null 2>&1; then
        # Already applied, or upstream changed underneath it. Either way,
        # forcing it would do more harm than leaving it alone.
        echo "  $name: does not apply (already applied, or fixed upstream) — skipping"
        skipped=$((skipped + 1))
        continue
    fi
    if patch -p2 -d "$site_packages" --forward <"$p" >/dev/null 2>&1; then
        echo "  $name: applied"
        applied=$((applied + 1))
    else
        echo >&2 "  $name: dry run passed but apply failed"
        skipped=$((skipped + 1))
    fi
done

echo "deploytix-hhd-patch: $applied applied, $skipped skipped"

# Stale bytecode would shadow the patched sources on the next start.
if [ "$applied" -gt 0 ]; then
    find "$hhd_dir" -name __pycache__ -type d -exec rm -rf {} + 2>/dev/null
    echo "deploytix-hhd-patch: cleared hhd __pycache__; restart hhd to pick the change up"
fi

exit 0
