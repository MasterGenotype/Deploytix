#!/usr/bin/env bash
set -eu

log_dir="${XDG_STATE_HOME:-$HOME/.local/state}"
log_file="$log_dir/pipewire-autostart.log"
mkdir -p "$log_dir"

runtime_dir="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

start_if_missing() {
    local cmd="$1"
    shift
    if ! pgrep -u "$USER" -x "$cmd" >/dev/null 2>&1; then
        "$@" >>"$log_file" 2>&1 &
    fi
}

# Block on the thing we actually need rather than on the clock.
#
# pipewire-pulse and wireplumber are clients of pipewire's core socket, so
# the only real ordering constraint is "that socket exists". Polling for it
# returns the instant it appears — typically well under 200 ms — where the
# fixed sleeps this replaced cost a flat 4 s on every session start, paid
# in full on the boot -> Steam path. The deadline keeps a pipewire that
# never comes up from wedging the caller.
wait_for_socket() {
    local path="$1" waited=0
    # 100 x 50 ms = 5 s ceiling.
    while [ ! -e "$path" ]; do
        if [ "$waited" -ge 100 ]; then
            return 1
        fi
        sleep 0.05
        waited=$((waited + 1))
    done
    return 0
}

start_if_missing pipewire pipewire

if ! wait_for_socket "$runtime_dir/pipewire-0"; then
    echo "pipewire socket did not appear within 5s; starting clients anyway" >>"$log_file"
fi

# Independent clients of the same socket — start both at once instead of
# serialising them behind a sleep.
start_if_missing pipewire-pulse pipewire-pulse
start_if_missing wireplumber wireplumber

exit 0
