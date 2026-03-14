#!/usr/bin/bash

# return-to-gamemode — Set next session to gamescope and log out of desktop.
#
# Writes "gamescope" to the sentinel so deploytix-session-manager launches
# gamescope-session after the desktop exits, then triggers a DE logout.
#
# No root required.

set -eu

SENTINEL="${XDG_CONFIG_HOME:-$HOME/.config}/deploytix-session"

# Write sentinel
mkdir -p "$(dirname "$SENTINEL")"
echo "gamescope" > "$SENTINEL"
echo "Next session set to gamescope"

# --- Detect desktop environment and log out ---
if command -v startplasma-wayland &>/dev/null || command -v startplasma-x11 &>/dev/null; then
    qdbus org.kde.Shutdown /Shutdown org.kde.Shutdown.logout
elif command -v gnome-session &>/dev/null; then
    gnome-session-quit --logout --no-prompt
elif command -v startxfce4 &>/dev/null; then
    xfce4-session-logout --logout
else
    echo >&2 "Warning: Unknown desktop environment, cannot auto-logout"
    echo >&2 "Please log out manually to return to Game Mode"
fi
