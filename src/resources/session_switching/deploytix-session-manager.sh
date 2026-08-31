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

# Teardown targets as "<mode>:<pattern>", where mode x matches an exact
# binary name (pkill/pgrep -x) and f matches the full command line (-f).
# One table drives detection, SIGTERM and SIGKILL so the three passes can
# never drift apart.
STALE_PROCS=(
    "x:gamescope"
    "f:steam.*-steamos3"
    "x:steam"
    "x:steamwebhelper"
    "x:kwin_wayland"
    "x:kwin_wayland_wrapper"
    "x:startplasma-wayland"
    "x:plasma_session"
    "x:kded6"
    "f:kactivitymanagerd"
    "f:xdg-desktop-portal-kde"
    "f:Xwayland :"
    "x:pipewire"
    "x:pipewire-pulse"
    "x:wireplumber"
)

# True if any teardown target is still running.
_stale_any() {
    local entry mode pat
    for entry in "${STALE_PROCS[@]}"; do
        mode="${entry%%:*}"
        pat="${entry#*:}"
        case "$mode" in
            x) pgrep -x "$pat" >/dev/null 2>&1 && return 0 ;;
            f) pgrep -f "$pat" >/dev/null 2>&1 && return 0 ;;
        esac
    done
    return 1
}

# Signal every teardown target. $1 is the pkill signal flag ("" for the
# default SIGTERM, "-9" for the fallback pass). Every pkill is allowed to
# "fail" (no match) without propagating a non-zero exit, which matters if
# `set -e` is ever enabled above.
_stale_kill() {
    local sig="$1" entry mode pat
    for entry in "${STALE_PROCS[@]}"; do
        mode="${entry%%:*}"
        pat="${entry#*:}"
        case "$mode" in
            x) pkill $sig -x "$pat" 2>/dev/null || true ;;
            f) pkill $sig -f "$pat" 2>/dev/null || true ;;
        esac
    done
}

cleanup_stale_sessions() {
    # Cold boot is the common case and the one that decides boot -> Steam
    # latency: there is no previous session to tear down, so skip the whole
    # TERM/settle/KILL cycle rather than paying a flat second (plus ~30
    # pkill spawns) before greetd IPC is even reached.
    if ! _stale_any; then
        echo "[session-manager] No stale session processes; skipping cleanup"
        return 0
    fi

    echo "[session-manager] Cleaning up stale session processes"
    _stale_kill ""

    # Poll for the graceful pass to land instead of sleeping a fixed second.
    # Processes that honour SIGTERM are typically gone within tens of
    # milliseconds; the 20 x 50 ms ceiling still bounds the pathological
    # case at the same one second the old fixed sleep cost unconditionally.
    local waited=0
    while _stale_any && [ "$waited" -lt 20 ]; do
        sleep 0.05
        waited=$((waited + 1))
    done

    # SIGKILL fallback for anything that ignored SIGTERM.
    if _stale_any; then
        echo "[session-manager] SIGTERM ignored by some processes; escalating to SIGKILL"
        _stale_kill "-9"
    fi
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

# Use an array so multi-word commands (e.g. dbus-run-session startplasma-wayland)
# are passed as separate arguments to greetd-ipc and exec.
case "$session" in
    gamescope)
        cmd=("/usr/local/bin/steam-gamescope-session")
        ;;
    desktop)
        desktop_cmd="$(detect_desktop_command)"
        if [[ -z "$desktop_cmd" ]]; then
            echo >&2 "[session-manager] No desktop environment found, falling back to gamescope"
            cmd=("/usr/local/bin/steam-gamescope-session")
        else
            # Use the desktop-session wrapper which runs the desktop in
            # background + wait, ensuring signal traps fire immediately
            # for clean session teardown on logout.
            cmd=("/usr/local/bin/desktop-session")
        fi
        ;;
    *)
        echo >&2 "[session-manager] Unknown session '$session', falling back to gamescope"
        cmd=("/usr/local/bin/steam-gamescope-session")
        ;;
esac

# ---------- Start session via greetd IPC ----------
if [[ -n "${GREETD_SOCK:-}" ]]; then
    echo "[session-manager] Starting via greetd IPC: ${cmd[*]}"
    /usr/bin/greetd-ipc "$(whoami)" "${cmd[@]}"
    ipc_ret=$?
    if (( ipc_ret != 0 )); then
        echo >&2 "[session-manager] greetd IPC failed ($ipc_ret), falling back to direct launch"
        exec "${cmd[@]}"
    fi
    # greetd will terminate us after this; exit cleanly
    echo "[session-manager] IPC succeeded, waiting for greetd to start user session"
    exit 0
else
    echo >&2 "[session-manager] GREETD_SOCK not set, launching directly"
    exec "${cmd[@]}"
fi
