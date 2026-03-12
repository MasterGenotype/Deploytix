#!/usr/bin/bash

set -e

die() { echo >&2 "!! $*"; exit 1; }

SENTINEL_FILE="session-select"

# If we proceed, chain to the detected desktop session
CHAINED_SESSION=""

# --- Detect desktop environment ---
if command -v startplasma-wayland &>/dev/null; then
  CHAINED_SESSION="/usr/bin/startplasma-wayland"
elif command -v gnome-session &>/dev/null; then
  CHAINED_SESSION="/usr/bin/gnome-session"
elif command -v startxfce4 &>/dev/null; then
  CHAINED_SESSION="/usr/bin/startxfce4"
else
  die "No supported desktop environment found (checked: plasma, gnome, xfce)"
fi

# --- Check sentinel file ---
check_sentinel() {
  if [[ -z ${HOME+x} ]]; then
    echo >&2 "$0: No \$HOME variable!"
    return 0
  fi

  local config_dir="${XDG_CONF_DIR:-"$HOME/.config"}"

  if [[ ! -f "$config_dir/$SENTINEL_FILE" ]]; then
    return 1
  fi

  local sentinel_value
  sentinel_value="$(cat "$config_dir/$SENTINEL_FILE")"

  case "$sentinel_value" in
    wayland|x11)
      # For KDE, select wayland vs x11 based on sentinel
      if command -v startplasma-wayland &>/dev/null; then
        if [[ "$sentinel_value" == "x11" ]]; then
          echo "/usr/bin/startplasma-x11"
        else
          echo "/usr/bin/startplasma-wayland"
        fi
      elif command -v gnome-session &>/dev/null; then
        echo "/usr/bin/gnome-session"
      elif command -v startxfce4 &>/dev/null; then
        echo "/usr/bin/startxfce4"
      fi
      rm -f "$config_dir/$SENTINEL_FILE"
      return 0
    ;;
    *)
      return 1
    ;;
  esac
}

if CONFIGURED_SESSION=$(check_sentinel); then
  # Sentinel found and consumed — launch desktop session
  echo >&2 "$0: Found and removed sentinel, launching one-shot desktop session"
  # Launch the desktop session; when it exits, restore gamescope
  "$CONFIGURED_SESSION" || true
  echo >&2 "$0: Desktop session exited, restoring gamescope session"
  /usr/bin/session-select gamescope 2>/dev/null || true
else
  echo >&2 "$0: No sentinel found, restoring gamescope session"
  /usr/bin/session-select gamescope 2>/dev/null || true
  # Fallback: run the desktop session anyway so greetd doesn't loop
  exec "$CHAINED_SESSION"
fi
