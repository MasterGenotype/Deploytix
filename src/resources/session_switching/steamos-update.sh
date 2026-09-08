#!/bin/sh
# Stub for steamos-update — Steam (running with -steamdeck) invokes this to
# check for and apply SteamOS system updates. There is no SteamOS updater on
# Artix; exit 7 is Valve's "no update available" code, which lets the OOBE
# and Settings update checks complete instead of hanging or erroring.
exit 7
