#!/usr/bin/bash

# session-select — Write the next session to the sentinel file and
# terminate the current session.
#
# Called from within a running session (gamescope or desktop) to tell
# deploytix-session-manager what to launch next.
#
# Also aliased as steamos-session-select so that Steam's built-in
# "Switch to Desktop" button works (Steam calls steamos-session-select plasma).
#
# Usage: session-select [gamescope|desktop|plasma]

set -eu

SENTINEL="${XDG_CONFIG_HOME:-$HOME/.config}/deploytix-session"

session="${1:-gamescope}"

case "$session" in
    gamescope|desktop|plasma)
        # Normalize: Steam sends "plasma" for desktop mode
        [[ "$session" != "gamescope" ]] && session="desktop"
        mkdir -p "$(dirname "$SENTINEL")"
        echo "$session" > "$SENTINEL"
        echo "Next session set to '$session'"
        ;;
    *)
        echo >&2 "Unknown session '$session'. Use: gamescope, desktop"
        exit 1
        ;;
esac

# Terminate the current session so the session manager can switch
if [[ "$session" == "desktop" ]] && pidof gamescope > /dev/null 2>&1; then
    kill -TERM $(pidof gamescope) 2>/dev/null || true
elif [[ "$session" == "gamescope" ]]; then
    pkill -x kwin_wayland 2>/dev/null || true
    pkill -x startplasma-wayland 2>/dev/null || true
    pkill -x plasma_session 2>/dev/null || true
fi
