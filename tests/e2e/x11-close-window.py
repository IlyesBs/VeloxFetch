#!/usr/bin/env python3
"""Close a program's windows the way a user would on Linux (X11), for the end-to-end tests.

    python3 x11-close-window.py <pid>

Sends WM_DELETE_WINDOW (what a window manager's close button sends) to every top-level window whose
_NET_WM_PID is <pid>. Uses only libX11 through ctypes: no extra packages. Exit code 0 = at least one
window was asked to close, 1 = no window of that program was found, 2 = no X display.
"""

import ctypes
import ctypes.util
import sys

x11 = ctypes.cdll.LoadLibrary(ctypes.util.find_library("X11") or "libX11.so.6")

Window = Atom = ctypes.c_ulong
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
x11.XDefaultRootWindow.restype = Window
x11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
x11.XInternAtom.restype = Atom
x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
x11.XQueryTree.argtypes = [ctypes.c_void_p, Window, ctypes.POINTER(Window), ctypes.POINTER(Window), ctypes.POINTER(ctypes.POINTER(Window)), ctypes.POINTER(ctypes.c_uint)]
x11.XGetWindowProperty.argtypes = [
    ctypes.c_void_p, Window, Atom, ctypes.c_long, ctypes.c_long, ctypes.c_int, Atom,
    ctypes.POINTER(Atom), ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_ulong),
    ctypes.POINTER(ctypes.c_void_p),
]
x11.XFree.argtypes = [ctypes.c_void_p]
x11.XSendEvent.argtypes = [ctypes.c_void_p, Window, ctypes.c_int, ctypes.c_long, ctypes.c_void_p]
x11.XFlush.argtypes = [ctypes.c_void_p]
x11.XCloseDisplay.argtypes = [ctypes.c_void_p]

ClientMessage = 33


class XClientMessageEvent(ctypes.Structure):
    _fields_ = [
        ("type", ctypes.c_int),
        ("serial", ctypes.c_ulong),
        ("send_event", ctypes.c_int),
        ("display", ctypes.c_void_p),
        ("window", Window),
        ("message_type", Atom),
        ("format", ctypes.c_int),
        ("data", ctypes.c_long * 5),
    ]


class XEvent(ctypes.Union):
    _fields_ = [("xclient", XClientMessageEvent), ("pad", ctypes.c_long * 24)]


def cardinals(dpy, win, prop):
    """The 32-bit values of a window property (empty when it is not set)."""
    actual, fmt, n, after, data = Atom(), ctypes.c_int(), ctypes.c_ulong(), ctypes.c_ulong(), ctypes.c_void_p()
    if x11.XGetWindowProperty(dpy, win, prop, 0, 64, 0, 0, ctypes.byref(actual), ctypes.byref(fmt), ctypes.byref(n), ctypes.byref(after), ctypes.byref(data)) != 0 or not data.value:
        return []
    try:
        if fmt.value != 32:
            return []
        return list(ctypes.cast(data, ctypes.POINTER(ctypes.c_ulong))[: n.value])  # format 32 is stored as longs
    finally:
        x11.XFree(data)


def children(dpy, win):
    root, parent, kids, n = Window(), Window(), ctypes.POINTER(Window)(), ctypes.c_uint()
    if not x11.XQueryTree(dpy, win, ctypes.byref(root), ctypes.byref(parent), ctypes.byref(kids), ctypes.byref(n)):
        return []
    try:
        return [kids[i] for i in range(n.value)]
    finally:
        if kids:
            x11.XFree(kids)


def main():
    pid = int(sys.argv[1])
    dpy = x11.XOpenDisplay(None)
    if not dpy:
        return 2
    net_wm_pid = x11.XInternAtom(dpy, b"_NET_WM_PID", 0)
    protocols = x11.XInternAtom(dpy, b"WM_PROTOCOLS", 0)
    delete = x11.XInternAtom(dpy, b"WM_DELETE_WINDOW", 0)
    asked = 0
    # The whole tree: a window manager re-parents client windows into frames, KWin two levels deep.
    todo, seen = children(dpy, x11.XDefaultRootWindow(dpy)), 0
    while todo and seen < 20000:
        win = todo.pop()
        seen += 1
        todo.extend(children(dpy, win))
        if cardinals(dpy, win, net_wm_pid)[:1] != [pid] or delete not in cardinals(dpy, win, protocols):
            continue
        ev = XEvent()
        ev.xclient.type = ClientMessage
        ev.xclient.window = win
        ev.xclient.message_type = protocols
        ev.xclient.format = 32
        ev.xclient.data[0] = delete
        ev.xclient.data[1] = 0  # CurrentTime
        x11.XSendEvent(dpy, win, 0, 0, ctypes.byref(ev))
        asked += 1
    x11.XFlush(dpy)
    x11.XCloseDisplay(dpy)
    return 0 if asked else 1


if __name__ == "__main__":
    sys.exit(main())
