#!/usr/bin/env sh
set -eu

# Run a GUI command as root without losing its system tray icon.
#
# StatusNotifierItem (SNI) icons live on the *session* bus. sudo's env_reset
# strips DBUS_SESSION_BUS_ADDRESS/XDG_RUNTIME_DIR, so root apps using
# libappindicator/ayatana cannot reach the bus and silently get no tray icon.
# Passing the graphical session variables through fixes that.

if [ "$#" -eq 0 ]; then
    printf '%s\n' "usage: ${0##*/} COMMAND [ARGS...]" >&2
    exit 2
fi

if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ] || [ -z "${XDG_RUNTIME_DIR:-}" ]; then
    printf '%s\n' \
        "error: no graphical session found (DBUS_SESSION_BUS_ADDRESS / XDG_RUNTIME_DIR unset)" \
        "       launch this from your desktop session, not from a TTY or cron" >&2
    exit 1
fi

# WAYLAND_DISPLAY is frequently wayland-1 rather than wayland-0, and Qt/GTK also
# need the session type and desktop name for correct theming and portals.
KEEP_VARS=DBUS_SESSION_BUS_ADDRESS,XDG_RUNTIME_DIR,WAYLAND_DISPLAY,DISPLAY,XAUTHORITY,XDG_SESSION_TYPE,XDG_CURRENT_DESKTOP

exec sudo --preserve-env="$KEEP_VARS" "$@"
