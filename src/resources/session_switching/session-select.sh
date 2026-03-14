#!/usr/bin/bash

# session-select — Write the next session to the sentinel file.
#
# Called from within a running session (gamescope or desktop) to tell
# deploytix-session-manager what to launch next.  The current session
# is expected to exit shortly after this runs.
#
# Usage: session-select [gamescope|desktop]

set -eu

SENTINEL="${XDG_CONFIG_HOME:-$HOME/.config}/deploytix-session"

session="${1:-gamescope}"

case "$session" in
    gamescope|desktop)
        mkdir -p "$(dirname "$SENTINEL")"
        echo "$session" > "$SENTINEL"
        echo "Next session set to '$session'"
        ;;
    *)
        echo >&2 "Unknown session '$session'. Use: gamescope, desktop"
        exit 1
        ;;
esac
