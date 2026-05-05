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

# Terminate the current session so the session manager can switch.
# For gamescope→desktop: kill gamescope so greetd restarts the session
# manager, which reads the "desktop" sentinel.
# For desktop→gamescope: stop greetd, kill orphaned processes that
# survive the session teardown (pipewire, steamwebhelper, etc.), then
# restart greetd on a clean VT.
if [[ "$session" == "desktop" ]] && pidof gamescope > /dev/null 2>&1; then
    kill -TERM $(pidof gamescope) 2>/dev/null || true
elif [[ "$session" == "gamescope" ]]; then
    # Fork a detached root process to handle the restart.  When greetd
    # stops it kills this session (and this script), so the sequence
    # must run in a process that survives the teardown.  sudo is on the
    # outside so the detached shell is already root (no TTY needed).
    sudo setsid sh -c '
        sv restart greetd
    ' </dev/null &>/dev/null &
fi
