# Tray icons from apps running as root

StatusNotifierItem (SNI) tray icons are registered on the **session bus**, not
the system bus. `sudo` resets the environment, so a root app that uses
`libappindicator` / `libayatana-appindicator` (for example Flutter's
`tray_manager`, which Hiddify uses) cannot reach the session bus and silently
ends up with **no tray icon at all**. There is no D-Bus object for the panel to
find in that case.

Scanning the system bus does not help: those libraries only ever call
`g_bus_get(G_BUS_TYPE_SESSION)`, and a root process has no session bus socket by
default. Creating `/run/user/0/bus` alone is not enough either — GLib falls back
to `$HOME/.cache` for `XDG_RUNTIME_DIR`, so the app still fails to connect.

There is a second hurdle on systems using **dbus-broker** (the systemd default
on Arch): its connect policy is deny-all except for the UID that owns the bus.
A root process cannot connect to your user session bus even if the environment
variables are correct. `assets/50-vibepanel-root-tray.conf` relaxes that.

## Fix

Two pieces are needed: a D-Bus policy, and a launch wrapper.

```sh
# in the repo root
sudo install -m644 assets/50-vibepanel-root-tray.conf /etc/dbus-1/session.d/
sudo install -m755 scripts/run-as-root-with-tray.sh /usr/local/bin/run-as-root-with-tray

# apply the policy: log out and back in (preferred). Restarting the user bus
# drops all existing connections, so apps would need restarting too:
# systemctl --user restart dbus-broker.service

# then, from your desktop session:
run-as-root-with-tray hiddify
```

The wrapper preserves `DBUS_SESSION_BUS_ADDRESS`, `XDG_RUNTIME_DIR` and the
display variables (`WAYLAND_DISPLAY` is often `wayland-1`, so it must be passed
explicitly). The D-Bus policy lets the root process connect and send messages
on that bus. The app then registers its SNI item with vibepanel's watcher,
which picks it up like any other tray icon.

The policy grants root only `send` access on the session bus — it cannot own
names there, which keeps the extra privilege minimal.

## Hiddify desktop entry

`assets/hiddify-root.desktop` is a ready-to-use launcher. Install it after the
wrapper above:

```sh
cp assets/hiddify-root.desktop ~/.local/share/applications/
update-desktop-database ~/.local/share/applications 2>/dev/null || true
```

It replaces `Exec=hiddify` with `Exec=run-as-root-with-tray hiddify`. The
original Hiddify entry keeps working for non-root launches.

## General note

The wrapper is generic — it works for any GUI app that needs its tray icon while
running as root:

```sh
run-as-root-with-tray <command> [args...]
```

It must be invoked from inside the graphical session; it refuses to run from a
TTY, cron, or other context with no `DBUS_SESSION_BUS_ADDRESS`.
