#!/bin/sh
# jupiter-biosupdate — deploytix stand-in for Steam Deck BIOS updates.
#
# Steam launched with -steamdeck probes this during the Deck OOBE and from
# Settings > System. Deploytix does not run on Valve hardware, so there is
# never a BIOS update to check for or apply.
#
# The real tool exits 0 both when it has nothing to do and when an update
# was applied successfully, so 0 is the correct answer either way; there is
# no "up to date" code to distinguish here the way steamos-update has one.
#
# Every invocation is logged so it is possible to see what Steam actually
# asks for. Logging is best-effort and never changes the exit status.

_args="$*"

_log() {
    _dir="${XDG_STATE_HOME:-$HOME/.local/state}"
    mkdir -p "$_dir" 2>/dev/null || return 0
    printf '%s jupiter-biosupdate [%s] -> exit %s\n' \
        "$(date -Is 2>/dev/null)" "$_args" "$1" \
        >>"$_dir/deploytix-steamos-tooling.log" 2>/dev/null || true
}

_log 0
exit 0
