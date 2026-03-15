#!/usr/bin/env bash
set -eu

log_dir="${XDG_STATE_HOME:-$HOME/.local/state}"
log_file="$log_dir/pipewire-autostart.log"
mkdir -p "$log_dir"

start_if_missing() {
    local cmd="$1"
    shift
    if ! pgrep -u "$USER" -x "$cmd" >/dev/null 2>&1; then
        "$@" >>"$log_file" 2>&1 &
    fi
}

# Give the session a moment to finish coming up
sleep 2

start_if_missing pipewire pipewire
sleep 1
start_if_missing pipewire-pulse pipewire-pulse
sleep 1
start_if_missing wireplumber wireplumber

exit 0
