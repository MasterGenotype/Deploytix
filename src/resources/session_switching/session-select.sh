#!/usr/bin/bash

set -e

die() { echo >&2 "!! $*"; exit 1; }

# --- Configuration paths ---
GREETD_CONF="/etc/greetd/config.toml"
SENTINEL_FILE="session-select"
DESKTOP_AUTOLOGIN_FLAG="/etc/bazzite/desktop_autologin"

session="${1:-gamescope}"
session_type="wayland"

if [[ "$2" == "--sentinel-created" ]]; then
  SENTINEL_CREATED=1
  session_type="${3:-wayland}"
fi

# --- Detect desktop environment by checking installed binaries ---
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

# --- Map session name to a launch command for greetd initial_session ---
session_command=""
create_sentinel=""

DE=$(detect_de)

case "$session" in
  plasma-wayland-persistent)
    session_command="dbus-launch startplasma-wayland"
  ;;
  plasma-x11-persistent)
    session_command="dbus-launch startplasma-x11"
  ;;
  gnome-wayland-persistent)
    session_command="dbus-launch gnome-session"
  ;;
  desktop)
    session_command="/usr/bin/desktop-session-oneshot"
    create_sentinel=1
  ;;
  gamescope)
    session_command="gamescope-session"
    create_sentinel=1
  ;;
  *)
    echo >&2 "!! Unrecognized session '$session'"
    exit 1
  ;;
esac

# --- Update sentinel file (as regular user) ---
if [[ -z $SENTINEL_CREATED ]]; then
  [[ $EUID == 0 ]] && die "Running $0 as root is not allowed"

  [[ -n ${HOME+x} ]] || die "No \$HOME variable"
  config_dir="${XDG_CONF_DIR:-"$HOME/.config"}"
  session_type=$(
    mkdir -p "$config_dir"
    if [[ -f "$config_dir/session-type" ]]; then
      cp "$config_dir/session-type" "$config_dir/$SENTINEL_FILE"
    else
      echo "wayland" > "$config_dir/$SENTINEL_FILE"
    fi
    cat "$config_dir/$SENTINEL_FILE"
  )

  export SENTINEL_CREATED=1
fi

echo "Updated user selected session to '$session' (command: $session_command)"

# --- Become root via pkexec ---
if [[ $EUID != 0 ]]; then
  exec pkexec "$(realpath "$0")" "$session" --sentinel-created "$session_type"
  exit 1
fi

# --- Detect the login user (first UID >= 1000) ---
LOGIN_USER=$(getent passwd | awk -F: '$3 >= 1000 && $3 < 60000 {print $1; exit}')
[[ -n "$LOGIN_USER" ]] || die "Cannot detect login user"

# --- Rewrite greetd config.toml ---
cat > "$GREETD_CONF" <<EOF
[terminal]
vt = 1

[default_session]
command = "agreety --cmd /bin/bash"
user = "greeter"

[initial_session]
command = "$session_command"
user = "$LOGIN_USER"
EOF

echo "Updated greetd config: initial_session.command = $session_command (user: $LOGIN_USER)"

# --- Detect init system and restart greetd ---
restart_greetd() {
  if [[ -d /run/runit/supervise.greetd ]] || command -v sv &>/dev/null && [[ -d /etc/runit/sv/greetd ]]; then
    echo "Restarting greetd via runit..."
    sv restart greetd
  elif [[ -d /run/openrc ]] || command -v rc-service &>/dev/null; then
    echo "Restarting greetd via openrc..."
    rc-service greetd restart
  elif [[ -d /run/s6-rc ]] || [[ -d /etc/s6/sv/greetd-srv ]]; then
    echo "Restarting greetd via s6..."
    # s6: bring down then up, with timeouts
    s6-svc -wD -T 5000 /run/service/greetd-srv 2>/dev/null || true
    s6-svc -wu -T 5000 /run/service/greetd-srv 2>/dev/null || true
    # Fallback: use s6-rc if direct svc fails
    if [[ $? -ne 0 ]]; then
      s6-rc -d change greetd-srv 2>/dev/null || true
      s6-rc -u change greetd-srv 2>/dev/null || true
    fi
  elif command -v dinitctl &>/dev/null; then
    echo "Restarting greetd via dinit..."
    dinitctl restart greetd
  else
    die "Cannot detect init system to restart greetd"
  fi
}

restart_greetd
echo "Restarted greetd"
