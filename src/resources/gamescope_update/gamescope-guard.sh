#!/bin/bash
# PreTransaction pacman hook body (deploytix-gamescope-guard.hook).
#
# Lets a gamescope install/upgrade proceed only when it was initiated by
# deploytix-update-gamescope, which raises the flag file below around its
# `pacman -U` call.  Anything else — `yay -Syu`, `yay -S gamescope-git`,
# a manual `pacman -U` of an AUR-built package — is aborted: those builds
# use the upstream Valve source and different meson options, and break the
# Steam gamescope session on Deploytix systems.

if [[ -e /run/deploytix/gamescope-update-in-progress ]]; then
    exit 0
fi

cat >&2 <<'EOF'
:: gamescope on this system is a Deploytix-managed build (Bazzite fork,
   custom meson options). Replacing it with an AUR or repo build breaks
   the Steam gamescope session (Steam fails to launch in game mode).

   To update gamescope, run:

       deploytix-update-gamescope

   or use the "Update Gamescope" entry in the application menu.
EOF
exit 1
