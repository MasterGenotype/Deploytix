#!/bin/sh
# steamos-select-branch — deploytix stand-in for SteamOS branch selection.
#
# Steam launched with -steamdeck queries the update branch to pick which
# client channel to fetch. Deploytix tracks no SteamOS image branches, so
# "stable" is the only branch that exists here — which is what makes Steam
# ask for steam_client_steamdeck_stable_ubuntu12.
#
# The interface follows the real tool:
#   -c | --current   print the current branch
#   -l | --list      list selectable branches, one per line
#   <branch>         select a branch
#
# Selecting anything other than "stable" fails rather than silently
# reporting success: claiming to have switched to a branch that does not
# exist would leave Steam asking for a client channel nothing can serve.
#
# Every invocation is logged so it is possible to see what Steam actually
# asks for. Logging is best-effort and never changes the exit status.

_args="$*"

_log() {
    _dir="${XDG_STATE_HOME:-$HOME/.local/state}"
    mkdir -p "$_dir" 2>/dev/null || return 0
    printf '%s steamos-select-branch [%s] -> exit %s\n' \
        "$(date -Is 2>/dev/null)" "$_args" "$1" \
        >>"$_dir/deploytix-steamos-tooling.log" 2>/dev/null || true
}

case "${1:-}" in
    ""|-c|--current)
        echo "stable"
        _log 0
        exit 0
        ;;
    -l|--list)
        echo "stable"
        _log 0
        exit 0
        ;;
    -*)
        # Unknown flag: report the current branch rather than failing, so a
        # probe deploytix has not seen before cannot break the OOBE.
        echo "stable"
        _log 0
        exit 0
        ;;
    stable)
        _log 0
        exit 0
        ;;
    *)
        echo >&2 "steamos-select-branch: no such branch '$1' (only 'stable' exists on this system)"
        _log 1
        exit 1
        ;;
esac
