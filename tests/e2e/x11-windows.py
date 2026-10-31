#!/usr/bin/env python3
"""The state of a program's top-level windows on Linux (X11), for the end-to-end tests.

    python3 x11-windows.py <pid>

Prints a JSON list, one object per window whose _NET_WM_PID is <pid>: its id, size, whether it is mapped (on
screen), minimized (_NET_WM_STATE_HIDDEN) and the active window. Uses only libX11 through ctypes: no extra packages.
Exit code 2 = no X display.
"""
import ctypes, ctypes.util, json, sys

x = ctypes.cdll.LoadLibrary(ctypes.util.find_library("X11") or "libX11.so.6")
W = ctypes.c_ulong
x.XOpenDisplay.restype = ctypes.c_void_p
x.XOpenDisplay.argtypes = [ctypes.c_char_p]
x.XDefaultRootWindow.restype = W
x.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
x.XInternAtom.restype = W
x.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
x.XQueryTree.argtypes = [ctypes.c_void_p, W, ctypes.POINTER(W), ctypes.POINTER(W), ctypes.POINTER(ctypes.POINTER(W)), ctypes.POINTER(ctypes.c_uint)]
x.XGetWindowProperty.argtypes = [ctypes.c_void_p, W, W, ctypes.c_long, ctypes.c_long, ctypes.c_int, W,
    ctypes.POINTER(W), ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_void_p)]

class Attr(ctypes.Structure):
    _fields_ = [("x", ctypes.c_int), ("y", ctypes.c_int), ("w", ctypes.c_int), ("h", ctypes.c_int), ("bw", ctypes.c_int),
                ("depth", ctypes.c_int), ("visual", ctypes.c_void_p), ("root", W), ("cls", ctypes.c_int), ("bg", ctypes.c_int),
                ("bs", ctypes.c_int), ("bp", ctypes.c_ulong), ("bpx", ctypes.c_ulong), ("su", ctypes.c_int), ("cm", ctypes.c_int),
                ("cmap", W), ("mi", ctypes.c_int), ("map_state", ctypes.c_int), ("aem", ctypes.c_long), ("yem", ctypes.c_long),
                ("dnp", ctypes.c_long), ("ovr", ctypes.c_int), ("screen", ctypes.c_void_p)]
x.XGetWindowAttributes.argtypes = [ctypes.c_void_p, W, ctypes.POINTER(Attr)]

d = x.XOpenDisplay(None)
if not d:
    sys.exit(2)
root = x.XDefaultRootWindow(d)
atom = lambda n: x.XInternAtom(d, n.encode(), 0)
PID, STATE, HIDDEN, ACTIVE = atom("_NET_WM_PID"), atom("_NET_WM_STATE"), atom("_NET_WM_STATE_HIDDEN"), atom("_NET_ACTIVE_WINDOW")

def prop(w, a, n=64):
    t, f, ni, ba, data = W(), ctypes.c_int(), ctypes.c_ulong(), ctypes.c_ulong(), ctypes.c_void_p()
    if x.XGetWindowProperty(d, w, a, 0, n, 0, 0, ctypes.byref(t), ctypes.byref(f), ctypes.byref(ni), ctypes.byref(ba), ctypes.byref(data)) != 0 or not data.value:
        return []
    arr = ctypes.cast(data, ctypes.POINTER(ctypes.c_ulong))
    return [arr[i] for i in range(ni.value)]

def walk(w):
    r, p, kids, n = W(), W(), ctypes.POINTER(W)(), ctypes.c_uint()
    if not x.XQueryTree(d, w, ctypes.byref(r), ctypes.byref(p), ctypes.byref(kids), ctypes.byref(n)):
        return
    for i in range(n.value):
        yield kids[i]
        yield from walk(kids[i])

pid = int(sys.argv[1])
active = (prop(root, ACTIVE) or [0])[0]
def children(w):
    r, p, kids, n = W(), W(), ctypes.POINTER(W)(), ctypes.c_uint()
    if not x.XQueryTree(d, w, ctypes.byref(r), ctypes.byref(p), ctypes.byref(kids), ctypes.byref(n)):
        return []
    return [kids[i] for i in range(n.value)]

def shown(w):
    a = Attr(); x.XGetWindowAttributes(d, w, ctypes.byref(a))
    return a.map_state == 2 and a.w > 100 and a.h > 100

top = [w for w in children(root) if shown(w)]
wm = bool(prop(root, atom("_NET_SUPPORTING_WM_CHECK"), 1))
out = []
for w in walk(root):
    # Managed windows carry WM_STATE; without a window manager the program's windows sit right under the root.
    managed = bool(prop(w, atom("WM_STATE"), 2))
    if prop(w, PID, 1) == [pid] and (managed or (not wm and w in top)):
        a = Attr(); x.XGetWindowAttributes(d, w, ctypes.byref(a))
        out.append({"id": w, "width": a.w, "height": a.h, "mapped": a.map_state == 2, "minimized": HIDDEN in prop(w, STATE), "active": w == active})
print(json.dumps(out))
