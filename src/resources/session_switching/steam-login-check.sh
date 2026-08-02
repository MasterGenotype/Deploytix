#!/bin/sh

# steam-login-check — exit 0 if a Steam account with remembered
# credentials exists, i.e. gamemode (gamescope + steam -gamepadui)
# can reach the gamepad UI without prompting for a password.
#
# Steam records signed-in accounts in loginusers.vdf. An account whose
# "Remember me" box was ticked has "RememberPassword" "1"; SteamOS-style
# auto-login additionally sets "AllowAutoLogin" "1". Either is enough
# for an unattended sign-in.
#
# Used by:
#   - steam-gamescope-session  (route to desktop when login fails)
#   - steam-first-login        (desktop autostart sign-in helper)

for vdf in \
    "$HOME/.local/share/Steam/config/loginusers.vdf" \
    "$HOME/.steam/steam/config/loginusers.vdf"
do
    [ -r "$vdf" ] || continue
    if grep -Eq '"(RememberPassword|AllowAutoLogin)"[[:space:]]+"1"' "$vdf"; then
        exit 0
    fi
done
exit 1
