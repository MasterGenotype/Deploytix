# Session Switching Rework

This document describes the overhaul of the Deploytix session switching subsystem,
covering the problems encountered when running Steam in gamescope on a greetd-managed
Artix Linux system and the solutions applied.

---

## Background

Deploytix deploys a handheld/console-style Steam session on Artix Linux using:

- **greetd** as the login manager (runs on VT1)
- **gamescope** as the Wayland compositor
- **Steam** in `-steamos3 -gamepadui` mode for the controller-driven UI
- **elogind** for seat/session management

The session manager (`deploytix-session-manager`) runs as greetd's `default_session`
greeter and is responsible for choosing between gamescope (game mode) and a desktop
environment, then launching the selected session.

## Architecture

```
greetd (PID 1-managed)
  |
  +--> deploytix-session-manager  (default_session, Class=greeter)
         |
         |  [greetd IPC: create_session + start_session]
         |
         +--> greetd creates new Class=user session
                |
                +--> steam-gamescope-session
                       |
                       +--> gamescope (Wayland compositor, backgrounded)
                       |     +--> Xwayland x2
                       |
                       +--> audio-startup (backgrounded, runs in parallel with
                       |    gamescope startup: pipewire, pipewire-pulse, wireplumber)
                       |
                       +--> [blocks on gamescope's ready fd]
                       |
                       +--> steam -steamos3 -gamepadui (foreground, blocks)
```

When Steam exits, `steam-gamescope-session` cleans up gamescope and exits, which
causes greetd to restart `deploytix-session-manager`, completing the cycle.

Session switching (game mode <-> desktop) is handled by writing a sentinel file
(`~/.config/deploytix-session`) via `session-select` / `steamos-session-select`
before killing the current session.

---

## Problems and Solutions

### 1. Black Screen -- elogind Seat Revocation (Class=greeter)

**Problem**: greetd's `default_session` runs with elogind `Class=greeter`. When the
greeter process directly launched gamescope (via `exec`), the session inherited
`Class=greeter` status. elogind revokes DRM/input device access for greeter sessions
once a user session is expected, causing gamescope to fail with a black screen and no
input.

**Root cause**: greetd creates elogind sessions via D-Bus directly, bypassing PAM
session modules. Attempting to override `XDG_SESSION_CLASS` via `pam_env.so` in
`/etc/pam.d/greetd` had no effect because (a) greetd uses PAM service
`greetd-greeter` for the default session, and (b) elogind session creation doesn't
consult PAM environment variables.

**Solution**: Rewrote `deploytix-session-manager` to use greetd's IPC protocol
instead of directly launching the session. A new helper script (`greetd-ipc`, Python)
communicates with greetd over `GREETD_SOCK` using the native-endian length-prefixed
JSON protocol:

1. `create_session` with the target username
2. Handle any auth challenges (auto-respond for passwordless login)
3. `start_session` with the target command

greetd then terminates the greeter and starts the requested command in a fresh
`Class=user` session with full seat access.

**Files changed**:
- `deploytix-session-manager.sh` -- rewritten from while-loop direct-launch to
  single-shot IPC-based greeter
- `greetd-ipc.py` -- new file, Python greetd IPC client
- `session_switching.rs` -- added `greetd-ipc.py` to deployment manifest

**PAM configuration required** (not yet in deployment automation):
- `/etc/pam.d/greetd` must use `pam_permit.so` for `auth` to allow passwordless
  IPC-created sessions (the socket is access-controlled, so this is safe)

### 2. Steam Not Displaying -- Missing Gamescope Base Layer

**Problem**: After gamescope started successfully and reported its `DISPLAY` and
`WAYLAND_DISPLAY`, Steam launched but nothing rendered. Gamescope showed an empty
compositor with no focused window.

**Root cause**: Gamescope's `--steam` mode expects the X root window property
`GAMESCOPECTRL_BASELAYER_APPID` to be set to tell it which application to focus as
the base compositing layer. Without it, gamescope has no window to present. Steam's
app ID in gamescope is `769`.

**Solution**: Added an `xprop` call to `steam-gamescope-session` immediately after
gamescope reports ready (after reading from the ready-fd socket), before launching
Steam:

```bash
xprop -root -f GAMESCOPECTRL_BASELAYER_APPID 32c \
    -set GAMESCOPECTRL_BASELAYER_APPID 769
```

**Files changed**:
- `steam-gamescope-session.sh` lines 110-111

### 3. Steam Not in Gamepad UI Mode

**Problem**: Steam launched and displayed, but showed the standard desktop Big Picture
interface instead of the SteamOS/Deck-style gamepad UI. Controller navigation was
limited.

**Root cause**: The `-steamos3` flag alone does not activate the full gamepad UI on
non-SteamOS systems. It enables SteamOS session management features (like
`steamos-session-select` integration) but the gamepad-native interface requires the
separate `-gamepadui` flag.

**Solution**: Changed the Steam launch command from `steam -steamos3` to
`steam -steamos3 -gamepadui`.

**Files changed**:
- `steam-gamescope-session.sh` line 115

### 4. Audio Not Starting -- Stale D-Bus Socket References

**Problem**: Audio devices were not available in the gamescope session. PipeWire log
showed repeated errors: `Failed to connect to socket /tmp/dbus-XXXXXXXX: No such
file or directory`.

**Root cause**: When greetd creates a new session via IPC, `steam-gamescope-session`
starts a fresh D-Bus session bus (`eval "$(dbus-launch --sh-syntax)"`). However,
PipeWire daemons from the *previous* session survived across the session boundary
with stale references to the old D-Bus socket (which no longer exists). The
`audio-startup` script used a `start_if_missing` pattern that checked `pgrep` -- since
the zombie daemons were technically still running, it skipped starting new ones.

**Solution** (two-layer fix):

1. **`audio-startup`**: Changed from skip-if-running to always kill-and-restart.
   Every session start now kills existing pipewire/pipewire-pulse/wireplumber
   processes, waits for them to die, then starts fresh instances that inherit the
   current session's D-Bus address.

2. **`deploytix-session-manager`**: Added pipewire, pipewire-pulse, and wireplumber
   to the `cleanup_stale_sessions()` function (both graceful SIGTERM and SIGKILL
   fallback passes). This provides defense-in-depth cleanup before the new session
   starts, in case audio-startup's own cleanup isn't sufficient.

**Files changed**:
- `deploytix-session-manager.sh` lines 32-34 (SIGTERM), lines 40-42 (SIGKILL)
- `audio-startup` is not in the Deploytix repo (lives at `~/.local/bin/audio-startup`
  on the target system)

### 5. First Boot Unusable -- No Way to Log In to Steam

**Problem**: On a fresh install booting straight into the gamescope session, Steam
had no cached credentials and no usable login UI. The user had to attach a keyboard,
switch to desktop mode, and sign in to Steam there before Game Mode worked.

**Root cause**: `steam -steamos3 -gamepadui` without cached credentials falls back
to the legacy desktop-style X11 login dialog instead of the Deck first-run
experience. Under gamescope's `--force-windows-fullscreen` with the base layer
pinned to app ID 769, that dialog is effectively invisible and unreachable without
a mouse.

**Solution** (three parts):

1. **Launch flags**: Steam is now launched as
   `steam -gamepadui -steamos3 -steampal -steamdeck` (the upstream
   ChimeraOS/Bazzite gamescope-session launch line). `-steampal -steamdeck`
   activate the Steam Deck OOBE on first run: language → network setup →
   controller-navigable login with on-screen keyboard and QR-code sign-in via the
   Steam mobile app.
2. **SteamOS tooling stubs**: with `-steamdeck`, Steam probes SteamOS update
   tooling. New stubs `/usr/bin/steamos-update` (exit 7 = "no update available")
   and `/usr/bin/jupiter-biosupdate` (exit 0) join the existing
   `steamos-select-branch` stub so the OOBE update checks complete instead of
   hanging.
3. **NetworkManager access**: the OOBE network page (and Settings > Internet)
   drives NetworkManager over D-Bus. A polkit rule
   (`/etc/polkit-1/rules.d/50-deploytix-networkmanager.rules`, templated on the
   autologin username) grants that user passwordless NetworkManager control.
   Validation now requires a NetworkManager backend when session switching is
   enabled; the wizard and GUI coerce the backend automatically.

**First-boot client bootstrap** (previously an unhandled dependency): Steam's
first-run client download happens *before* the OOBE (and its network page)
exists, so the device needs connectivity from the very first boot. For Wi-Fi-only
devices the deployment config accepts `network.wifi_ssid` /
`network.wifi_password`, which deploytix pre-seeds as a NetworkManager system
connection (or an iwd network file for non-gaming iwd installs) so the system
auto-connects immediately on boot.

Connectivity alone was not enough, because nothing ever *performed* that
download. `seed_steam_bootstrap` unpacks only `bootstraplinux_ubuntu12_32.tar.xz`
— steam.sh plus the 32-bit launcher. The gamepad UI itself is
`ubuntu12_64/steamwebhelper` (which renders it) and `steamui.so` (which
implements it), and both arrive only with the client Steam fetches on its first
real run. So `-gamepadui` had nothing to draw, Steam exited within seconds, the
short-session tracker collected its five strikes, and the first boot of every new
install landed on the desktop with the client still missing. The fix people found
by hand — `pkill steam`, then a plain `steam` in a terminal — is now the shipped
path:

- `steam-bootstrap-check` answers "is the client downloaded?", separately from
  `steam-login-check`'s "is an account remembered?". Both must hold before
  gamemode is worth entering.
- `steam-gamescope-session` runs a **plain windowed `steam`** inside the already
  running compositor when the client is missing, waits (bounded: 45 s for a
  default route, 15 min for the download), stops it, and only then launches
  `-gamepadui -steamos3 -steampal -steamdeck`. The download is not scored as a
  gamemode session, and a failed one routes to the desktop rather than looping.
- `steam-stop` reaps the real process tree (`steam.sh` → `ubuntu12_32/steam` →
  `steamwebhelper`, plus `reaper SteamLaunch`) and removes `~/.steam/steam.pipe`
  and `steam.pid`. Steam is single-instance and the wrapper hands new
  invocations off through that pipe, so a client that died badly makes every
  later `steam` exit silently — the reason a manual `pkill` was needed first.
  The greeter's teardown table mirrors its patterns but never invokes it (see
  the `cleanup_stale_sessions` warning about running `steam` from a seat-less
  greeter).
- `steam-first-login` handles both halves on the desktop fallback, and no longer
  returns to gamemode on a remembered account whose client was never downloaded.
- `short_session_recover` re-extracts the bootstrap tarball only when `steam.sh`
  is genuinely missing. Untarring it over a client that is merely mid-download
  was turning a slow first boot into a corrupted one.

**Optional: `packages.steam_prefetch_client`.** Downloads the client during
installation (`xvfb-run steam +quit`, run as the target user in the chroot, with
`xorg-server-xvfb` pulled in automatically) so the deployed system boots straight
into Game Mode. Costs a few hundred MB and several minutes of install time.
Entirely best-effort — no network, no Xvfb, or a timeout just logs, and the
first-boot path above still covers it.

**Files changed**:
- `steam-gamescope-session.sh` -- launch flags, client bootstrap phase
- `steam-bootstrap-check.sh`, `steam-stop.sh` -- new helpers
- `steam-first-login.sh` -- client-aware desktop fallback
- `packages.rs` -- optional install-time client prefetch
- `steamos-update.sh`, `jupiter-biosupdate.sh` -- new stubs
- `50-deploytix-networkmanager.rules` -- new polkit rule template
- `session_switching.rs` -- deploys the above
- `network.rs` -- Wi-Fi pre-seeding
- `deployment.rs` -- `wifi_ssid`/`wifi_password` config fields, validation, wizard

### 6. Slow Boot -- Fixed Sleeps on the Path to Game Mode

**Problem**: Roughly five seconds elapsed between greetd starting and gamescope
being spawned, all of it spent sleeping rather than doing work. On a handheld
that is five seconds of black screen on every single boot.

**Root cause**: Three unconditional delays, all serialised ahead of gamescope:

1. `deploytix-session-manager`'s `cleanup_stale_sessions()` ran a full
   SIGTERM -> `sleep 1` -> SIGKILL cycle (about 30 `pkill` spawns) before
   reaching the greetd IPC call -- even on a cold boot, where there is by
   definition no previous session to tear down.
2. `steam-gamescope-session` ran `audio-startup` **in the foreground**, before
   launching gamescope.
3. `audio-startup` itself was `sleep 2` (settle) + `sleep 1` + `sleep 1`
   between starting pipewire, pipewire-pulse and wireplumber.

**Solution** -- wait on state, not on the clock, and overlap what can overlap:

1. **`deploytix-session-manager`**: one `STALE_PROCS` table now drives detection,
   SIGTERM and SIGKILL, so the three passes cannot drift apart. `cleanup_stale_sessions()`
   returns immediately when `_stale_any` finds nothing (the cold-boot case), and
   otherwise polls at 50 ms for the graceful pass to land instead of sleeping a
   flat second. The SIGKILL pass runs only against processes that actually
   ignored SIGTERM.
2. **`steam-gamescope-session`**: gamescope is spawned first -- its startup (DRM
   master, Vulkan device init, two Xwayland servers) is the long pole -- and
   `audio-startup` is backgrounded so it runs *during* that startup rather than
   before it. Nothing between the top of the script and the ready-fd `read`
   blocks. Steam does not need PipeWire to exist before it launches; it opens
   audio devices lazily.
3. **`audio-startup`**: the only real ordering constraint is that pipewire's core
   socket exists before its clients connect, so `wait_for_socket` polls
   `$XDG_RUNTIME_DIR/pipewire-0` at 50 ms (5 s ceiling, then it proceeds anyway
   and logs). pipewire-pulse and wireplumber are independent clients of that one
   socket, so they start together instead of one-per-sleep.

Net effect: about 5 s of unconditional sleeping removed from the boot -> Game Mode
path, and gamescope now starts within milliseconds of the session script.

**Files changed**:
- `deploytix-session-manager.sh` -- `STALE_PROCS`, `_stale_any`, `_stale_kill`,
  rewritten `cleanup_stale_sessions()`
- `steam-gamescope-session.sh` -- sections 9/10 reordered (gamescope, then
  backgrounded audio)
- `audio-startup.sh` -- `wait_for_socket` replaces the fixed sleeps

Guarded by regression tests in `session_switching.rs` (spawn ordering, no fixed
sleeps before the ready-fd read, cold-boot cleanup fast path) and `packages.rs`
(socket wait, no whole-second sleeps).

---

### 7. "Return to Desktop" Stopped Working After a Reinstall

**Problem**: On a machine reinstalled in September 2026, selecting "Return to
Desktop" in Steam's power menu did nothing, and later hung without ever
reaching a desktop. The same feature had worked for about four months before
that on the same hardware.

**Root cause**: Not one bug. The whole switch path was rewritten over three
days at the end of August 2026 — `ea0be34`, `ce81896` and `dc7ef7b` — and
reinstalling put all of it on the machine at once. The version that had worked
was the one in the tree from 2026-05-04 to 2026-08-29.

Two things made this hard to see:

- `desktop-session` was installed the whole time, but not through
  `DEPLOY_FILES`. It has its own write in `setup_session_switching`, so
  searching the manifest for it finds nothing.
- The change was to the file's *contents*, not whether it was there.
  `ea0be34` replaced the static script with a generated template that added
  `XDG_CURRENT_DESKTOP`, `XDG_SESSION_DESKTOP` and `XDG_SESSION_TYPE` exports
  on the startup path, a fallback-candidate loop, and a `printf | while read`
  teardown whose SIGKILL pass also hit pipewire and wireplumber.

**Solution**: Revert `src/resources/session_switching/` to `ea0be34^`
(`0f9689c`, 2026-08-28) — the tree as it stood through the four months it
worked. The scripts are byte-identical to that commit apart from three added
things, all of which only add:

1. **An invocation log** in `session-select`, written before the `case` that
   can reject a name. Steam discards the exit code, so a rejected session name
   is otherwise invisible. The log is at
   `~/.local/state/deploytix-session-select.log`. Same treatment the
   `steamos-update` and `jupiter-biosupdate` stubs get.

2. **A console clear** at the end of `cleanup_stale_sessions`. The desktop's
   processes (plasma, kwin, pipewire, Xwayland) inherit stdio from greetd's
   session, which is VT1, so their messages land on screen when the greeter
   kills them. The greeter's own output already goes to a log; theirs cannot
   be redirected from outside, so it clears what they leave behind.

3. **A network wait** in `steam-first-login`. It runs from XDG autostart, which
   fires before NetworkManager has finished associating, so Steam's first act
   was a client update with no route. It now waits up to 45 seconds for a
   default route, then starts Steam either way.

**Later, separately**: `desktop-session` went back to being generated per
desktop environment (the `ea0be34` approach), because a KDE install and a GNOME
install genuinely need different launch commands and teardown lists. What
changed is that the generator is now the only thing that writes the file, and
two tests enforce it — one checks the installed file equals the rendering for
the configured desktop and is not the template, the other fails if a
destination ever appears in both the static and generated manifests.

**Files changed**:
- `src/resources/session_switching/` — reverted to `ea0be34^`, plus the three
  additions above
- `session_switching.rs` — `GeneratedFile` / `GENERATED_FILES` manifest beside
  `DEPLOY_FILES`, so the "is everything referenced also installed?" test reads
  both lists instead of a hand-maintained one

**What was wrong with the first attempt at this**: the original diagnosis said
`session-select` rejecting unknown session names was the cause, and normalising
them was the fix. It was not. The same name matching ran for the whole four
months it worked. The rejection is a real latent bug, but it is not this one,
and the normalisation was dropped.

---

### 8. "Switch to Desktop" Raced greetd's Restart Against Gamescope's Teardown

**Problem**: Even after problem 7's revert, and after fixing `steam-gamescope-session`
to wait on gamescope as well as Steam (so a killed compositor always ends the
session -- see the `wait -n` change below), "Switch to Desktop" from inside
Steam remained unreliable: intermittently a black screen with no gamescope,
no desktop and no greeter, matching problem 7's original symptom exactly.

**Root cause**: Two teardown mechanisms were layered on top of each other for
this one direction. `session-select` unconditionally restarted the greetd
*daemon* (`sudo setsid deploytix-restart-greetd &`, detached so it survives
the very teardown it causes), then, only for the desktop target, slept 4
seconds and fell back to killing gamescope directly if the session was
somehow still alive.

Restarting greetd this way does not wait for the *old* session to actually
finish exiting before the *new* greetd instance starts trying to spawn the
next one. That is invisible for desktop → gamescope (a desktop compositor
tears down fast enough that the race is essentially never lost, which is why
`return-to-gamemode` has used this mechanism alone, reliably, since the
original session-switching implementation). It is not invisible for
gamescope → desktop: gamescope holds the DRM master *and* two Xwayland
servers, so it can still be mid-teardown when the freshly restarted greetd
spawns the next greeter and that greeter tries to start a desktop compositor
with nowhere to acquire DRM. The 4-second fallback papered over the fast
path's failure often enough to look like a delay, but when the timing lined
up worse it produced exactly problem 7's symptom again, from a different
mechanism than the one problem 7 fixed.

**Solution**: Stop restarting the greetd daemon for the gamescope → desktop
direction entirely. `session-select` now kills gamescope directly and polls
(200 ms, up to 3 s) for it to actually die before escalating to `SIGKILL` --
no daemon restart, no sudo, on this path. This relies on the *same*,
never-restarted greetd noticing the session's process tree exited and
starting the greeter itself, which is the ordinary mechanism greetd already
provides (see `deploytix-session-manager`'s header comment: "greetd
terminates this greeter and starts the user session; when the user session
exits, greetd restarts this greeter. No while-loop needed."). It works
*because* `steam-gamescope-session` now waits on gamescope as well as Steam
(`wait -n "$steam_pid" "$gamescope_pid"`, added alongside the greetd-restart
attempt this section replaces): killing gamescope no longer depends on Steam
noticing and exiting on its own, which is the thing that made the original
direct-kill mechanism unreliable enough to move away from in the first place.

desktop → gamescope (`return-to-gamemode`, and `session-select`'s own
`gamescope` target) is untouched and still restarts greetd, since that
direction has no equivalent race to avoid.

**Files changed**:
- `session-select.sh` -- desktop target kills gamescope immediately and polls
  for it to die instead of sleeping 4s behind a greetd restart; gamescope
  target unchanged
- `session_switching.rs` -- test rewritten to assert the desktop branch never
  restarts greetd and the gamescope branch still does

---

## File Inventory

All session switching resources live in `src/resources/session_switching/` and are
compiled into the binary via `include_str!` in `src/configure/session_switching.rs`.

| File | Deployed to | Purpose |
|------|-------------|---------|
| `deploytix-session-manager.sh` | `/usr/bin/deploytix-session-manager` | greetd greeter; chooses session, launches via IPC |
| `greetd-ipc.py` | `/usr/bin/greetd-ipc` | Python greetd IPC client for creating Class=user sessions |
| `steam-gamescope-session.sh` | `/usr/local/bin/steam-gamescope-session` | Gamescope + Steam session launcher |
| `session-select.sh` | `/usr/bin/session-select` | Write the sentinel file and end the current session |
| `return-to-gamemode.sh` | `/usr/bin/return-to-gamemode` | Desktop shortcut to switch back to game mode |
| `steamos-select-branch.sh` | `/usr/bin/steamos-select-branch` | Stub for Steam compatibility |
| `steamos-update.sh` | `/usr/bin/steamos-update` | Stub: "no update available" (exit 7) for Steam's `-steamdeck` update checks |
| `jupiter-biosupdate.sh` | `/usr/bin/jupiter-biosupdate` | Stub: no-op BIOS update for Steam's `-steamdeck` mode |
| `50-deploytix-networkmanager.rules` | `/etc/polkit-1/rules.d/50-deploytix-networkmanager.rules` | Polkit rule (templated on username): passwordless NetworkManager control from Steam's UI |
| `gamescope-session.desktop` | `/usr/share/wayland-sessions/gamescope-session.desktop` | Wayland session .desktop entry |
| `deploytix-restart-greetd.sh` | `/usr/bin/deploytix-restart-greetd` | Init-agnostic greetd restart (runit `sv`, OpenRC `rc-service`, s6 `s6-svc`/`s6-rc`, dinit `dinitctl`) |
| `steam-login-check.sh` | `/usr/bin/steam-login-check` | Exit 0 when a remembered Steam login exists in loginusers.vdf |
| `steam-first-login.sh` | `/usr/bin/steam-first-login` | Desktop autostart helper: windowed Steam sign-in + auto return-to-gamemode |
| `deploytix-steam-first-login.desktop` | `/etc/xdg/autostart/deploytix-steam-first-login.desktop` | XDG autostart entry that runs steam-first-login in desktop sessions |
| `greetd.pam` | `/etc/pam.d/greetd` | PAM service for IPC-created Class=user sessions (passwordless auth, full session chain via system-local-login) |
| `greetd-greeter.pam` | `/etc/pam.d/greetd-greeter` | PAM service for greetd's default_session (the greeter itself); required so pam_start("greetd-greeter") does not fall through to `/etc/pam.d/other` (deny-all) |

Additionally, `session_switching.rs` creates a symlink:
`/usr/bin/steamos-session-select` -> `session-select`
(Steam calls `steamos-session-select` for "Switch to Desktop")

`/usr/local/bin/desktop-session` is not in the table above because it is not in
`DEPLOY_FILES`. It is generated per desktop environment and written by
`GENERATED_FILES` in the same function. See problem 7.

---

## First-Boot Steam Sign-In

On a fresh install there are no Steam credentials, so booting straight into
gamescope + `steam -steamos3 -gamepadui` lands on Steam's login screen, where
on-screen-keyboard/text input is not fully reliable pre-login. The sign-in
flow handles this with a gamescope-first, desktop-fallback design:

```
boot
 └─ greetd → deploytix-session-manager → gamescope + steam -gamepadui
      │
      ├─ user signs in via gamepad-UI login (QR code / OSK)
      │    └─ Steam continues into the gamepad UI — done, no restart needed
      │
      └─ Steam exits while still logged out (input failed / user quit)
           └─ steam-gamescope-session writes "desktop" sentinel
                └─ next session: desktop
                     └─ /etc/xdg/autostart runs steam-first-login
                          ├─ already signed in?  exit immediately
                          └─ notify + launch windowed Steam for sign-in
                               └─ poll loginusers.vdf; on login:
                                    notify, wait 15 s, return-to-gamemode
```

Key pieces:

- **`steam-login-check`** — shared predicate. Greps `loginusers.vdf`
  (both `~/.local/share/Steam` and `~/.steam/steam` locations) for
  `"RememberPassword" "1"` or `"AllowAutoLogin" "1"`. A login without
  "Remember me" intentionally does not count: it would not survive the
  session restart into gamemode.
- **`steam-gamescope-session`** — after Steam exits, if `steam-login-check`
  fails it writes `desktop` to the session sentinel so the session manager
  boots the desktop escape hatch instead of looping on the gamescope login.
- **`steam-first-login`** — runs from XDG autostart in every desktop
  session and exits immediately when already signed in, so it costs nothing
  in normal desktop use. When logged out it launches the regular windowed
  Steam client (real keyboard + QR available), polls for credentials, and
  automatically switches back to gamemode 15 seconds after sign-in (via
  `return-to-gamemode`, which requires the passwordless-sudo wheel rule the
  installer already configures). If the user quits Steam without signing
  in, the watcher stops and the desktop session continues normally.

---

## Init-Agnostic greetd Restart

Session switching works by bouncing greetd for the gamescope-bound
directions: `return-to-gamemode`, and `session-select`'s own `gamescope`
target. (The desktop-bound direction kills gamescope directly instead --
see problem 8 -- so it never calls this.) This was originally hardcoded as
`sv restart greetd`, which only worked on runit. `session-select` and
`return-to-gamemode` now invoke `/usr/bin/deploytix-restart-greetd` instead,
which detects the *running* init system from its runtime state directory
(installed binaries are not a reliable signal, since supervision tools from
several init systems can coexist on disk):

| Detection | Init | Restart command |
|-----------|------|-----------------|
| `/run/runit` exists | runit | `sv restart greetd` |
| `/run/openrc` exists | OpenRC | `rc-service greetd restart` |
| `/run/s6-rc` exists | s6 | `s6-svc -t /run/service/greetd-srv` (fallback: `s6-rc -d/-u change greetd-srv`) |
| `/run/dinitctl` socket or dinit running | dinit | `dinitctl restart greetd` |

If none of the runtime markers match, it falls back to trying whichever
tool is installed, in the same order.

---

## Remaining Work

- **audio-startup deployment**: The audio startup script currently lives outside the
  repo at `~/.local/bin/audio-startup`. Consider whether it should be managed by
  Deploytix or remain user-configured.
- **greetd config deployment**: `/etc/greetd/config.toml` pointing to
  `deploytix-session-manager` as the default session is not deployed by
  `session_switching.rs` (it's handled elsewhere in the installation pipeline via
  `configure::greetd::configure_greetd`).
