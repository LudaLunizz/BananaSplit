#!/usr/bin/env python3
"""A system tray for tests, written from the StatusNotifierItem and
dbusmenu specifications: it owns org.kde.StatusNotifierWatcher (and
plays the host that shows the icons), and when an item registers it reads
its properties and menu the way KDE's or GNOME's tray does, logging what it
sees. It also clicks: write commands into the command file.

  fake_tray.py LOG CMDFILE

Commands (one per line, the file is emptied after reading):
  click ID     the menu item ID was clicked (dbusmenu Event "clicked")
  activate     left click on the icon (Activate)
  layout       log the menu again
  props        log the item's properties again
Needs python3 with dbus-python and PyGObject (GLib).
"""
import os
import sys

import dbus
import dbus.mainloop.glib
import dbus.service
from gi.repository import GLib

LOG, CMD = sys.argv[1], sys.argv[2]
WATCHER = "org.kde.StatusNotifierWatcher"


def log(*a):
    with open(LOG, "a") as f:
        f.write(" ".join(str(x) for x in a) + "\n")


class Watcher(dbus.service.Object):
    def __init__(self, bus):
        super().__init__(bus, "/StatusNotifierWatcher")
        self.bus = bus
        self.items = []

    @dbus.service.method(WATCHER, in_signature="s", out_signature="", sender_keyword="sender")
    def RegisterStatusNotifierItem(self, service, sender=None):
        # the spec allows a bus name (object at /StatusNotifierItem) or a path
        name, path = (service, "/StatusNotifierItem") if not service.startswith("/") else (sender, service)
        self.items.append((name, path))
        log("registered", service, "from", sender)
        self.StatusNotifierItemRegistered(service)
        for sig in ("NewIcon", "NewToolTip", "NewTitle", "NewStatus"):
            self.bus.add_signal_receiver(lambda *a, s=sig: log("signal", s), signal_name=sig,
                                         dbus_interface="org.kde.StatusNotifierItem", bus_name=name, path=path)
        GLib.idle_add(self.inspect)

    @dbus.service.method(WATCHER, in_signature="s", out_signature="")
    def RegisterStatusNotifierHost(self, service):
        pass

    @dbus.service.signal(WATCHER, signature="s")
    def StatusNotifierItemRegistered(self, service):
        pass

    @dbus.service.method(dbus.PROPERTIES_IFACE, in_signature="ss", out_signature="v")
    def Get(self, iface, prop):
        return self.GetAll(iface)[prop]

    @dbus.service.method(dbus.PROPERTIES_IFACE, in_signature="s", out_signature="a{sv}")
    def GetAll(self, iface):
        return {"RegisteredStatusNotifierItems": dbus.Array([n for n, _ in self.items], signature="s"),
                "IsStatusNotifierHostRegistered": dbus.Boolean(True), "ProtocolVersion": dbus.Int32(0)}

    def item(self):
        name, path = self.items[-1]
        return self.bus.get_object(name, path)

    def inspect(self):
        p = self.item().GetAll("org.kde.StatusNotifierItem", dbus_interface=dbus.PROPERTIES_IFACE)
        pix = ["%dx%d:%d" % (w, h, len(b)) for (w, h, b) in p["IconPixmap"]]
        log("props", "Id=" + p["Id"], "Title=" + p["Title"], "Category=" + p["Category"], "Status=" + p["Status"],
            "Menu=" + p["Menu"], "ItemIsMenu=%s" % bool(p["ItemIsMenu"]), "IconName=" + repr(str(p["IconName"])),
            "IconPixmap=" + ",".join(pix), "ToolTip=" + p["ToolTip"][2] + "/" + p["ToolTip"][3])
        self.menu_path = p["Menu"]
        name = self.items[-1][0]
        self.bus.add_signal_receiver(lambda rev, parent: (log("signal LayoutUpdated", rev), self.layout()),
                                     signal_name="LayoutUpdated", dbus_interface="com.canonical.dbusmenu",
                                     bus_name=name, path=self.menu_path)
        self.layout()
        return False

    def menu(self):
        return self.bus.get_object(self.items[-1][0], self.menu_path)

    def layout(self):
        rev, (rid, props, kids) = self.menu().GetLayout(0, -1, dbus.Array([], signature="s"),
                                                         dbus_interface="com.canonical.dbusmenu")
        labels = []
        for k in kids:
            kid, kp, _ = k
            labels.append("%d:%s" % (kid, "-" if kp.get("type") == "separator" else kp.get("label")))
        ver = self.menu().Get("com.canonical.dbusmenu", "Version", dbus_interface=dbus.PROPERTIES_IFACE)
        log("layout", "rev=%d" % rev, "root=%d" % rid, "version=%d" % ver, " | ".join(labels))
        # the per-item calls hosts make before showing the menu
        g = self.menu().GetGroupProperties(dbus.Array([1, 5], signature="i"), dbus.Array([], signature="s"),
                                           dbus_interface="com.canonical.dbusmenu")
        log("group", ",".join("%d:%s" % (i, p.get("label")) for i, p in g))
        self.menu().AboutToShow(0, dbus_interface="com.canonical.dbusmenu")

    def command(self, line):
        w = line.split()
        if not w or not self.items:
            return
        if w[0] == "click":
            self.menu().Event(int(w[1]), "clicked", dbus.Int32(0, variant_level=1), dbus.UInt32(0),
                              dbus_interface="com.canonical.dbusmenu")
            log("clicked", w[1])
        elif w[0] == "activate":
            self.item().Activate(0, 0, dbus_interface="org.kde.StatusNotifierItem")
            log("activated")
        elif w[0] == "layout":
            self.layout()
        elif w[0] == "props":
            self.inspect()


def main():
    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    w = Watcher(bus)
    name = dbus.service.BusName(WATCHER, bus)  # keep it: dropping it releases the name
    log("watcher up")

    def poll():
        try:
            with open(CMD) as f:
                lines = f.read().splitlines()
            if lines:
                open(CMD, "w").close()
            for l in lines:
                try:
                    w.command(l)
                except Exception as e:  # keep going; the test sees the log
                    log("error", l, e)
        except FileNotFoundError:
            pass
        return True

    GLib.timeout_add(100, poll)
    GLib.MainLoop().run()
    del name


main()
