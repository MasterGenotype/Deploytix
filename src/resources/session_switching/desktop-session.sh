#!/bin/sh
# ==================================================
# Desktop Session Wrapper — GENERATED
#
# This file is a template. deploytix renders it at install time from the
# deployment's chosen desktop environment and writes the result to
# /usr/local/bin/desktop-session.
#
# The placeholder names are documented on `DesktopSpec` in
# configure::session_switching, deliberately NOT here: substitution is a plain
# string replace over the whole file, so naming a placeholder in a comment
# expands it there too. The teardown list is multi-line, which turned this
# header into nine bare words that the shell then tried to run as commands.
#
# Rendering is pure: the same DeploymentConfig always produces the same
# bytes, so re-running the installer over an existing system converges
# rather than accumulating state.
#
# Purpose: wrap the desktop environment with background + wait so signal
# traps fire immediately, ensuring the greetd user session always exits
# cleanly on logout.
#
# This deliberately exports NO XDG_* session variables. startplasma-wayland is
# what creates the Wayland session, so declaring XDG_SESSION_TYPE=wayland ahead
# of it tells Qt and KDE components that a session already exists and they
# reach for a WAYLAND_DISPLAY that kwin_wayland has not created yet. The
# version of this script that ran on working hardware for four months set none
# of them and let the desktop establish its own environment; setting them is
# what broke "Return to Desktop".
#
# Without this wrapper, `dbus-run-session startplasma-wayland` runs as
# the session leader; if any subprocess hangs on logout (kwin, kded6,
# xdg-desktop-portal-*, etc.), dbus-run-session never exits, greetd never
# restarts the greeter, and the system appears stuck on a blank screen.
# ==================================================

set -e

# --------- 0. Logging ---------
_LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}"
mkdir -p "$_LOG_DIR" 2>/dev/null || true
exec >>"$_LOG_DIR/desktop-session.log" 2>&1
echo "[desktop-session] ==== starting at $(date -Is) pid=$$ uid=$(id -u) ===="

# --------- 1. Resolve the desktop command ---------
# The primary command is the one deploytix installed a desktop environment
# for. The fallbacks cover a system whose DE was changed after install.
desktop_cmd=""
for _candidate in "@DEPLOYTIX_DESKTOP_CMD@" @DEPLOYTIX_DESKTOP_FALLBACKS@; do
    [ -n "$_candidate" ] || continue
    if command -v "$_candidate" >/dev/null 2>&1; then
        desktop_cmd="$_candidate"
        break
    fi
done

if [ -z "$desktop_cmd" ]; then
    echo >&2 "[desktop-session] No desktop environment found (expected @DEPLOYTIX_DESKTOP_CMD@)"
    exit 1
fi

if [ "$desktop_cmd" != "@DEPLOYTIX_DESKTOP_CMD@" ]; then
    echo "[desktop-session] Configured command @DEPLOYTIX_DESKTOP_CMD@ is absent; using $desktop_cmd"
fi

# --------- 2. Cleanup handler (runs on exit or signal) ---------
# Teardown targets are DE-specific: killing KDE processes on a GNOME
# install is pointless, and leaving GNOME's alive on a GNOME install is
# what wedges the logout. Each entry is "<match>:<pattern>", where match
# is `x` (exact process name) or `f` (full command line).
_DE_PROCS="@DEPLOYTIX_DE_PROCS@"

_de_kill() {
    _sig="$1"
    printf '%s\n' "$_DE_PROCS" | while IFS= read -r _entry; do
        [ -n "$_entry" ] || continue
        _mode="${_entry%%:*}"
        _pat="${_entry#*:}"
        case "$_mode" in
            x) pkill $_sig -x "$_pat" 2>/dev/null || true ;;
            f) pkill $_sig -f "$_pat" 2>/dev/null || true ;;
        esac
    done
}

_cleaned=0
cleanup() {
    [ "$_cleaned" -ne 0 ] && return 0
    _cleaned=1
    echo "[desktop-session] Cleanup: tearing down desktop session (pid=$$)"

    # Phase 1: SIGTERM desktop processes
    _de_kill ""

    sleep 1

    # Phase 2: SIGKILL stubborn processes
    _de_kill "-9"

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
# environments need (kwin_wayland, kded6, gnome-shell, etc. fail
# without one).
echo "[desktop-session] Starting $desktop_cmd via dbus-run-session"
dbus-run-session "$desktop_cmd" &
desktop_pid=$!

wait "$desktop_pid" 2>/dev/null || true
desktop_ret=$?
echo "[desktop-session] Desktop exited ($desktop_ret)"

exit "$desktop_ret"
