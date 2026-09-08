#!/bin/sh

# steam-first-login — desktop-mode Steam sign-in helper.
#
# Started from /etc/xdg/autostart in every desktop session. If a
# remembered Steam login already exists this exits immediately and the
# desktop is untouched. Otherwise it launches the regular windowed
# Steam client so the user can sign in with a real keyboard (or the QR
# code in the Steam mobile app), and polls loginusers.vdf. Once
# credentials land it notifies the user and automatically returns to
# gamemode via return-to-gamemode.
#
# This is the fallback half of the first-boot sign-in flow: the primary
# path is Steam's own gamepad-UI login inside gamescope. When that
# fails (Steam exits while still logged out), steam-gamescope-session
# routes the next session to the desktop, which triggers this helper.

# Already signed in — nothing to do.
/usr/bin/steam-login-check && exit 0

# ---------- Logging ----------
_LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}"
mkdir -p "$_LOG_DIR" 2>/dev/null || true
exec >>"$_LOG_DIR/steam-first-login.log" 2>&1
echo "[first-login] ==== starting at $(date -Is) pid=$$ ===="

notify() {
    if command -v notify-send >/dev/null 2>&1; then
        notify-send -a "Deploytix" "$@" 2>/dev/null || true
    fi
}

notify "Steam sign-in required" \
    "Sign in to Steam (tick 'Remember me') to enter Game Mode. The system switches to Game Mode automatically after sign-in."

# ---------- Wait for the network ----------
# This runs from XDG autostart, which fires as soon as the desktop session
# comes up — typically before NetworkManager has finished associating with the
# pre-seeded Wi-Fi. Starting Steam there means its first act is a client update
# against no route at all, which it reports as a connection failure rather than
# retrying cleanly. Bounded, because a machine with no network at all should
# still reach a usable desktop with Steam open rather than hang here.
NETWORK_WAIT_SECONDS=45
_net_deadline=$(( $(date +%s) + NETWORK_WAIT_SECONDS ))
while [ "$(date +%s)" -lt "$_net_deadline" ]; do
    if ip route show default 2>/dev/null | grep -q .; then
        echo "[first-login] Network is up"
        break
    fi
    sleep 2
done
if ! ip route show default 2>/dev/null | grep -q .; then
    echo >&2 "[first-login] No default route after ${NETWORK_WAIT_SECONDS}s; starting Steam anyway"
    notify "No network" \
        "Steam is starting without a connection. Connect to Wi-Fi and sign in to enter Game Mode."
fi

# Windowed Steam login (regular client; QR code and keyboard available).
echo "[first-login] Launching windowed Steam for sign-in"
steam &
steam_pid=$!

# Watch for credentials. Poll instead of inotify so this works without
# extra dependencies. The loop dies with the desktop session (greetd
# teardown kills the autostart entry's children).
while :; do
    if /usr/bin/steam-login-check; then
        echo "[first-login] Steam login detected; returning to gamemode"
        notify "Signed in to Steam" "Returning to Game Mode in 15 seconds…"
        sleep 15
        exec /usr/bin/return-to-gamemode
    fi
    # Stop watching if the user closed Steam without signing in.
    # (Note: closing the Steam window only minimizes it to the tray;
    # this fires when Steam actually exits.)
    if ! kill -0 "$steam_pid" 2>/dev/null; then
        echo "[first-login] Steam exited without a login; staying on desktop"
        notify "Steam sign-in skipped" \
            "Sign in to Steam and run 'return-to-gamemode' when ready."
        exit 0
    fi
    sleep 3
done
