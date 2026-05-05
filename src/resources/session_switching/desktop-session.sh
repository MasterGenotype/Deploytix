#!/bin/sh
# ==================================================
# Desktop Session Wrapper
# Wraps the desktop environment (KDE Plasma / GNOME / XFCE) with
# background + wait so signal traps fire immediately, ensuring the
# greetd user session always exits cleanly on logout.
#
# Without this wrapper, dbus-run-session startplasma-wayland runs as
# the session leader; if any KDE subprocess hangs on logout (kwin,
# kded6, xdg-desktop-portal-kde, etc.), dbus-run-session never exits,
# greetd never restarts the greeter, and the system appears stuck on
# a blank screen.
# ==================================================

set -e

# --------- 0. Logging ---------
_LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}"
mkdir -p "$_LOG_DIR" 2>/dev/null || true
exec >>"$_LOG_DIR/desktop-session.log" 2>&1
echo "[desktop-session] ==== starting at $(date -Is) pid=$$ uid=$(id -u) ===="

# --------- 1. Detect desktop environment ---------
if command -v startplasma-wayland >/dev/null 2>&1; then
    desktop_cmd="startplasma-wayland"
elif command -v gnome-session >/dev/null 2>&1; then
    desktop_cmd="gnome-session"
elif command -v startxfce4 >/dev/null 2>&1; then
    desktop_cmd="startxfce4"
else
    echo >&2 "[desktop-session] No desktop environment found"
    exit 1
fi

# --------- 2. Cleanup handler (runs on exit or signal) ---------
_cleaned=0
cleanup() {
    [ "$_cleaned" -ne 0 ] && return 0
    _cleaned=1
    echo "[desktop-session] Cleanup: tearing down desktop session (pid=$$)"

    # Phase 1: SIGTERM desktop processes
    pkill -x kwin_wayland            2>/dev/null || true
    pkill -x kwin_wayland_wrapper    2>/dev/null || true
    pkill -x startplasma-wayland     2>/dev/null || true
    pkill -x plasma_session          2>/dev/null || true
    pkill -x kded6                   2>/dev/null || true
    pkill -f kactivitymanagerd       2>/dev/null || true
    pkill -f xdg-desktop-portal-kde  2>/dev/null || true
    pkill -f 'Xwayland :'           2>/dev/null || true
    pkill -x pipewire                2>/dev/null || true
    pkill -x pipewire-pulse          2>/dev/null || true
    pkill -x wireplumber             2>/dev/null || true

    sleep 1

    # Phase 2: SIGKILL stubborn processes
    pkill -9 -x kwin_wayland         2>/dev/null || true
    pkill -9 -x kwin_wayland_wrapper 2>/dev/null || true
    pkill -9 -x startplasma-wayland  2>/dev/null || true
    pkill -9 -x plasma_session       2>/dev/null || true
    pkill -9 -x kded6                2>/dev/null || true
    pkill -9 -f kactivitymanagerd    2>/dev/null || true
    pkill -9 -f xdg-desktop-portal-kde 2>/dev/null || true
    pkill -9 -f 'Xwayland :'        2>/dev/null || true

    # Phase 3: Kill the dbus-run-session process if still alive
    if [ -n "${desktop_pid:-}" ]; then
        kill "$desktop_pid" 2>/dev/null || true
        kill -9 "$desktop_pid" 2>/dev/null || true
    fi

    # Phase 4: Force-kill any remaining background jobs
    for job in $(jobs -p); do
        kill -9 "$job" 2>/dev/null || true
    done
}
trap cleanup EXIT HUP TERM

# --------- 3. Launch desktop session ---------
# Run in background + wait so that signal traps fire immediately.
# dbus-run-session provides the D-Bus session bus that desktop
# environments need (kwin_wayland, kded6, etc. fail without one).
echo "[desktop-session] Starting $desktop_cmd via dbus-run-session"
dbus-run-session "$desktop_cmd" &
desktop_pid=$!

wait "$desktop_pid" 2>/dev/null || true
desktop_ret=$?
echo "[desktop-session] Desktop exited ($desktop_ret)"

exit "$desktop_ret"
