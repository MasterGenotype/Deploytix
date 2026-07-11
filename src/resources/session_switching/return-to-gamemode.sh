#!/usr/bin/bash

# return-to-gamemode — Switch from desktop back to gamescope session.
#
# Writes "gamescope" to the sentinel and restarts greetd, which kills
# the desktop session and relaunches the session manager in gamescope
# mode (the default).
#
# Requires: passwordless sudo for `deploytix-restart-greetd`.

set -eu

SENTINEL="${XDG_CONFIG_HOME:-$HOME/.config}/deploytix-session"

# Write sentinel (not strictly necessary since gamescope is the default,
# but makes intent explicit in the log)
mkdir -p "$(dirname "$SENTINEL")"
echo "gamescope" > "$SENTINEL"
echo "Next session set to gamescope"

# Fork a detached root process to handle the restart.  When greetd stops
# it kills this desktop session (and this script), so the sequence must
# run in a process that survives the teardown.  sudo is on the outside so
# the detached process is already root (no TTY needed).
# deploytix-restart-greetd picks the right service command for the
# running init system (runit, OpenRC, s6, dinit).
sudo setsid /usr/bin/deploytix-restart-greetd </dev/null &>/dev/null &
