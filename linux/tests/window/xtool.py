#!/usr/bin/env python3
"""Look at X11 windows without a window manager (ctypes + libX11 only).

  xtool.py list            one JSON line per top-level window
  xtool.py close TITLE     ask the window titled TITLE to close (WM_DELETE_WINDOW),
                           the way a window manager's close button does
"""
import ctypes
import ctypes.util
import json
import sys

X = ctypes.cdll.LoadLibrary(ctypes.util.find_library("X11") or "libX11.so.6")
Window = ctypes.c_ulong
Atom = ctypes.c_ulong
X.XOpenDisplay.restype = ctypes.c_void_p
X.XOpenDisplay.argtypes = [ctypes.c_char_p]
X.XDefaultRootWindow.restype = Window
X.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
X.XQueryTree.argtypes = [ctypes.c_void_p, Window, ctypes.POINTER(Window), ctypes.POINTER(Window), ctypes.POINTER(ctypes.POINTER(Window)), ctypes.POINTER(ctypes.c_uint)]
X.XInternAtom.restype = Atom
X.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
X.XGetWindowProperty.argtypes = [ctypes.c_void_p, Window, Atom, ctypes.c_long, ctypes.c_long, ctypes.c_int, Atom,
                                 ctypes.POINTER(Atom), ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_ulong),
                                 ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_void_p)]
X.XFree.argtypes = [ctypes.c_void_p]


class Attrs(ctypes.Structure):
    _fields_ = [("x", ctypes.c_int), ("y", ctypes.c_int), ("width", ctypes.c_int), ("height", ctypes.c_int),
                ("border_width", ctypes.c_int), ("depth", ctypes.c_int), ("visual", ctypes.c_void_p), ("root", Window),
                ("class_", ctypes.c_int), ("bit_gravity", ctypes.c_int), ("win_gravity", ctypes.c_int),
                ("backing_store", ctypes.c_int), ("backing_planes", ctypes.c_ulong), ("backing_pixel", ctypes.c_ulong),
                ("save_under", ctypes.c_int), ("colormap", ctypes.c_ulong), ("map_installed", ctypes.c_int),
                ("map_state", ctypes.c_int), ("all_event_masks", ctypes.c_long), ("your_event_mask", ctypes.c_long),
                ("do_not_propagate_mask", ctypes.c_long), ("override_redirect", ctypes.c_int), ("screen", ctypes.c_void_p)]


X.XGetWindowAttributes.argtypes = [ctypes.c_void_p, Window, ctypes.POINTER(Attrs)]


class ClientMessage(ctypes.Structure):
    _fields_ = [("type", ctypes.c_int), ("serial", ctypes.c_ulong), ("send_event", ctypes.c_int), ("display", ctypes.c_void_p),
                ("window", Window), ("message_type", Atom), ("format", ctypes.c_int), ("l", ctypes.c_long * 5)]


class XEvent(ctypes.Union):
    _fields_ = [("xclient", ClientMessage), ("pad", ctypes.c_long * 24)]


X.XSendEvent.argtypes = [ctypes.c_void_p, Window, ctypes.c_int, ctypes.c_long, ctypes.POINTER(XEvent)]
X.XFlush.argtypes = [ctypes.c_void_p]

d = X.XOpenDisplay(None)
if not d:
    sys.exit("cannot open the display")


def prop(w, name, typ="UTF8_STRING"):
    a = X.XInternAtom(d, name.encode(), 0)
    t = X.XInternAtom(d, typ.encode(), 0)
    at, af, n, after, data = Atom(), ctypes.c_int(), ctypes.c_ulong(), ctypes.c_ulong(), ctypes.c_void_p()
    if X.XGetWindowProperty(d, w, a, 0, 4096, 0, t, ctypes.byref(at), ctypes.byref(af), ctypes.byref(n), ctypes.byref(after), ctypes.byref(data)) != 0 or not data.value:
        return None
    # format 32 comes back as an array of C longs
    size = n.value * (ctypes.sizeof(ctypes.c_long) if af.value == 32 else af.value // 8)
    raw = ctypes.string_at(data.value, size)
    X.XFree(data)
    return raw


def windows():
    root = X.XDefaultRootWindow(d)
    r, p, kids, n = Window(), Window(), ctypes.POINTER(Window)(), ctypes.c_uint()
    X.XQueryTree(d, root, ctypes.byref(r), ctypes.byref(p), ctypes.byref(kids), ctypes.byref(n))
    out = []
    for i in range(n.value):
        w = kids[i]
        title = prop(w, "_NET_WM_NAME") or prop(w, "WM_NAME", "STRING")
        if not title:
            continue
        cls = (prop(w, "WM_CLASS", "STRING") or b"").split(b"\0")
        a = Attrs()
        X.XGetWindowAttributes(d, w, ctypes.byref(a))
        out.append(dict(id=w, title=title.decode(errors="replace"), wm_class=[c.decode() for c in cls if c],
                        mapped=a.map_state == 2, width=a.width, height=a.height,
                        pid=int.from_bytes(prop(w, "_NET_WM_PID", "CARDINAL") or b"\0\0\0\0", "little")))
    return out


if sys.argv[1] == "list":
    for w in windows():
        print(json.dumps(w))
elif sys.argv[1] == "close":
    hits = [w for w in windows() if w["title"] == sys.argv[2] and w["mapped"]]
    if not hits:
        sys.exit("no such window")
    ev = XEvent()
    ev.xclient.type = 33  # ClientMessage
    ev.xclient.window = hits[0]["id"]
    ev.xclient.message_type = X.XInternAtom(d, b"WM_PROTOCOLS", 0)
    ev.xclient.format = 32
    ev.xclient.l[0] = X.XInternAtom(d, b"WM_DELETE_WINDOW", 0)
    ev.xclient.l[1] = 0  # CurrentTime
    X.XSendEvent(d, hits[0]["id"], 0, 0, ctypes.byref(ev))
    X.XFlush(d)
