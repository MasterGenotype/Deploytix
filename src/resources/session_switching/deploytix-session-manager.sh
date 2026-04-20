#!/usr/bin/bash

# deploytix-session-manager — greetd greeter for auto-login
#
# Runs as greetd's default_session (Class=greeter). Instead of launching
# sessions directly (which inherits the greeter's revoked seat), it uses
# greetd IPC to create a proper Class=user session. greetd terminates
# this greeter and starts the user session; when the user session exits,
# greetd restarts this greeter.  No while-loop needed.

set -u

SENTINEL="${XDG_CONFIG_HOME:-$HOME/.config}/deploytix-session"

# ---------- Logging ----------
LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}"
mkdir -p "$LOG_DIR" 2>/dev/null || true
exec >>"$LOG_DIR/deploytix-session.log" 2>&1
echo "[session-manager] ==== starting at $(date -Is) pid=$$ ===="

# ---------- Stale-process cleanup ----------
#
# Runs inside the greetd greeter (Class=greeter, revoked seat, no X/Wayland).
# It MUST be fast and side-effect-free — anything that hangs or fails badly
# here will cause bash to exit before we reach the greetd IPC start_session
# call, and greetd will log:
#     error: check_children: greeter exited without creating a session
# and respawn us in a tight loop.
#
# IMPORTANT: do NOT invoke `steam` (even `steam -shutdown`). The Arch/Artix
# `steam` wrapper ignores -shutdown at the wrapper layer and runs the full
# Steam Runtime bootstrap with `set -e`. Running that from a seat-less greeter
# context routinely hangs or exits non-deterministically, which was the
# primary cause of the "greeter exited without creating a session" respawn
# loop. pkill alone is sufficient to reap any lingering steam processes.
cleanup_stale_sessions() {
    echo "[session-manager] Cleaning up stale session processes"
    # Graceful SIGTERM pass — every pkill is allowed to "fail" (no match)
    # without propagating a non-zero exit, which matters if `set -e` is ever
    # enabled above.
    pkill -x gamescope               2>/dev/null || true
    pkill -f 'steam.*-steamos3'      2>/dev/null || true
    pkill -x steam                   2>/dev/null || true
    pkill -x steamwebhelper          2>/dev/null || true
    pkill -x kwin_wayland            2>/dev/null || true
    pkill -x kwin_wayland_wrapper    2>/dev/null || true
    pkill -x startplasma-wayland     2>/dev/null || true
    pkill -x plasma_session          2>/dev/null || true
    pkill -f 'Xwayland :'            2>/dev/null || true
    pkill -x pipewire                2>/dev/null || true
    pkill -x pipewire-pulse          2>/dev/null || true
    pkill -x wireplumber             2>/dev/null || true
    sleep 1 || true
    # SIGKILL fallback for anything that ignored SIGTERM.
    pkill -9 -x gamescope            2>/dev/null || true
    pkill -9 -x steam                2>/dev/null || true
    pkill -9 -x steamwebhelper       2>/dev/null || true
    pkill -9 -x kwin_wayland         2>/dev/null || true
    pkill -9 -x kwin_wayland_wrapper 2>/dev/null || true
    pkill -9 -f 'Xwayland :'         2>/dev/null || true
    pkill -9 -x pipewire             2>/dev/null || true
    pkill -9 -x pipewire-pulse       2>/dev/null || true
    pkill -9 -x wireplumber          2>/dev/null || true
}

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

cleanup_stale_sessions

# ---------- Choose session ----------
session="gamescope"
if [[ -f "$SENTINEL" ]]; then
    session="$(cat "$SENTINEL")"
    rm -f "$SENTINEL"
fi
echo "[session-manager] Selected session: $session"

case "$session" in
    gamescope)
        cmd="/usr/local/bin/steam-gamescope-session"
        ;;
    desktop)
        cmd="$(detect_desktop_command)"
        if [[ -z "$cmd" ]]; then
            echo >&2 "[session-manager] No desktop environment found, falling back to gamescope"
            cmd="/usr/local/bin/steam-gamescope-session"
        fi
        ;;
    *)
        echo >&2 "[session-manager] Unknown session '$session', falling back to gamescope"
        cmd="/usr/local/bin/steam-gamescope-session"
        ;;
esac

# ---------- Start session via greetd IPC ----------
if [[ -n "${GREETD_SOCK:-}" ]]; then
    echo "[session-manager] Starting via greetd IPC: $cmd"
    /usr/bin/greetd-ipc "$(whoami)" "$cmd"
    ipc_ret=$?
    if (( ipc_ret != 0 )); then
        echo >&2 "[session-manager] greetd IPC failed ($ipc_ret), falling back to direct launch"
        exec "$cmd"
    fi
    # greetd will terminate us after this; exit cleanly
    echo "[session-manager] IPC succeeded, waiting for greetd to start user session"
    exit 0
else
    echo >&2 "[session-manager] GREETD_SOCK not set, launching directly"
    exec "$cmd"
fi
