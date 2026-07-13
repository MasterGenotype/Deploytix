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

msg()  { printf '\033[1;34m==>\033[0m \033[1m%s\033[0m\n' "$*"; }
msg2() { printf '  \033[1;32m->\033[0m %s\n' "$*"; }
err()  { printf '\033[1;31m==> ERROR:\033[0m %s\n' "$*" >&2; }

usage() {
    cat <<'EOF'
Usage: deploytix-update-gamescope [--check] [--force]

Rebuilds gamescope (Bazzite fork) from the Deploytix source branch with the
exact same meson options used at install time, then installs it via pacman.
Do NOT update gamescope through the AUR — that build breaks the Steam
gamescope session.

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
    msg "git is not installed; installing it (sudo)..."
    sudo pacman -S --needed --noconfirm git
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
remote_short="${remote_full:0:7}"
msg2 "Remote HEAD: $remote_short"

# pkgver format is r<count>.<shorthash> (see PKGBUILD), so the installed
# version string contains the short commit hash of the built source.
up_to_date=0
if [[ -n "$installed_ver" && "$installed_ver" == *".${remote_short}-"* ]]; then
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

# ── Build with the canonical PKGBUILD (exact same meson options) ─────────────
msg "Rebuilding gamescope from $BRANCH with the Deploytix build configuration..."
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"
cp "$PKGBUILD_SRC" "$BUILD_DIR/PKGBUILD"
cd "$BUILD_DIR"

# base-devel is assumed present by makepkg (AUR convention) but is not
# guaranteed on every deployment; --needed makes this a no-op when it is.
msg2 "Ensuring build prerequisites (base-devel)..."
sudo pacman -S --needed --noconfirm base-devel

# --syncdeps pulls makedepends via sudo pacman; --cleanbuild guarantees a
# pristine srcdir so stale build artifacts can never leak into the package.
makepkg --syncdeps --force --cleanbuild --noconfirm

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
# pacman -U is waiting or running), the guard hook would wave through the AUR
# gamescope installs it exists to block.  The EXIT trap removes it on every
# exit path; the INT/TERM traps convert those signals into an exit so the
# EXIT trap is guaranteed to run.  (/run is tmpfs, so even an unkillable
# SIGKILL leaves the flag behind only until reboot.)
remove_guard_flag() { sudo rm -f "$GUARD_FLAG"; }
msg "Installing $(basename "$pkgfile")..."
sudo mkdir -p "$(dirname "$GUARD_FLAG")"
trap remove_guard_flag EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
sudo touch "$GUARD_FLAG"
rc=0
sudo pacman -U --noconfirm "$pkgfile" || rc=$?
remove_guard_flag
trap - EXIT INT TERM
if (( rc != 0 )); then
    err "pacman -U failed (exit $rc); the previous gamescope remains installed."
    exit "$rc"
fi

rm -rf "$BUILD_DIR"
msg "gamescope updated successfully: $(pacman -Q "$PKGNAME")"
msg2 "Restart the gamescope session (or reboot) to pick up the new build."
