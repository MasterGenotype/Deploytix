#!/bin/sh
# ==================================================
# Steam + Gamescope System Session
# Uses ready-fd socket approach (per gamescope-session-plus)
# to properly coordinate gamescope and Steam startup.
# ==================================================

set -e

# --------- 0. Logging ---------
# When launched by greetd IPC (the normal path), this process has fresh stdio
# that is NOT inherited from deploytix-session-manager, so nothing we echo
# here ends up in deploytix-session.log. Redirect our own output so early
# failures (dbus-launch, mktemp, gamescope startup, audio-startup, etc.) are
# visible for diagnosis.
_LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}"
mkdir -p "$_LOG_DIR" 2>/dev/null || true
exec >>"$_LOG_DIR/steam-gamescope-session.log" 2>&1
echo "[steam-session] ==== starting at $(date -Is) pid=$$ uid=$(id -u) ===="
echo "[steam-session] env: USER=${USER:-?} HOME=${HOME:-?} XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-?} XDG_SEAT=${XDG_SEAT:-?} XDG_SESSION_ID=${XDG_SESSION_ID:-?} XDG_VTNR=${XDG_VTNR:-?}"

# --------- 0.5 Short-session tracker ---------
# Ported from ChimeraOS gamescope-session-plus, which uses the same 60s /
# 5-strike thresholds. A session that dies in under $SHORT_SESSION_SECONDS
# never presented a UI; $SHORT_SESSION_LIMIT of those in a row means the
# client is broken rather than merely unlucky. Without this, a Steam that
# cannot start (no network on first boot, so steamwebhelper — which *is*
# the gamepad UI — never initialises) respawns through greetd forever and
# nothing ever reaches the screen.
SHORT_SESSION_SECONDS=60
SHORT_SESSION_LIMIT=5
SHORT_SESSION_TRACKER="$_LOG_DIR/deploytix-short-sessions"
SENTINEL="${XDG_CONFIG_HOME:-$HOME/.config}/deploytix-session"

# Point the next session at the desktop. deploytix-session-manager consumes
# (and deletes) the sentinel on its next run.
_route_to_desktop() {
    mkdir -p "$(dirname "$SENTINEL")" 2>/dev/null || true
    if echo "desktop" > "$SENTINEL" 2>/dev/null; then
        echo "[steam-session] Next session -> desktop"
    else
        echo >&2 "[steam-session] WARNING: could not write $SENTINEL (is \$HOME writable?)"
    fi
}

# Repair what a half-finished Steam install most often loses, then hand the
# user a desktop — where the steam-first-login autostart entry offers a
# windowed sign-in and returns to gamemode afterwards.
short_session_recover() {
    echo "[steam-session] Recovering: clearing widevine cache, re-seeding Steam bootstrap"
    mkdir -p "$HOME/.local/share/Steam" 2>/dev/null || true
    rm -rf --one-file-system "$HOME/.local/share/Steam/config/widevine" 2>/dev/null || true
    for _bootstrap in /usr/lib/steam/bootstraplinux_ubuntu12_32.tar.xz \
                      /etc/first-boot/bootstraplinux_ubuntu12_32.tar.xz; do
        [ -f "$_bootstrap" ] || continue
        if tar xf "$_bootstrap" -C "$HOME/.local/share/Steam" 2>/dev/null; then
            echo "[steam-session] Re-extracted $_bootstrap"
            break
        fi
    done
    _route_to_desktop
}

_short_session_count=0
if [ -f "$SHORT_SESSION_TRACKER" ]; then
    _short_session_count=$(wc -l < "$SHORT_SESSION_TRACKER" 2>/dev/null || echo 0)
fi

if [ "$_short_session_count" -ge "$SHORT_SESSION_LIMIT" ]; then
    echo >&2 "[steam-session] $_short_session_count consecutive short sessions — Steam is not starting here."
    short_session_recover
    rm -f "$SHORT_SESSION_TRACKER"
    exit 1
fi

_session_start=$(date +%s)

# Record how long this session lasted: a short one adds a strike, a real
# one clears the record. Returns non-zero for a short session, so callers
# must invoke it as a condition or with `|| true` under `set -e`.
_record_session_outcome() {
    _elapsed=$(( $(date +%s) - _session_start ))
    if [ "$_elapsed" -lt "$SHORT_SESSION_SECONDS" ]; then
        echo "session failed after ${_elapsed}s" >> "$SHORT_SESSION_TRACKER" 2>/dev/null || true
        echo "[steam-session] Short session (${_elapsed}s) — strike $((_short_session_count + 1))/$SHORT_SESSION_LIMIT"
        return 1
    fi
    rm -f "$SHORT_SESSION_TRACKER"
    echo "[steam-session] Session lasted ${_elapsed}s; short-session record cleared"
    return 0
}

# --------- 1. Seat & Session Environment ---------
# Select libseat backend adaptively:
# - Prefer logind (elogind) when its D-Bus service is reachable; greetd's PAM
#   stack (pam_elogind) already created an active seat session that grants
#   DRM/input ACLs, and forcing logind avoids seatd/elogind dual-seat
#   confusion when both daemons are present (non-S6 installs).
# - Fall back to seatd when elogind is not running (S6 installs, or any
#   system that ships only seatd).  The user must be in the 'seat' group.
if pidof elogind >/dev/null 2>&1; then
    export LIBSEAT_BACKEND=logind
    echo "[steam-session] libseat backend: logind (elogind running)"
elif [ -S /run/seatd.sock ]; then
    export LIBSEAT_BACKEND=seatd
    echo "[steam-session] libseat backend: seatd"
else
    echo "[steam-session] warning: neither elogind nor seatd detected; libseat will auto-detect"
fi

export XDG_SESSION_TYPE=wayland
export XDG_CURRENT_DESKTOP=gamescope
export XDG_SESSION_DESKTOP=gamescope
# XDG_RUNTIME_DIR is created by pam_elogind on normal greetd sessions.
# On S6 (no greetd/pam) it may be absent; create it here so mktemp and
# PipeWire sockets have a place to live.
: "${XDG_RUNTIME_DIR:=/run/user/$(id -u)}"
if [ ! -d "$XDG_RUNTIME_DIR" ]; then
    mkdir -p "$XDG_RUNTIME_DIR"
    chmod 700 "$XDG_RUNTIME_DIR"
    echo "[steam-session] Created XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR"
fi
export XDG_RUNTIME_DIR

# --------- 2. GPU / Vulkan ---------
export ENABLE_GAMESCOPE_WSI=1
export ENABLE_VKBASALT=0
export MANGOHUD=0
export mesa_glthread=true

# --------- 3. Misc Steam / Game Env ---------
export SDL_VIDEO_MINIMIZE_ON_FOCUS_LOSS=0
export vk_xwayland_wait_ready=false
export GAMESCOPE_NV12_COLORSPACE=k_EStreamColorspace_BT601
export VKD3D_SWAPCHAIN_LATENCY_FRAMES=3

# Legion Go S refresh rates
export STEAM_DISPLAY_REFRESH_LIMITS=60,144

# --------- 4. Library Detection ---------
[ -f /usr/lib/libgamemodeauto.so.0 ] && \
    LD_PRELOAD="${LD_PRELOAD:+$LD_PRELOAD:}/usr/lib/libgamemodeauto.so.0"
[ -f /usr/lib/liblatencyflex.so ] && \
    LD_PRELOAD="${LD_PRELOAD:+$LD_PRELOAD:}/usr/lib/liblatencyflex.so"
[ -n "$LD_PRELOAD" ] && export LD_PRELOAD

# --------- 5. D-Bus Session Bus ---------
# Start D-Bus independently so it persists even if gamescope restarts.
eval "$(dbus-launch --sh-syntax)"
export DBUS_SESSION_BUS_ADDRESS DBUS_SESSION_BUS_PID

# --------- 6. Output Resolution ---------
WIDTH=1920
HEIGHT=1200

# --------- 7. Create Sockets ---------
# Ready-fd socket: gamescope writes DISPLAY and WAYLAND_DISPLAY here when ready.
# Stats pipe: used by mangoapp and Steam for perf data.
tmpdir=$(mktemp -p "$XDG_RUNTIME_DIR" -d -t gamescope.XXXXXXX)
socket="$tmpdir/startup.socket"
stats="$tmpdir/stats.pipe"
mkfifo -- "$socket"
mkfifo -- "$stats"
export GAMESCOPE_STATS="$stats"

# Claim global session stats link
sessionlink="$XDG_RUNTIME_DIR/gamescope-stats"
lockfile="$sessionlink.lck"
exec 9>"$lockfile"
if flock -n 9 && rm -f "$sessionlink" && ln -sf "$tmpdir" "$sessionlink"; then
    echo "[steam-session] Claimed global stats session at '$sessionlink'"
fi

# --------- 8. Gamescope Command ---------
GAMESCOPE_CMD="/usr/bin/gamescope \
    -w $WIDTH -h $HEIGHT \
    -f \
    --steam \
    --xwayland-count 2 \
    --force-windows-fullscreen \
    --force-grab-cursor \
    --sdr-gamut-wideness 0.77 \
    --adaptive-sync \
    --custom-refresh-rates 60,144 \
    --rt \
    -R $socket \
    -T $stats"

# --------- 9. Launch Gamescope (background) ---------
# Gamescope goes first and nothing between here and the ready-fd read is
# allowed to block: compositor startup (DRM master, Vulkan device init,
# two Xwayland servers) is the long pole on the boot -> Steam path, so it
# must be running while we do everything else rather than after it.
echo "[steam-session] Starting gamescope..."
$GAMESCOPE_CMD &
gamescope_pid=$!

# --------- 10. Audio (background, parallel with gamescope startup) ---------
# Steam does not need PipeWire to exist before it launches — it opens audio
# devices lazily — so audio-startup runs concurrently with gamescope's
# initialisation instead of ahead of it. Running it in the foreground here
# used to add its full runtime to every boot before gamescope was even
# spawned; now it overlaps with work we are waiting on anyway.
#
# Never let a flaky audio-startup tear down the whole session. audio-startup
# may legitimately return non-zero on a fresh boot (e.g. its own `set -e`
# tripping on a `pkill` that matched no stale daemon). Backgrounding it also
# keeps `set -e` out of the picture: a non-zero exit from an unwaited-for
# background job cannot kill this shell.
if [ -x "$HOME/.local/bin/audio-startup" ]; then
    "$HOME/.local/bin/audio-startup" &
    echo "[steam-session] Running audio-startup in background (pid=$!)"
fi

# --------- 11. Wait for Ready ---------
if read -r response_x_display response_wl_display <>"$socket"; then
    export DISPLAY="$response_x_display"
    export GAMESCOPE_WAYLAND_DISPLAY="$response_wl_display"
    echo "[steam-session] Gamescope ready: DISPLAY=$DISPLAY GAMESCOPE_WAYLAND_DISPLAY=$GAMESCOPE_WAYLAND_DISPLAY"
else
    echo >&2 "[steam-session] Gamescope failed to start"
    _record_session_outcome || true
    kill -9 "$gamescope_pid" 2>/dev/null
    wait "$gamescope_pid" 2>/dev/null
    rm -rf "$tmpdir"
    exit 1
fi

# Propagate display variables to D-Bus activation environment
dbus-update-activation-environment DISPLAY GAMESCOPE_WAYLAND_DISPLAY \
    XDG_CURRENT_DESKTOP XDG_SESSION_TYPE XDG_SESSION_DESKTOP 2>/dev/null || true

# Tell gamescope to focus Steam (app ID 769) as the base layer
xprop -root -f GAMESCOPECTRL_BASELAYER_APPID 32c \
    -set GAMESCOPECTRL_BASELAYER_APPID 769

# --------- 12. Cleanup handler (runs on exit or signal) ---------
_cleaned=0
cleanup() {
    [ "$_cleaned" -ne 0 ] && return 0
    _cleaned=1
    echo "[steam-session] Cleanup: tearing down session (pid=$$)"

    # Phase 1: Kill child processes of gamescope (Xwayland, etc.) first
    if [ -n "${gamescope_pid:-}" ]; then
        for child in $(pgrep -P "$gamescope_pid" 2>/dev/null); do
            kill "$child" 2>/dev/null || true
        done
    fi
    # Kill Steam and its children
    if [ -n "${steam_pid:-}" ]; then
        kill "$steam_pid" 2>/dev/null || true
    fi
    pkill -x steamwebhelper 2>/dev/null || true
    pkill -f 'Xwayland :'  2>/dev/null || true

    sleep 1

    # Phase 2: Now kill gamescope itself
    if [ -n "${gamescope_pid:-}" ]; then
        kill "$gamescope_pid" 2>/dev/null || true
    fi

    # Phase 3: Wait for gamescope exit (2s timeout)
    sleep 2 &
    _sp=$!
    if [ -n "${gamescope_pid:-}" ]; then
        wait -n "$gamescope_pid" "$_sp" 2>/dev/null || true
    else
        wait "$_sp" 2>/dev/null || true
    fi

    # Phase 4: Force-kill any remaining jobs
    for job in $(jobs -p); do
        kill -9 "$job" 2>/dev/null || true
    done

    # Phase 5: Tear down D-Bus and temp files
    if [ -n "${DBUS_SESSION_BUS_PID:-}" ]; then
        kill "$DBUS_SESSION_BUS_PID" 2>/dev/null || true
    fi
    rm -rf "${tmpdir:-}"
}
trap cleanup EXIT HUP TERM

# --------- 13. Launch Steam ---------
# Run Steam in background + wait so that signal traps (TERM/HUP) fire
# immediately instead of being deferred until the foreground child exits.
# Without this, a hung Steam process blocks the cleanup trap indefinitely,
# preventing greetd from restarting the session manager on logout.
#
# Flags match the upstream ChimeraOS/Bazzite gamescope-session launch line.
# -steampal -steamdeck are required in addition to -steamos3 -gamepadui so
# that a fresh install (no cached credentials) presents the Steam Deck OOBE:
# language -> network setup -> controller-navigable login with on-screen
# keyboard and QR-code sign-in. Without them, Steam falls back to the legacy
# X11 login dialog, which is unreachable under gamescope's forced-fullscreen
# base layer. -steamdeck makes Steam probe SteamOS update tooling; the
# steamos-update / jupiter-biosupdate / steamos-select-branch stubs in
# /usr/bin satisfy those probes.
echo "[steam-session] Starting Steam (-gamepadui -steamos3 -steampal -steamdeck)..."
steam -gamepadui -steamos3 -steampal -steamdeck &
steam_pid=$!

steam_ret=0
wait "$steam_pid" 2>/dev/null || steam_ret=$?
echo "[steam-session] Steam exited ($steam_ret)"

# --------- 14. First-login fallback ---------
# Steam's gamepad-UI login screen (QR code + on-screen keyboard) is the
# primary sign-in path and runs right here in gamescope. But if Steam
# exited while there is still no remembered account, the user could not
# (or chose not to) sign in under gamescope — OSK/text input is not
# fully reliable there pre-login. Route the next session to the
# desktop, where the steam-first-login autostart entry offers a
# windowed sign-in and automatically returns to gamemode afterward.
#
# Gated on the session having lasted long enough to be a sign-in attempt.
# A Steam that exits in seconds never showed a login screen — it crashed —
# and routing that to the desktop turns a crash into a session flip-flop.
# Those are strikes for the short-session tracker to act on instead.
if _record_session_outcome; then
    if ! /usr/bin/steam-login-check; then
        echo "[steam-session] No Steam login on exit; first-login fallback"
        _route_to_desktop
    fi
else
    echo "[steam-session] Exit was too fast to be a sign-in attempt; leaving session selection alone"
fi

exit "$steam_ret"
