#!/usr/bin/env bash
# deploytix-update-gamescope — rebuild and update the Deploytix gamescope
#
# gamescope on Deploytix systems is the Bazzite-maintained fork built with
# a specific set of meson options (see /usr/share/deploytix/gamescope/PKGBUILD).
# Updating it through the AUR replaces it with the upstream Valve build,
# compiled with different options, which breaks the Steam gamescope session
# (Steam fails to launch in game mode).
#
# This tool rebuilds gamescope from the same fork/branch with the exact
# same PKGBUILD — and therefore the exact same meson options — every time.
# It is the only supported way to update gamescope on a Deploytix system:
# a pacman PreTransaction hook (deploytix-gamescope-guard) aborts any
# gamescope install/upgrade not initiated by this script.
#
# ── Immutable installs ──────────────────────────────────────────────────────
# On an immutable deployment (immutable_root = true, either the btrfs
# snapshot-set backend or the LVM A/B dm-verity backend) / and /usr are
# read-only, so pacman cannot install anything into the running system.  This
# script detects that and adapts:
#
#   * the package is installed with `deploytix update <pkgfile>`, which builds
#     a new snapshot set / inactive slot and takes effect on the next reboot,
#     instead of `pacman -U` against the live root;
#   * build dependencies are never installed on the fly — they cannot be — so
#     missing ones are reported with the `deploytix update` command that adds
#     them, rather than failing deep inside makepkg.
#
# makepkg itself still runs on the live system: it only writes to the user's
# cache under $HOME, which is a shared writable subvolume on both backends.
#
# Usage:
#   deploytix-update-gamescope [--check] [--force]
#
#   --check   Only report whether an update is available (exit 0 = update
#             available, exit 10 = already up to date), do not build.
#   --force   Rebuild and reinstall even if already at the latest commit.

set -euo pipefail

PKGNAME="gamescope-git"
PKGBUILD_SRC="/usr/share/deploytix/gamescope/PKGBUILD"
GUARD_FLAG="/run/deploytix/gamescope-update-in-progress"
BUILD_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/deploytix/gamescope-update"
REMOTE="https://github.com/MasterGenotype/gamescope.git"
BRANCH="gamescope-ba"

# Immutable-backend markers.  These are the same signals deploytix itself
# dispatches on: immutable::update::ensure_immutable() checks the pairing
# marker, immutable::lvm_ab::detect() checks the slot state file.
PAIR_MARKER="/.deploytix-pair"
LVM_AB_STATE="/boot/deploytix-slots.conf"

msg()  { printf '\033[1;34m==>\033[0m \033[1m%s\033[0m\n' "$*"; }
msg2() { printf '  \033[1;32m->\033[0m %s\n' "$*"; }
err()  { printf '\033[1;31m==> ERROR:\033[0m %s\n' "$*" >&2; }

usage() {
    cat <<'EOF'
Usage: deploytix-update-gamescope [--check] [--force]

Rebuilds gamescope (Bazzite fork) from the Deploytix source branch with the
exact same meson options used at install time, then installs it.
Do NOT update gamescope through the AUR — that build breaks the Steam
gamescope session.

On an immutable installation the rebuilt package is installed with
`deploytix update`, so it becomes active on the next reboot.

Options:
  --check   Only report whether an update is available (exit 0 = update
            available, exit 10 = already up to date); do not build.
  --force   Rebuild and reinstall even if already at the latest commit.
  --help    Show this help.
EOF
}

CHECK_ONLY=0
FORCE=0
for arg in "$@"; do
    case "$arg" in
        --check) CHECK_ONLY=1 ;;
        --force) FORCE=1 ;;
        -h|--help) usage; exit 0 ;;
        *) err "unknown option: $arg"; usage >&2; exit 2 ;;
    esac
done

# ── Immutable detection ─────────────────────────────────────────────────────
# Either backend marker means the running / and /usr are read-only and pacman
# must not be pointed at them.  A read-only /usr without a marker is treated as
# immutable too: whatever put it there, `pacman -U` would fail against it.
is_immutable() {
    [[ -e "$PAIR_MARKER" || -e "$LVM_AB_STATE" ]] && return 0
    local opts
    opts="$(findmnt -no OPTIONS --target /usr 2>/dev/null || true)"
    [[ ",$opts," == *,ro,* ]]
}

IMMUTABLE=0
if is_immutable; then
    IMMUTABLE=1
fi

# Packages that must already be installed, because on an immutable system we
# cannot add them mid-build.  Reports them all at once with the command that
# installs them, rather than failing one at a time.
#
# $@ = package names.  Returns 1 (and prints guidance) if any are unsatisfied.
require_preinstalled() {
    local missing
    # `pacman -T` prints the arguments it cannot satisfy, one per line, and
    # exits 127 when there is at least one.  Anything else is a real error.
    missing="$(pacman -T "$@" 2>/dev/null || true)"
    [[ -z "$missing" ]] && return 0

    err "missing build prerequisites on a read-only system:"
    printf '      %s\n' $missing >&2
    err ""
    err "This is an immutable installation, so they cannot be installed into"
    err "the running system.  Add them transactionally and reboot first:"
    err ""
    err "    sudo deploytix update ${missing//$'\n'/ }"
    err "    sudo reboot"
    err ""
    err "then run deploytix-update-gamescope again."
    return 1
}

if [[ $EUID -eq 0 ]]; then
    err "run as a regular user — makepkg refuses to run as root."
    err "sudo is invoked internally for dependency sync and package install."
    exit 1
fi

if [[ ! -r "$PKGBUILD_SRC" ]]; then
    err "canonical PKGBUILD not found at $PKGBUILD_SRC"
    err "this system does not appear to be a Deploytix gaming deployment."
    exit 1
fi

# git is needed for the remote check and by makepkg to fetch sources.
if ! command -v git >/dev/null; then
    if (( IMMUTABLE )); then
        require_preinstalled git || exit 1
    else
        msg "git is not installed; installing it (sudo)..."
        sudo pacman -S --needed --noconfirm git
    fi
fi

# ── Check whether the remote branch has moved past the installed build ───────
installed_ver="$(pacman -Q "$PKGNAME" 2>/dev/null | awk '{print $2}' || true)"
if [[ -n "$installed_ver" ]]; then
    msg2 "Installed: $PKGNAME $installed_ver"
else
    msg2 "$PKGNAME is not currently installed"
fi

msg "Checking $REMOTE ($BRANCH)..."
remote_full="$(git ls-remote "$REMOTE" "refs/heads/$BRANCH" | awk '{print $1}')"
if [[ -z "$remote_full" ]]; then
    err "could not resolve branch '$BRANCH' on $REMOTE (network down?)"
    exit 1
fi
# pkgver embeds `git rev-parse --short HEAD`, whose width follows core.abbrev
# and grows with the object count.  Compare against the installed hash's own
# length rather than assuming 7, so the check keeps working as the repo grows.
installed_hash="${installed_ver#*.}"   # r2513.754b539-1 -> 754b539-1
installed_hash="${installed_hash%%-*}" #                 -> 754b539
abbrev_len="${#installed_hash}"
(( abbrev_len >= 7 && abbrev_len <= 40 )) || abbrev_len=7
remote_short="${remote_full:0:abbrev_len}"
msg2 "Remote HEAD: $remote_short"

up_to_date=0
if [[ -n "$installed_ver" && "$installed_hash" == "$remote_short" ]]; then
    up_to_date=1
fi

if (( CHECK_ONLY )); then
    if (( up_to_date )); then
        msg "gamescope is up to date ($installed_ver)"
        exit 10
    fi
    msg "gamescope update available (remote: $remote_short)"
    exit 0
fi

if (( up_to_date )) && ! (( FORCE )); then
    msg "gamescope is already built from the latest commit ($remote_short); nothing to do."
    msg2 "Use --force to rebuild anyway."
    exit 0
fi

# Checked after --check (a read-only query needs none of this) but before the
# build, so a misconfigured system fails in a second rather than after a
# full gamescope compile.
if (( IMMUTABLE )) && ! command -v deploytix >/dev/null; then
    err "immutable system detected but the 'deploytix' binary is not on PATH."
    err "it is required to install packages transactionally; cannot continue."
    exit 1
fi

# ── Build with the canonical PKGBUILD (exact same meson options) ─────────────
msg "Rebuilding gamescope from $BRANCH with the Deploytix build configuration..."
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"
cp "$PKGBUILD_SRC" "$BUILD_DIR/PKGBUILD"
cd "$BUILD_DIR"

if (( IMMUTABLE )); then
    # No dependency syncing is possible against a read-only /usr, so check the
    # full set up front and hand back one actionable command.  base-devel is a
    # group rather than a package, so its members that makepkg actually needs
    # are listed explicitly.
    msg "Verifying build prerequisites (read-only system, nothing will be installed)..."
    mapfile -t pkgbuild_deps < <(
        bash -c 'source "$1"; printf "%s\n" "${depends[@]}" "${makedepends[@]}"' \
             _ "$BUILD_DIR/PKGBUILD"
    )
    require_preinstalled \
        binutils fakeroot gcc make patch \
        "${pkgbuild_deps[@]}" || exit 1
    msg2 "All build prerequisites present."

    # --syncdeps would shell out to `pacman -S`; deps are verified above.
    makepkg --force --cleanbuild --noconfirm
else
    # base-devel is assumed present by makepkg (AUR convention) but is not
    # guaranteed on every deployment; --needed makes this a no-op when it is.
    msg2 "Ensuring build prerequisites (base-devel)..."
    sudo pacman -S --needed --noconfirm base-devel

    # --syncdeps pulls makedepends via sudo pacman; --cleanbuild guarantees a
    # pristine srcdir so stale build artifacts can never leak into the package.
    makepkg --syncdeps --force --cleanbuild --noconfirm
fi

# BUILD_DIR was wiped above, so any package here is from this run.  Exclude
# split debug packages in case makepkg.conf has OPTIONS=(debug).
pkgfile="$(find "$BUILD_DIR" -maxdepth 1 -name "${PKGNAME}-*.pkg.tar.*" \
           ! -name "*-debug-*" | head -n1)"
if [[ -z "$pkgfile" ]]; then
    err "makepkg completed but no ${PKGNAME} package was produced in $BUILD_DIR"
    exit 1
fi

# ── Install — raise the guard flag so the pacman hook lets this through ──────
# The flag must never outlive this script: if it lingered (e.g. Ctrl-C while
# pacman is waiting or running), the guard hook would wave through the AUR
# gamescope installs it exists to block.  The EXIT trap removes it on every
# exit path; the INT/TERM traps convert those signals into an exit so the
# EXIT trap is guaranteed to run.  (/run is tmpfs, so even an unkillable
# SIGKILL leaves the flag behind only until reboot.)
#
# On an immutable system the guard hook runs inside the chroot that
# `deploytix update` sets up; artix-chroot bind-mounts the host's /run into it,
# so the flag below is visible there and the transaction is let through.
remove_guard_flag() { sudo rm -f "$GUARD_FLAG"; }
msg "Installing $(basename "$pkgfile")..."
sudo mkdir -p "$(dirname "$GUARD_FLAG")"
trap remove_guard_flag EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
sudo touch "$GUARD_FLAG"
rc=0
if (( IMMUTABLE )); then
    # Builds a new snapshot set (btrfs) or inactive slot (LVM A/B), installs
    # into it with `pacman -U`, and repoints the boot pointer.  The running
    # system is untouched; a failure discards the half-built set.
    sudo deploytix update "$pkgfile" || rc=$?
else
    sudo pacman -U --noconfirm "$pkgfile" || rc=$?
fi
remove_guard_flag
trap - EXIT INT TERM
if (( rc != 0 )); then
    if (( IMMUTABLE )); then
        err "deploytix update failed (exit $rc); the running system is unchanged."
    else
        err "pacman -U failed (exit $rc); the previous gamescope remains installed."
    fi
    exit "$rc"
fi

# Step out of BUILD_DIR before removing it, or every subshell below inherits a
# deleted cwd and bash warns about getcwd.
cd /
rm -rf "$BUILD_DIR"
if (( IMMUTABLE )); then
    msg "gamescope update staged successfully."
    msg2 "Reboot to activate it (roll back with \`deploytix rollback\` if needed)."
else
    msg "gamescope updated successfully: $(pacman -Q "$PKGNAME")"
    msg2 "Restart the gamescope session (or reboot) to pick up the new build."
fi
