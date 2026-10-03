"""Runner-local StatusNotifier host; dispatch the application's exported menu."""
import json
import pathlib
import sys

import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

WATCHER = "org.kde.StatusNotifierWatcher"
PROPERTIES = "org.freedesktop.DBus.Properties"
DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()


class Watcher(dbus.service.Object):
    def __init__(self):
        self.items = []
        self.name = dbus.service.BusName(WATCHER, bus, do_not_queue=True)
        super().__init__(self.name, "/StatusNotifierWatcher")

    @dbus.service.method(WATCHER, in_signature="s", sender_keyword="sender")
    def RegisterStatusNotifierItem(self, service, sender=None):
        item = str(sender) + str(service) if service.startswith("/") else str(service) + "/StatusNotifierItem"
        if item not in self.items:
            self.items.append(item)
            self.StatusNotifierItemRegistered(item)

    @dbus.service.method(WATCHER, in_signature="s")
    def RegisterStatusNotifierHost(self, service):
        self.StatusNotifierHostRegistered()

    @dbus.service.signal(WATCHER, signature="s")
    def StatusNotifierItemRegistered(self, item):
        pass

    @dbus.service.signal(WATCHER)
    def StatusNotifierHostRegistered(self):
        pass

    @dbus.service.method(PROPERTIES, in_signature="s", out_signature="a{sv}")
    def GetAll(self, interface):
        if interface != WATCHER:
            raise ValueError("Unexpected interface")
        return {"RegisteredStatusNotifierItems": dbus.Array(self.items, signature="s"),
                "IsStatusNotifierHostRegistered": dbus.Boolean(True), "ProtocolVersion": dbus.Int32(0)}

    @dbus.service.method(PROPERTIES, in_signature="ss", out_signature="v")
    def Get(self, interface, name):
        return self.GetAll(interface)[name]


def activate_menu(pid, label):
    watcher = bus.get_object(WATCHER, "/StatusNotifierWatcher")
    items = dbus.Interface(watcher, PROPERTIES).Get(WATCHER, "RegisteredStatusNotifierItems")
    daemon = dbus.Interface(bus.get_object("org.freedesktop.DBus", "/org/freedesktop/DBus"), "org.freedesktop.DBus")
    owned = []
    for item in items:
        service, path = str(item).split("/", 1)
        if int(daemon.GetConnectionUnixProcessID(service)) == pid:
            owned.append((service, "/" + path))
    if len(owned) != 1:
        raise RuntimeError("Exactly one application-owned tray item required")
    service, path = owned[0]
    properties = dbus.Interface(bus.get_object(service, path), PROPERTIES)
    menu_path = properties.Get("org.kde.StatusNotifierItem", "Menu")
    menu = dbus.Interface(bus.get_object(service, str(menu_path)), "com.canonical.dbusmenu")
    menu.AboutToShow(dbus.Int32(0))
    _, layout = menu.GetLayout(dbus.Int32(0), dbus.Int32(-1), dbus.Array([], signature="s"))
    matches = []

    def visit(node):
        identifier, props, children = node
        if str(props.get("label", "")).replace("_", "") == label:
            if not props.get("enabled", True) or not props.get("visible", True):
                raise RuntimeError("Requested menu item is disabled or hidden")
            matches.append(int(identifier))
        for child in children:
            visit(child)

    visit(layout)
    if len(matches) != 1:
        raise RuntimeError("Exactly one matching exported menu item required")
    menu.Event(dbus.Int32(matches[0]), "clicked", dbus.Int32(0), dbus.UInt32(0))
    print(json.dumps({"ownerPid": pid, "label": label, "clicked": True}))


if sys.argv[1] == "host":
    watcher = Watcher()
    host_name = dbus.service.BusName("org.kde.StatusNotifierHost.mesh_talk_diagnostics", bus, do_not_queue=True)
    pathlib.Path(sys.argv[2]).write_text("ready", encoding="utf8")
    GLib.MainLoop().run()
elif sys.argv[1] == "show":
    activate_menu(int(sys.argv[2]), "Show")
else:
    raise ValueError("Expected host or show")
