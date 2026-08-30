#!/bin/sh
# steamos-update — deploytix stand-in for the SteamOS image updater.
#
# Steam launched with -steamdeck probes SteamOS update tooling. A deploytix
# system is not SteamOS: the OS is updated with pacman, or with
# `deploytix update` on an immutable root, and never through this path. So
# the honest answer to every query is "already up to date".
#
# The interface follows the real tool:
#   steamos-update [--supervised] [--enable-duplicate-detection] [check|now]
#     check   query only   — exit 0 if an update is available, 7 if not
#     now     apply        — exit 0 if an update was applied, 7 if none
#   No subcommand means apply.
#
# Both paths return 7 here. Returning 0 would tell Steam an image update
# exists and then never deliver one, which is worse than saying there is
# nothing to do.
#
# Every invocation is logged so it is possible to see what Steam actually
# asks for. Logging is best-effort and never changes the exit status.

_args="$*"

_log() {
    _dir="${XDG_STATE_HOME:-$HOME/.local/state}"
    mkdir -p "$_dir" 2>/dev/null || return 0
    printf '%s steamos-update [%s] -> exit %s\n' \
        "$(date -Is 2>/dev/null)" "$_args" "$1" \
        >>"$_dir/deploytix-steamos-tooling.log" 2>/dev/null || true
}

# No subcommand means apply, matching the real tool.
_mode=apply
for _arg in "$@"; do
    case "$_arg" in
        --*)        ;;   # --supervised, --enable-duplicate-detection, …
        check)      _mode=check ;;
        now|apply)  _mode=apply ;;
    esac
done

case "$_mode" in
    check) _rc=7 ;;   # no image update available
    apply) _rc=7 ;;   # nothing to apply
    *)     _rc=7 ;;
esac

_log "$_rc"
exit "$_rc"
