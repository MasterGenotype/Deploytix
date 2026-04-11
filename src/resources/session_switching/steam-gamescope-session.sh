#!/bin/sh
# ==================================================
# Steam + Gamescope System Session
#
# Auto-detects output resolution from DRM and applies the gamescope
# launch profile used by the Deploytix reference handheld configuration.
#
# Override any auto-detected value with environment variables:
#   WIDTH=2560 HEIGHT=1440 REFRESH=165 PROFILE=quality steam-gamescope-session
# ==================================================

PROFILE=${PROFILE:-balanced}   # latency, quality, balanced
DEBUG=${DEBUG:-0}

# --------- 1. GPU Detection ---------
GPU=$(lspci | grep -i 'vga' | tr '[:upper:]' '[:lower:]')
if echo "$GPU" | grep -q 'intel'; then
    export MESA_LOADER_DRIVER_OVERRIDE=iris
elif echo "$GPU" | grep -q 'amd'; then
    export RADV_PERFTEST=aco
elif echo "$GPU" | grep -q 'nvidia'; then
    export __GL_SYNC_TO_VBLANK=0
fi

# --------- 2. Vulkan Layer Configuration ---------
# vkBasalt opts in only when the user has a config file.
VKCONF="$HOME/.config/vkBasalt/vkBasalt.conf"
if [ -f "$VKCONF" ]; then
    export ENABLE_VKBASALT=1
else
    export ENABLE_VKBASALT=0
fi
export ENABLE_GAMESCOPE_WSI=1
export MANGOHUD=${MANGOHUD:-0}

# --------- 3. Detect Output Resolution ---------
# Read the preferred (first-listed) mode from the first connected DRM
# connector.  Falls back to 1920x1080 when detection fails.  Any of
# WIDTH / HEIGHT / REFRESH can be pre-set in the environment to skip
# auto-detection for that value.
if [ -z "${WIDTH:-}" ] || [ -z "${HEIGHT:-}" ]; then
    for connector in /sys/class/drm/card[0-9]*-*/; do
        [ -f "$connector/status" ] || continue
        [ "$(cat "$connector/status")" = "connected" ] || continue
        mode=$(head -1 "$connector/modes" 2>/dev/null)
        if [ -n "$mode" ]; then
            WIDTH="${mode%%x*}"
            HEIGHT="${mode##*x}"
            break
        fi
    done
fi

WIDTH=${WIDTH:-1920}
HEIGHT=${HEIGHT:-1080}
REFRESH=${REFRESH:-0}   # 0 = let gamescope auto-detect

# --------- 4. Library Detection ---------
[ -f /usr/lib/libgamemodeauto.so.0 ] && \
    LD_PRELOAD="${LD_PRELOAD:+$LD_PRELOAD:}/usr/lib/libgamemodeauto.so.0"

[ -f /usr/lib/liblatencyflex.so ] && \
    LD_PRELOAD="${LD_PRELOAD:+$LD_PRELOAD:}/usr/lib/liblatencyflex.so"

export LD_PRELOAD

# --------- 5. Environment ---------
export XDG_CURRENT_DESKTOP=gamescope

# --------- 6. Gamescope Args ---------
# Mirrors the handheld reference configuration:
#   -e                      — enable Steam integration
#   --adaptive-sync         — VRR when the panel supports it
#   --force-grab-cursor     — keep cursor inside the session
#   --force-windows-fullscreen — fullscreen all top-level windows
#   --sdr-gamut-wideness    — widen SDR gamut on modern panels
#   --rt                    — use SCHED_RR for the compositor thread
REFRESH_ARG=""
[ "$REFRESH" -gt 0 ] 2>/dev/null && REFRESH_ARG="-r $REFRESH"

GAMESCOPE_ARGS="\
    -w $WIDTH -h $HEIGHT \
    $REFRESH_ARG \
    -f \
    -e \
    --adaptive-sync \
    --force-grab-cursor \
    --force-windows-fullscreen \
    --sdr-gamut-wideness 0.77 \
    --rt"

# --------- 7. Debug Logging ---------
[ "$DEBUG" = "1" ] && {
    echo "=== Steam Gamescope Session ==="
    echo "PROFILE: $PROFILE"
    echo "GPU: $GPU"
    echo "Output: ${WIDTH}x${HEIGHT} (refresh: ${REFRESH:-auto})"
    echo "LD_PRELOAD: $LD_PRELOAD"
    echo "ENABLE_VKBASALT: $ENABLE_VKBASALT"
    echo "ENABLE_GAMESCOPE_WSI: $ENABLE_GAMESCOPE_WSI"
    echo "MANGOHUD: $MANGOHUD"
    echo "Gamescope Args: $GAMESCOPE_ARGS"
    echo "Launching Steam..."
}

# --------- 8. Launch Audio + Steam ---------
[ -x "$HOME/.local/bin/audio-startup" ] && "$HOME/.local/bin/audio-startup"
exec /usr/bin/gamescope $GAMESCOPE_ARGS -- steam -steamos3
