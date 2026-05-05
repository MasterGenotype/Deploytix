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
# Start a background watchdog that force-kills stale desktop processes
# after 5 seconds, in case the normal logout path hangs (e.g. kwin or
# kded6 refusing to exit).  If the session exits cleanly before the
# timeout, the watchdog dies with the session.
(
    sleep 5
    pkill -x kwin_wayland            2>/dev/null || true
    pkill -x kwin_wayland_wrapper    2>/dev/null || true
    pkill -x startplasma-wayland     2>/dev/null || true
    pkill -x plasma_session          2>/dev/null || true
    pkill -x kded6                   2>/dev/null || true
    pkill -f kactivitymanagerd       2>/dev/null || true
    pkill -f xdg-desktop-portal-kde  2>/dev/null || true
) &

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
