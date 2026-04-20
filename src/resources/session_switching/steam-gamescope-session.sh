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

# --------- 1. Seat & Session Environment ---------
# Use logind backend to avoid seatd/elogind dual-seat conflict
export LIBSEAT_BACKEND=logind

export XDG_SESSION_TYPE=wayland
export XDG_CURRENT_DESKTOP=gamescope
export XDG_SESSION_DESKTOP=gamescope
: "${XDG_RUNTIME_DIR:=/run/user/$(id -u)}"
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
export LD_PRELOAD

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
GAMESCOPE_CMD="/usr/local/bin/gamescope \
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

# --------- 9. Audio ---------
# Never let a flaky audio-startup tear down the whole session. audio-startup
# may legitimately return non-zero on a fresh boot (e.g. its own `set -e`
# tripping on a `pkill` that matched no stale daemon), and with `set -e`
# above, the final command after the final `&&` in a list is NOT protected
# from exiting the shell. Use an explicit `if`+`|| true` to isolate it.
if [ -x "$HOME/.local/bin/audio-startup" ]; then
    echo "[steam-session] Running audio-startup"
    "$HOME/.local/bin/audio-startup" || \
        echo "[steam-session] audio-startup returned non-zero; continuing"
fi

# --------- 10. Launch Gamescope (background) ---------
echo "[steam-session] Starting gamescope..."
$GAMESCOPE_CMD &
gamescope_pid=$!

# --------- 11. Wait for Ready ---------
if read -r response_x_display response_wl_display <>"$socket"; then
    export DISPLAY="$response_x_display"
    export GAMESCOPE_WAYLAND_DISPLAY="$response_wl_display"
    echo "[steam-session] Gamescope ready: DISPLAY=$DISPLAY GAMESCOPE_WAYLAND_DISPLAY=$GAMESCOPE_WAYLAND_DISPLAY"
else
    echo >&2 "[steam-session] Gamescope failed to start"
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

# --------- 12. Launch Steam ---------
echo "[steam-session] Starting Steam (-steamos3 -gamepadui)..."
steam -steamos3 -gamepadui
steam_ret=$?
echo "[steam-session] Steam exited ($steam_ret)"

# --------- 13. Cleanup ---------
kill "$gamescope_pid" 2>/dev/null
sleep 2 &
sleep_pid=$!
wait -n "$gamescope_pid" "$sleep_pid" 2>/dev/null || true
for job in $(jobs -p); do
    kill -9 "$job" 2>/dev/null
done
kill "$DBUS_SESSION_BUS_PID" 2>/dev/null
rm -rf "$tmpdir"
exit "$steam_ret"
