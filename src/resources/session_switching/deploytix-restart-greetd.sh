#!/bin/sh

# deploytix-restart-greetd — restart the greetd display manager on any
# Artix init system (runit, OpenRC, s6, dinit).
#
# Called as root (detached via `sudo setsid`) from session-select and
# return-to-gamemode: restarting greetd tears down the current user
# session and relaunches deploytix-session-manager on a clean VT.
#
# Detection keys off the active init's runtime state directory rather
# than installed binaries, because supervision tools from several init
# systems can coexist on disk (e.g. s6 utilities on a runit install).

if [ -d /run/runit ]; then
    exec sv restart greetd
elif [ -d /run/openrc ]; then
    exec rc-service greetd restart
elif [ -d /run/s6-rc ]; then
    # Artix s6: the live scandir is /run/service and service directories
    # follow the {name}-srv convention (greetd-srv is written by
    # configure_greetd, since no official greetd-s6 package exists).
    # s6-svc -t sends SIGTERM; s6-supervise restarts the wanted-up
    # longrun automatically.
    if s6-svc -t /run/service/greetd-srv 2>/dev/null; then
        exit 0
    fi
    # Fallback if the scandir lives elsewhere: bounce via s6-rc.
    s6-rc -d change greetd-srv
    exec s6-rc -u change greetd-srv
elif [ -S /run/dinitctl ] || pidof dinit >/dev/null 2>&1; then
    exec dinitctl restart greetd
fi

# Runtime detection failed — fall back to whichever tool exists.
command -v sv         >/dev/null 2>&1 && exec sv restart greetd
command -v rc-service >/dev/null 2>&1 && exec rc-service greetd restart
command -v dinitctl   >/dev/null 2>&1 && exec dinitctl restart greetd

echo >&2 "deploytix-restart-greetd: could not determine init system"
exit 1
