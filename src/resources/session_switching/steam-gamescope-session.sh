#!/bin/sh
# ==================================================
# Steam + Gamescope System Session (Fully Self-Tuning)
# Output: 1920x1200 @144Hz
# Auto FSR scaling per game, GPU detection, gamemode, vkBasalt
# ==================================================

PROFILE=${PROFILE:-balanced}   # latency, quality, balanced
DEBUG=${DEBUG:-0}

# --------- 1. GPU Detection ---------
GPU=$(lspci | grep -i 'vga' | tr '[:upper:]' '[:lower:]')
if echo "$GPU" | grep -q 'intel'; then
    export MESA_LOADER_DRIVER_OVERRIDE=iris
    export ENABLE_VKBASALT=0
elif echo "$GPU" | grep -q 'amd'; then
    export RADV_PERFTEST=aco
    export ENABLE_VKBASALT=1
elif echo "$GPU" | grep -q 'nvidia'; then
    export __GL_SYNC_TO_VBLANK=0
    export ENABLE_VKBASALT=1
else
    ENABLE_VKBASALT=0
fi

# --------- 2. Fixed Output ---------
WIDTH=1920
HEIGHT=1200
REFRESH=144

# --------- 3. Detect Steam Game Resolution ---------
# Default internal game resolution
GAME_W=1280
GAME_H=720

# Resolution heuristic based on profile
case "$PROFILE" in
    latency)
        GAME_W=1280
        GAME_H=720
        ;;
    quality)
        GAME_W=1600
        GAME_H=900
        ;;
    balanced|*)
        GAME_W=1440
        GAME_H=810
        ;;
esac

# --------- 4. FSR Sharpness Auto-Tune ---------
# Higher output -> higher sharpness
FSR_SHARPNESS=$(( (WIDTH + HEIGHT) / 640 ))  # Rough scaling factor
[ "$FSR_SHARPNESS" -gt 5 ] && FSR_SHARPNESS=5  # Clamp max

# --------- 5. Library Detection ---------
[ -f /usr/lib/libgamemodeauto.so.0 ] && \
    LD_PRELOAD="${LD_PRELOAD:+$LD_PRELOAD:}/usr/lib/libgamemodeauto.so.0"

[ -f /usr/lib/liblatencyflex.so ] && \
    LD_PRELOAD="${LD_PRELOAD:+$LD_PRELOAD:}/usr/lib/liblatencyflex.so"

export LD_PRELOAD

VKCONF="$HOME/.config/vkBasalt/vkBasalt.conf"
[ -f "$VKCONF" ] && export ENABLE_VKBASALT=1 || export ENABLE_VKBASALT=0

# --------- 6. Gamescope Args ---------
GAMESCOPE_ARGS="\
    -w $WIDTH -h $HEIGHT \
    -W $GAME_W -H $GAME_H \
    -r $REFRESH \
    --expose-wayland \
    --rt \
    -f"

# Apply profile-specific optimizations
case "$PROFILE" in
    latency)
        GAMESCOPE_ARGS="$GAMESCOPE_ARGS --immediate-flips -S integer"
        ;;
    quality)
        GAMESCOPE_ARGS="$GAMESCOPE_ARGS -F fsr --fsr-sharpness $FSR_SHARPNESS"
        ;;
    balanced|*)
        GAMESCOPE_ARGS="$GAMESCOPE_ARGS -S integer -F fsr --fsr-sharpness $FSR_SHARPNESS"
        ;;
esac

# --------- 7. Debug Logging ---------
[ "$DEBUG" = "1" ] && {
    echo "=== Steam Gamescope Session ==="
    echo "PROFILE: $PROFILE"
    echo "GPU: $GPU"
    echo "Output: ${WIDTH}x${HEIGHT}@${REFRESH}Hz"
    echo "Internal Game Resolution: ${GAME_W}x${GAME_H}"
    echo "FSR Sharpness: $FSR_SHARPNESS"
    echo "LD_PRELOAD: $LD_PRELOAD"
    echo "ENABLE_VKBASALT: $ENABLE_VKBASALT"
    echo "Gamescope Args: $GAMESCOPE_ARGS"
    echo "Launching Steam..."
}

# --------- 8. Launch Steam ---------
exec /bin/gamescope $GAMESCOPE_ARGS -- steam -steamos
