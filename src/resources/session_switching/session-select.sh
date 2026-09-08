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

# Record the raw invocation before anything can reject it. Steam's name for the
# desktop varies by client version, and an unrecognised one exits 1 below —
# silently, since Steam discards the exit code. This log is what turns "the
# button did nothing" into a fact rather than a guess. Same treatment the
# steamos-update / jupiter-biosupdate stubs get.
_SELECT_LOG="${XDG_STATE_HOME:-$HOME/.local/state}/deploytix-session-select.log"
mkdir -p "$(dirname "$_SELECT_LOG")" 2>/dev/null || true
printf '[%s] %s %s\n' "$(date -Is)" "${0##*/}" "$*" >> "$_SELECT_LOG" 2>/dev/null || true

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
#
# The two directions are not symmetric, so they use different mechanisms:
#
#   desktop -> gamescope: restart greetd (below, and what return-to-gamemode
#   has always done). Restarting the daemon kills the desktop session
#   outright, so it does not depend on anything inside the session noticing.
#
#   gamescope -> desktop: kill gamescope directly instead. Restarting greetd
#   here used to race gamescope's own teardown: gamescope holds the DRM
#   master and two Xwayland servers, and a freshly (re)started greetd can
#   spawn the next greeter before the old gamescope session has actually
#   released them, leaving the desktop compositor it starts with nothing to
#   acquire -- the switch stalls on a black screen with no gamescope, no
#   desktop and no greeter. Killing gamescope directly and waiting for it to
#   actually die avoids that race: the *same*, still-running greetd notices
#   the session exit and starts the greeter itself, exactly as
#   deploytix-session-manager's own header comment describes ("no while-loop
#   needed"). No daemon restart, no sudo, on this path.
#
#   This depends on steam-gamescope-session waiting on gamescope as well as
#   Steam (`wait -n "$steam_pid" "$gamescope_pid"`), not Steam alone: Steam
#   does not reliably exit just because its compositor disappeared, so
#   killing gamescope used to leave the session script wedged forever in
#   `wait "$steam_pid"` with nothing left to end the session.
_log() {
    printf '[%s] %s\n' "$(date -Is)" "$*" >> "$_SELECT_LOG" 2>/dev/null || true
}

if [[ "$session" == "desktop" ]]; then
    _log "ending session: killing gamescope"
    if pidof gamescope > /dev/null 2>&1; then
        kill -TERM $(pidof gamescope) 2>/dev/null || true
        _tries=0
        while pidof gamescope > /dev/null 2>&1 && [ "$_tries" -lt 15 ]; do
            sleep 0.2
            _tries=$((_tries + 1))
        done
        pidof gamescope > /dev/null 2>&1 && kill -KILL $(pidof gamescope) 2>/dev/null || true
    else
        _log "no gamescope process found"
    fi
else
    # Fork it detached: when greetd stops it kills this session and this
    # script, so the sequence has to run somewhere that survives the
    # teardown. sudo is on the outside so the detached process is already
    # root and needs no TTY. Passwordless sudo is set up on every deploytix
    # system (configure_sudoers uncomments the wheel NOPASSWD rule, and sudo
    # itself comes in with base-devel), so this needs no guard.
    _log "restarting greetd"
    sudo setsid /usr/bin/deploytix-restart-greetd </dev/null &>/dev/null &
fi
