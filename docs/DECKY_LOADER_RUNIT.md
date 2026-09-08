# Decky Loader on runit — Installation & Runtime Requirements

Audience: agents and humans implementing (or correcting) Decky Loader support
in deploytix on Artix/runit handhelds.

This document is the **verified working contract** for Decky Loader when runit
is the init system. It is centered on a live deploytix handheld install where
Decky was brought up by hand, the runit service was rewritten until stable, and
the Decky UI successfully appeared in Steam's Quick Access Menu (QAM).

Verified on **2026-09-05/06** against:

- Artix Linux + **runit**
- User account: `superphenotype`
- Package seed: `decky-loader-bin 3.1.3-1` (AUR)
- Runtime binary after self-update: **Decky Loader v3.2.8**
- Service: `plugin_loader` under runit (`sv status` → run)
- QAM: Decky frontend loaded and visible

It is **not** a description of the current in-tree deploytix Decky path. That
path is incomplete / incorrect relative to this working system; use this doc as
the target behavior when fixing deploytix.

---

## 1. Goal and success criteria

Decky Loader must:

1. Start at boot under runit as a long-running supervised service.
2. Bind its backend on `127.0.0.1:1337`.
3. Inject its frontend into Steam Game Mode via CEF remote debugging.
4. Show the Decky tab / plugins UI in the QAM.
5. Keep plugins and settings under the canonical SteamOS-style `~/homebrew`
   layout (not the AUR helper's alternate path).

Observed success signals on the reference host:

```text
sv status plugin_loader
# run: plugin_loader: (pid …) …s; run: log: (pid …) …s

# logs
[main][INFO]: Starting Decky version v3.2.8
[loader][INFO]: plugin_path: /home/<user>/homebrew/plugins
[main][INFO]: Loading Decky frontend!
[loader][INFO]: Hot reload enabled

# listen socket
ss -tln | grep 127.0.0.1:1337
```

Plus human confirmation: Decky visible in QAM.

---

## 2. System context

A deploytix gaming install is a SteamOS-like handheld stack:

- **Artix Linux** — no systemd as PID 1. Init is runit (this doc), or
  OpenRC / s6 / dinit (out of scope here except where Decky still calls
  `systemctl`).
- **Steam** in Game Mode (`-gamepadui -steamos3 -steampal -steamdeck`) under
  gamescope.
- **greetd** session switching (see `docs/SESSION_SWITCHING.md`).
- **User home** on a normal writable filesystem (often btrfs `@home`).

Decky is upstream-designed around:

- a **root** long-running loader process,
- an **unprivileged user** who owns `~/homebrew`,
- Steam's CEF debugger socket,
- and (on SteamOS/Arch packaging) **systemd** unit control via `systemctl`.

On runit, the last item must be shimmed. Everything else must match upstream
semantics closely enough that PluginLoader does not fall back to the hardcoded
username `deck` or the wrong data root.

---

## 3. Package and binary sources

### 3.1 AUR package (seed only)

| Item | Value |
|---|---|
| Package | `decky-loader-bin` |
| Provides | `decky-loader` |
| Conflicts | `decky-loader` |
| Reference version | `3.1.3-1` |
| Declared depends | none (Steam is an external runtime requirement) |

Files shipped by the package:

```text
/usr/lib/decky-loader/PluginLoader          # real loader binary (PyInstaller)
/usr/bin/decky-loader-helper                # bootstrap helper (do not use as-is)
/usr/lib/systemd/system/decky-loader@.service
```

The systemd unit is **reference only** on Artix/runit. Useful facts from it:

- `User=root`
- `Restart=always`
- `Environment=UNPRIVILEGED_PATH=…` and `PRIVILEGED_PATH=…`
- `WorkingDirectory` = the services directory next to PluginLoader
- `ExecStartPre` runs `decky-loader-helper`

### 3.2 Why the AUR helper is the wrong bootstrap on this stack

`/usr/bin/decky-loader-helper` hardcodes:

```text
$DECKY_USER_HOME/.local/var/opt/decky-loader/{services,plugins}
```

and writes CEF flag via:

```text
$DECKY_USER_HOME/.steam/steam/.cef-enable-remote-debugging
```

That data root is **not** the canonical SteamOS / upstream Decky layout
(`~/homebrew`). Plugin docs, plugins, and the loader's own expectations assume
`~/homebrew`. On the working system we:

1. Install the package for `/usr/lib/decky-loader/PluginLoader`.
2. **Bypass** `decky-loader-helper`.
3. Bootstrap `~/homebrew` ourselves.
4. Keep CEF flag under the real Steam data dir
   `~/.local/share/Steam/`, with `~/.steam/steam` as a symlink to it.

### 3.3 Runtime binary may self-update

The live host seeded PluginLoader from the package (`v3.1.3`), kept a backup as
`PluginLoader.v3.1.3.bak`, then ran **v3.2.8** after Decky's own updater path
(`/tmp/decky-upgrade/PluginLoader-3.2.8`). `.loader.version` reads `v3.2.8`.

Installers must tolerate (and not fight) in-place upgrades under
`~/homebrew/services/`.

---

## 4. Installation requirements

### 4.1 Prerequisites

| Requirement | Why |
|---|---|
| runit as PID 1 (or at least `runsv`/`sv` supervising services) | service model |
| Steam installed and able to reach Game Mode | CEF injection target |
| A real interactive user home (`/home/<user>`) | owns `~/homebrew` |
| Network at runtime | plugin store / version checks (optional at install) |
| Write access to `/etc/runit/sv/` and runsvdir | service install + enable |
| Write access under the user home | homebrew + Steam CEF flag |
| `ss` available in PATH for the systemctl shim | active-check on port 1337 |
| `svlogd` available | runit logging |

Recommended tooling used on the reference host:

- `yay` (or equivalent) to build/install `decky-loader-bin`
- `install`, `chown`, `ln`, `sv`

### 4.2 User and path parameters

Substitute the real session/gaming user everywhere:

```text
DECKY_USER=<username>                 # e.g. superphenotype
DECKY_HOME=/home/<username>
DECKY_ROOT=$DECKY_HOME/homebrew
DECKY_SERVICES=$DECKY_ROOT/services
```

Do **not** hardcode `deck` unless that is actually the account name.

### 4.3 Directory layout to create

Canonical layout (verified):

```text
/home/<user>/homebrew/
  services/
    PluginLoader              # executable loader (may self-update)
    .loader.version           # e.g. v3.2.8
    PluginLoader.*.bak        # optional package-seed backup
  plugins/                    # plugin packages live here
  settings/
    loader.json               # created/updated by Decky at runtime
  bin/
    systemctl                 # runit-aware shim (required)

/home/<user>/.local/share/Steam/
  .cef-enable-remote-debugging   # empty marker file, user-owned

/home/<user>/.steam/steam -> /home/<user>/.local/share/Steam
```

Create dirs with user ownership:

```sh
install -d -o "$DECKY_USER" -g "$DECKY_USER" \
  "$DECKY_ROOT/services" \
  "$DECKY_ROOT/plugins" \
  "$DECKY_ROOT/settings" \
  "$DECKY_HOME/homebrew/bin"
```

Notes from the live system:

- After Decky runs as root, some trees may flip to `root:root` (e.g. `plugins/`,
  and `settings/loader.json`). That is expected: Decky's platform layer chowns
  between effective root and the unprivileged user. Do not "fix" this by
  forcing the loader itself to drop privileges.
- Keep `services/PluginLoader` and `bin/systemctl` executable.

### 4.4 Seed PluginLoader into `~/homebrew`

```sh
install -m 755 -o "$DECKY_USER" -g "$DECKY_USER" \
  /usr/lib/decky-loader/PluginLoader \
  "$DECKY_SERVICES/PluginLoader"

# version tag — package version without pkgrel is fine as a seed
DECKY_VER=$(pacman -Q decky-loader-bin | awk '{print $2}' | sed 's/-[0-9]*$//')
printf 'v%s\n' "$DECKY_VER" > "$DECKY_SERVICES/.loader.version"
chown "$DECKY_USER:$DECKY_USER" "$DECKY_SERVICES/.loader.version"
```

Optional: keep the package binary as `PluginLoader.v<ver>.bak` before allowing
updates.

### 4.5 Steam CEF remote debugging (required for QAM)

Without the CEF marker, Decky's backend can run while the frontend never
injects — no QAM UI.

```sh
STEAM_DATA="$DECKY_HOME/.local/share/Steam"
STEAM_DOT="$DECKY_HOME/.steam"

install -d -o "$DECKY_USER" -g "$DECKY_USER" "$STEAM_DATA" "$STEAM_DOT"
# ~/.steam/steam must be a symlink, not a real directory
ln -sfn "$STEAM_DATA" "$STEAM_DOT/steam"

install -o "$DECKY_USER" -g "$DECKY_USER" -m 644 /dev/null \
  "$STEAM_DATA/.cef-enable-remote-debugging"
```

If Flatpak Steam is also present, the same empty marker under
`~/.var/app/com.valvesoftware.Steam/data/Steam/` is appropriate.

Steam may need a restart after the marker is first created.

### 4.6 systemctl shim (required on runit)

Decky's Linux platform code shells out to `systemctl` for service
active/stop/start/restart and related no-ops (including optional Steam Deck
units like `steam-web-debug-portforward`). On a pure runit host this throws:

```text
FileNotFoundError: [Errno 2] No such file or directory: 'systemctl'
```

That exception is noisy and interacts badly with restart / single-instance
logic (see §7). The working fix is a **minimal shim** early on `PATH`:

Path: `/home/<user>/homebrew/bin/systemctl` (mode `0755`)

Behavior verified on the reference host:

| Verb | Behavior |
|---|---|
| `is-active plugin_loader[.service]` | `active` (exit 0) if `127.0.0.1:1337` is listening; else `inactive` (exit 3) |
| `is-active` other units | `inactive` (exit 3) |
| `start/stop/restart plugin_loader[.service]` | `sv up` / `sv down` / `sv restart plugin_loader` |
| `stop steam-web-debug-portforward[.service]` | no-op success |
| `daemon-reload` | no-op success |
| `status plugin_loader[.service]` | `sv status plugin_loader` |
| unknown verbs | exit 0 (avoid task exceptions) |

Reference implementation (live copy):

```sh
#!/bin/sh
# Minimal systemctl shim for Decky Loader on non-systemd (runit) systems.
# Decky only uses systemctl for optional CEF port-forward unit control and self-restarts.
set -u

cmd="${1:-}"
shift 2>/dev/null || true

case "$cmd" in
  ""|-h|--help)
    echo "systemctl shim for Decky on runit"
    exit 0
    ;;
  is-active)
    unit="${1:-}"
    case "$unit" in
      plugin_loader|plugin_loader.service)
        if ss -tln | grep -q '127.0.0.1:1337'; then
          echo active
          exit 0
        fi
        echo inactive
        exit 3
        ;;
      *)
        echo inactive
        exit 3
        ;;
    esac
    ;;
  daemon-reload)
    exit 0
    ;;
  start)
    unit="${1:-}"
    case "$unit" in
      plugin_loader|plugin_loader.service)
        if command -v sv >/dev/null 2>&1; then
          sv up plugin_loader >/dev/null 2>&1 || true
        fi
        ;;
    esac
    exit 0
    ;;
  stop)
    unit="${1:-}"
    case "$unit" in
      plugin_loader|plugin_loader.service)
        if command -v sv >/dev/null 2>&1; then
          sv down plugin_loader >/dev/null 2>&1 || true
        fi
        ;;
      steam-web-debug-portforward|steam-web-debug-portforward.service)
        exit 0
        ;;
    esac
    exit 0
    ;;
  restart)
    unit="${1:-}"
    case "$unit" in
      plugin_loader|plugin_loader.service)
        if command -v sv >/dev/null 2>&1; then
          sv restart plugin_loader >/dev/null 2>&1 || true
        fi
        ;;
    esac
    exit 0
    ;;
  status)
    unit="${1:-}"
    if command -v sv >/dev/null 2>&1 && [ "$unit" = "plugin_loader" -o "$unit" = "plugin_loader.service" ]; then
      sv status plugin_loader
      exit $?
    fi
    echo "o ${unit:-unknown} - shimmed on runit"
    exit 3
    ;;
  *)
    exit 0
    ;;
esac
```

The runit `run` script **must** put `$DECKY_HOME/homebrew/bin` first on `PATH`
so Decky finds this shim before any real systemd binary (if one exists on PATH
from foreign tooling).

### 4.7 runit service definition

Service name: **`plugin_loader`** (matches upstream unit naming and the shim).

#### Files

```text
/etc/runit/sv/plugin_loader/
  run                 # executable
  log/run             # executable
  supervise/          # created by runsv at runtime
  log/supervise/      # created by runsv at runtime
```

#### `/etc/runit/sv/plugin_loader/run` (working copy)

```sh
#!/bin/sh
exec 2>&1

DECKY_USER=superphenotype
DECKY_HOME=/home/superphenotype
DECKY_ROOT="$DECKY_HOME/homebrew"
DECKY_SERVICES="$DECKY_ROOT/services"

# Ensure data dirs exist with correct ownership
install -d -o "$DECKY_USER" -g "$DECKY_USER" \
  "$DECKY_ROOT/services" "$DECKY_ROOT/plugins" "$DECKY_ROOT/settings" "$DECKY_HOME/homebrew/bin"

# CEF remote debugging flag required for QAM injection
STEAM_DEBUGGING_FILE="$DECKY_HOME/.local/share/Steam/.cef-enable-remote-debugging"
[ -f "$STEAM_DEBUGGING_FILE" ] || touch "$STEAM_DEBUGGING_FILE"
chown "$DECKY_USER:$DECKY_USER" "$STEAM_DEBUGGING_FILE" 2>/dev/null || true

cd "$DECKY_SERVICES" || exit 1

# PATH puts Decky systemctl shim first for non-systemd hosts
export HOME="$DECKY_HOME"
export USER="$DECKY_USER"
export UNPRIVILEGED_USER="$DECKY_USER"
export UNPRIVILEGED_PATH="$DECKY_ROOT"
export PRIVILEGED_PATH="$DECKY_ROOT"
export LOG_LEVEL=INFO
export PATH="$DECKY_HOME/homebrew/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

exec "$DECKY_SERVICES/PluginLoader"
```

Generalize `DECKY_USER` / `DECKY_HOME` for other accounts. Keep the rest.

Critical properties of this script:

1. **Runs as root** (no `chpst -u`, no privilege drop). Matches upstream
   `User=root` and is required for Game Mode reachability observed here.
2. **`exec 2>&1`** so runit logging captures stderr.
3. **`cd` into `…/services`** before exec (working directory contract).
4. **`exec` PluginLoader** so it is PID 1 of the runsv leaf (signals go to the
   loader).
5. Exports **both** `UNPRIVILEGED_PATH` and `PRIVILEGED_PATH` to the same
   `~/homebrew` root (what `localplatformlinux.py` actually reads).
6. Sets **`UNPRIVILEGED_USER`** explicitly so ownership never falls back to
   `deck`.
7. Sets **`HOME`** and **`USER`** to the gaming account even though euid is root.
8. Ensures CEF marker + data dirs on every start (idempotent recovery).
9. Prefers the **systemctl shim** via `PATH`.

#### `/etc/runit/sv/plugin_loader/log/run`

```sh
#!/bin/sh
[ -d /var/log/plugin_loader ] || install -dm 755 /var/log/plugin_loader
exec svlogd -tt /var/log/plugin_loader
```

Logs land in `/var/log/plugin_loader/current` (svlogd).

### 4.8 Enable and start under runit

```sh
# enable at boot (default runlevel)
ln -s /etc/runit/sv/plugin_loader /etc/runit/runsvdir/default/plugin_loader

# live supervise link (Artix runit)
# /run/runit/service/plugin_loader -> /etc/runit/sv/plugin_loader

sv up plugin_loader
# or: sv restart plugin_loader
```

Reference host links:

```text
/etc/runit/runsvdir/default/plugin_loader -> /etc/runit/sv/plugin_loader
/run/runit/service/plugin_loader -> /etc/runit/sv/plugin_loader
```

---

## 5. Runtime requirements

### 5.1 Process model

Observed while healthy:

```text
root  runsv plugin_loader
root  svlogd -tt /var/log/plugin_loader
root  /home/<user>/homebrew/services/PluginLoader          # thin wrapper
root  Decky Loader vX.Y.Z (.../PluginLoader)               # main runtime
```

- Supervisor and loader run as **root**.
- CWD of the loader: `/home/<user>/homebrew/services`.
- Backend HTTP: **`http://127.0.0.1:1337`** (exclusive bind).
- Steam CEF / steamwebhelper separately listens on **`127.0.0.1:8080`** when
  remote debugging is active — Decky attaches to that world, it does not replace
  it.

Only **one** PluginLoader instance may own port 1337. A second start fails with:

```text
OSError: [Errno 98] error while attempting to bind on address ('127.0.0.1', 1337)
```

and runit will spin if the old process was not supervised or not killed.

### 5.2 Environment contract (must be present at exec)

| Variable | Required value | Role |
|---|---|---|
| `HOME` | `/home/<user>` | user context for paths |
| `USER` | `<user>` | user context |
| `UNPRIVILEGED_USER` | `<user>` | plugin/file ownership target (not `deck`) |
| `UNPRIVILEGED_PATH` | `/home/<user>/homebrew` | loader data root |
| `PRIVILEGED_PATH` | `/home/<user>/homebrew` | same root (upstream sets both equal) |
| `LOG_LEVEL` | `INFO` (or as desired) | loader logging |
| `PATH` | `…/homebrew/bin` **first**, then normal sbin/bin | find systemctl shim |

`HOMEBREW_FOLDER` is historically related but the live working service relies on
`UNPRIVILEGED_PATH` / `PRIVILEGED_PATH` (and the `~/homebrew` layout) rather than
depending on the AUR helper path.

### 5.3 Runtime files Decky writes

From the healthy host:

```json
// ~/homebrew/settings/loader.json (example)
{
    "branch": 0,
    "pluginOrder": [],
    "user_info.user_name": "superphenotype",
    "user_info.user_home": "/home/superphenotype",
    "store": 0
}
```

Plugins import from `~/homebrew/plugins`. Empty is fine for first boot; QAM still
shows the Decky UI once frontend injection succeeds.

### 5.4 Steam / session runtime needs

| Need | Detail |
|---|---|
| Game Mode Steam running | CEF target; desktop-only Steam is insufficient for QAM-in-Game-Mode verification |
| CEF flag present before/during Steam start | `.cef-enable-remote-debugging` |
| `~/.steam/steam` symlink intact | must not be a plain directory |
| User session for that home | same `$DECKY_USER` Decky was configured with |
| Localhost networking | bind/connect on `127.0.0.1` |

Frontend load is asynchronous. Logs show `Loading Decky frontend!` and may show
transient CEF disconnect / webhelper crash warnings during Steam restarts; the
loader stays up and retries.

### 5.5 What is *not* required at runtime

- systemd, `systemctl` (real), or the packaged `decky-loader@.service`
- `decky-loader-helper` on each start (the run script replaces its useful bits)
- Preinstalled plugins
- Network, for basic QAM injection (store/updates want network)

---

## 6. Operator commands

```sh
# status
sv status plugin_loader

# control
sv up plugin_loader
sv down plugin_loader
sv restart plugin_loader

# logs
tail -F /var/log/plugin_loader/current

# port
ss -tln | grep 127.0.0.1:1337

# processes
pgrep -a PluginLoader
```

After replacing `/etc/runit/sv/plugin_loader/run`, always `sv restart
plugin_loader` (or down/up). Confirm no duplicate listeners on 1337 before up.

---

## 7. Failure modes learned while bringing this up

### 7.1 Missing `systemctl`

**Symptom:** traceback `FileNotFoundError: … 'systemctl'` from
`localplatformlinux.py` (`service_active` / `service_stop`).

**Fix:** install the shim at `~/homebrew/bin/systemctl` and put that directory
first on `PATH` in the runit `run` script.

### 7.2 Port 1337 already in use

**Symptom:** loader exits immediately; runit respawn loop; log shows
`address already in use` on `127.0.0.1:1337`.

**Causes seen:** leftover PluginLoader from manual tests; overlapping restart
while an old instance still held the port; shim-less stop path failing to clear
the old process.

**Fix:** `sv down plugin_loader`, kill stray PluginLoader PIDs if any, verify
`ss` shows 1337 free, then `sv up`. Ensure only runit supervises the loader.

### 7.3 Wrong data root (`~/.local/var/opt/decky-loader`)

**Symptom:** service "runs" but nothing matches SteamOS plugin paths; confusion
with docs/plugins that assume `~/homebrew`.

**Fix:** never use AUR helper destination; always `~/homebrew` with both path
env vars pointing there.

### 7.4 Running the loader as the unprivileged user

**Symptom:** Decky unreachable in Game Mode / QAM never appears even if the
process lives.

**Fix:** run as **root**, set `UNPRIVILEGED_USER` to the gaming account. This
matches upstream release unit `User=root`.

### 7.5 CEF flag missing or Steam data path wrong

**Symptom:** backend on 1337, no frontend injection, no QAM entry.

**Fix:** empty file
`~/.local/share/Steam/.cef-enable-remote-debugging`, user-owned; ensure
`~/.steam/steam` → that Steam dir; restart Steam/Game Mode.

### 7.6 `UNPRIVILEGED_USER` unset

**Symptom:** Decky falls back to username `deck` for chown/path logic.

**Fix:** export `UNPRIVILEGED_USER=<real user>` in the service environment.

### 7.7 Transient CEF / webhelper flaps

**Symptom:** log lines like `CEF has disconnected`, `webhelper crashed within a
minute`, then `Loading Decky frontend!` again.

**Interpretation:** often Steam lifecycle noise; not necessarily a bad service
file. Confirm 1337 stays up and QAM returns after Steam stabilizes.

---

## 8. Checklist — install from zero on runit

1. Install Steam and confirm Game Mode works for `$DECKY_USER`.
2. Install `decky-loader-bin` (AUR).
3. Create `~/homebrew/{services,plugins,settings,bin}` owned by `$DECKY_USER`.
4. Copy `/usr/lib/decky-loader/PluginLoader` → `~/homebrew/services/PluginLoader`.
5. Write `~/homebrew/services/.loader.version`.
6. Install `~/homebrew/bin/systemctl` shim (mode 0755).
7. Ensure `~/.local/share/Steam` exists, `~/.steam/steam` symlink, CEF marker file.
8. Install `/etc/runit/sv/plugin_loader/run` and `log/run` (mode 0755).
9. Enable: symlink into `/etc/runit/runsvdir/default/`.
10. `sv up plugin_loader` → `sv status` shows run; logs show version + plugin_path.
11. Confirm `127.0.0.1:1337` listen.
12. Start/restart Game Mode Steam → logs show `Loading Decky frontend!`.
13. Confirm Decky in QAM.

---

## 9. Checklist — deploytix implementation target

When correcting deploytix, the installer must produce **this** runtime, not the
AUR systemd helper layout:

- [ ] Package install of `decky-loader-bin` (via yay/AUR path already required
      for other gaming extras).
- [ ] Bootstrap `~/homebrew` (not `~/.local/var/opt/decky-loader`).
- [ ] Bypass `decky-loader-helper` for path selection.
- [ ] Ship/install the **systemctl shim** under `~/homebrew/bin/systemctl`.
- [ ] Write the runit service exactly with: root exec, path env pair,
      `UNPRIVILEGED_USER`, `HOME`/`USER`, CEF ensure, `PATH` shim-first,
      `cd` + `exec` PluginLoader.
- [ ] Enable `plugin_loader` in the default runsvdir.
- [ ] CEF marker + `~/.steam/steam` symlink rules (symlink, never mkdir the
      steam path as a real directory).
- [ ] Do not drop privileges with `chpst -u`.
- [ ] Do not require real systemd units.
- [ ] Tolerate PluginLoader self-updates under `~/homebrew/services/`.

---

## 10. Reference snapshot (live host)

Captured while green on the reference machine:

| Item | Value |
|---|---|
| Init | runit |
| Service | `plugin_loader` |
| Status | `run: plugin_loader …; run: log …` |
| User (euid) | root |
| Configured user | `superphenotype` |
| Data root | `/home/superphenotype/homebrew` |
| Loader | `/home/superphenotype/homebrew/services/PluginLoader` |
| Version file | `v3.2.8` |
| Backend | `127.0.0.1:1337` |
| CEF flag | `/home/superphenotype/.local/share/Steam/.cef-enable-remote-debugging` |
| Steam symlink | `~/.steam/steam` → `~/.local/share/Steam` |
| Shim | `/home/superphenotype/homebrew/bin/systemctl` |
| Logs | `/var/log/plugin_loader/` |
| QAM | Decky UI present |

Staging copy used while iterating the service (for recovery/reinstall patterns):

```text
/tmp/plugin_loader-runit/run
/tmp/plugin_loader-runit/log/run
/tmp/plugin_loader-runit/README
```

---

## 11. Related docs

- `docs/SESSION_SWITCHING.md` — Game Mode / desktop session lifecycle (Steam
  must actually be in Game Mode for QAM verification).
- `docs/SESSION_SWITCHING_REWORK.md` — historical session bugs (context only).
- Upstream project: <https://github.com/SteamDeckHomebrew/decky-loader>
- AUR seed package name: `decky-loader-bin`
