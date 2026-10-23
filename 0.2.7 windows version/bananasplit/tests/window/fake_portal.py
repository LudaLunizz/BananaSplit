#!/usr/bin/env python3
"""The part of xdg-desktop-portal that BananaSplit's window reads, written
from the portal's Settings interface: org.freedesktop.appearance
color-scheme (0 no preference, 1 prefer dark, 2 prefer light). Read answers
the way the portal always has (the value boxed twice); the scheme changes
with SettingChanged when a number is written into the command file.

  fake_portal.py LOG CMDFILE INITIAL_SCHEME
"""
import sys

import dbus
import dbus.mainloop.glib
import dbus.service
from gi.repository import GLib

LOG, CMD, SCHEME = sys.argv[1], sys.argv[2], int(sys.argv[3])
IFACE = "org.freedesktop.portal.Settings"


def log(*a):
    with open(LOG, "a") as f:
        f.write(" ".join(str(x) for x in a) + "\n")


class Settings(dbus.service.Object):
    def __init__(self, bus):
        super().__init__(bus, "/org/freedesktop/portal/desktop")
        self.scheme = SCHEME

    @dbus.service.method(IFACE, in_signature="ss", out_signature="v")
    def Read(self, ns, key):
        log("read", ns, key)
        if (ns, key) != ("org.freedesktop.appearance", "color-scheme"):
            raise dbus.exceptions.DBusException("not found", name="org.freedesktop.portal.Error.NotFound")
        return dbus.UInt32(self.scheme, variant_level=2)

    @dbus.service.signal(IFACE, signature="ssv")
    def SettingChanged(self, ns, key, value):
        pass

    def set(self, s):
        self.scheme = s
        self.SettingChanged("org.freedesktop.appearance", "color-scheme", dbus.UInt32(s, variant_level=1))
        log("changed", s)


def main():
    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    s = Settings(bus)
    name = dbus.service.BusName("org.freedesktop.portal.Desktop", bus)  # keep it
    log("portal up")

    def poll():
        try:
            with open(CMD) as f:
                lines = f.read().split()
            if lines:
                open(CMD, "w").close()
            for l in lines:
                s.set(int(l))
        except FileNotFoundError:
            pass
        return True

    GLib.timeout_add(50, poll)
    GLib.MainLoop().run()
    del name


main()
