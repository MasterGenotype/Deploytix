#!/usr/bin/env bash
# scripts/bootstrap.sh — Clone and prepare a complete deploytix working tree.
#
# Clones the deploytix repository (or updates an existing clone), then
# initializes all vendored submodules (vendor/tkg-gui, vendor/gamescope)
# so the installer can resolve and build its custom packages
# (deploytix-git, deploytix-gui-git, tkg-gui-git, gamescope-git) from
# source without any sibling checkouts.
#
# The default destination (~/.gitrepos/deploytix) matches the paths the
# installer searches at runtime.
#
# Usage:
#   bootstrap.sh [DEST]            clone/update + submodules (default DEST: ~/.gitrepos/deploytix)
#   bootstrap.sh --build [DEST]    additionally build and install via `make install`
set -euo pipefail

REPO_URL="https://github.com/MasterGenotype/Deploytix.git"

BUILD=0
if [[ "${1:-}" == "--build" ]]; then
    BUILD=1
    shift
fi
DEST="${1:-$HOME/.gitrepos/deploytix}"

if [[ -d "$DEST/.git" ]]; then
    echo ">>> Updating existing clone at $DEST"
    git -C "$DEST" pull --ff-only
else
    echo ">>> Cloning $REPO_URL to $DEST"
    mkdir -p "$(dirname "$DEST")"
    git clone "$REPO_URL" "$DEST"
fi

# sync picks up .gitmodules URL changes on pre-existing clones before init.
echo ">>> Initializing vendored submodules"
git -C "$DEST" submodule sync --recursive
git -C "$DEST" submodule update --init --recursive

echo ">>> Submodule status:"
git -C "$DEST" submodule status --recursive

if [[ "$BUILD" == 1 ]]; then
    echo ">>> Building and installing deploytix"
    make -C "$DEST" install
fi

echo ">>> Done. Working tree ready at $DEST"
