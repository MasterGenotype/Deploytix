#!/usr/bin/bash

# deploytix-session-manager — Session loop manager
#
# Launched by greetd as the user's auto-login session. Runs an infinite
# loop: read the sentinel file to determine which session to launch,
# consume (delete) the sentinel, run the session, and repeat when the
# session exits.
#
# No sentinel file = default to gamescope-session.

set -u

SENTINEL="${XDG_CONFIG_HOME:-$HOME/.config}/deploytix-session"

# Minimum seconds a session must run before we consider it a "real" session.
# If a session exits faster than this, we insert a delay to prevent a tight
# crash-loop from burning CPU.
MIN_SESSION_SECS=5
CRASH_DELAY=3

# --- Detect desktop environment ---
detect_desktop_command() {
    if command -v startplasma-wayland &>/dev/null; then
        echo "startplasma-wayland"
    elif command -v gnome-session &>/dev/null; then
        echo "gnome-session"
    elif command -v startxfce4 &>/dev/null; then
        echo "startxfce4"
    else
        echo ""
    fi
}

# --- Main loop ---
while true; do
    # Read and consume the sentinel (one-shot directive)
    session="gamescope"
    if [[ -f "$SENTINEL" ]]; then
        session="$(cat "$SENTINEL")"
        rm -f "$SENTINEL"
    fi

    start_time=$(date +%s)

    case "$session" in
        gamescope)
            echo "[session-manager] Launching steam-gamescope-session"
            /usr/local/bin/steam-gamescope-session || true
            ;;
        desktop)
            desktop_cmd="$(detect_desktop_command)"
            if [[ -z "$desktop_cmd" ]]; then
                echo >&2 "[session-manager] No supported desktop environment found"
            else
                echo "[session-manager] Launching $desktop_cmd"
                dbus-launch "$desktop_cmd" || true
            fi
            ;;
        *)
            echo >&2 "[session-manager] Unknown session '$session', falling back to gamescope"
            /usr/local/bin/steam-gamescope-session || true
            ;;
    esac

    elapsed=$(( $(date +%s) - start_time ))

    if (( elapsed < MIN_SESSION_SECS )); then
        echo >&2 "[session-manager] Session exited after ${elapsed}s (< ${MIN_SESSION_SECS}s), waiting ${CRASH_DELAY}s"
        sleep "$CRASH_DELAY"
    fi
done
