#!/usr/bin/bash

set -e

die() { echo >&2 "!! $*"; exit 1; }

GREETD_CONF="/etc/greetd/config.toml"

# --- Detect the login user (first UID >= 1000) ---
LOGIN_USER=$(getent passwd | awk -F: '$3 >= 1000 && $3 < 60000 {print $1; exit}')
LOGIN_HOME=$(getent passwd "$LOGIN_USER" | cut -d: -f6)

# --- Only update greetd config if Steam/gamescope is available ---
if [[ -f "$LOGIN_HOME/.local/share/Steam/ubuntu12_32/steamui.so" ]] || command -v gamescope-session &>/dev/null; then
  cat > "$GREETD_CONF" <<EOF
[terminal]
vt = 1

[default_session]
command = "agreety --cmd /bin/bash"
user = "greeter"

[initial_session]
command = "gamescope-session"
user = "$LOGIN_USER"
EOF
  echo "Updated greetd config for gamescope-session"
fi

# --- Detect desktop environment and log out ---
detect_de() {
  if command -v startplasma-wayland &>/dev/null; then
    echo "kde"
  elif command -v gnome-session &>/dev/null; then
    echo "gnome"
  elif command -v startxfce4 &>/dev/null; then
    echo "xfce"
  else
    echo "unknown"
  fi
}

DE=$(detect_de)

case "$DE" in
  kde)
    sudo -Eu "$LOGIN_USER" qdbus org.kde.Shutdown /Shutdown org.kde.Shutdown.logout
  ;;
  gnome)
    sudo -Eu "$LOGIN_USER" gnome-session-quit --logout --no-prompt
  ;;
  xfce)
    sudo -Eu "$LOGIN_USER" xfce4-session-logout --logout
  ;;
  *)
    echo >&2 "Warning: Unknown desktop environment, cannot auto-logout"
    echo >&2 "Please log out manually to return to Game Mode"
  ;;
esac
