"""Read-only X11 keyboard mapping. Never allocates or changes key symbols."""
import ctypes
import ctypes.util


def keyboard_mapping():
    name = ctypes.util.find_library('X11')
    if not name:
        raise ValueError('libX11 is unavailable')
    x = ctypes.CDLL(name)
    x.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x.XOpenDisplay.restype = ctypes.c_void_p
    x.XCloseDisplay.argtypes = [ctypes.c_void_p]
    x.XDisplayKeycodes.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_int)]
    x.XGetKeyboardMapping.argtypes = [ctypes.c_void_p, ctypes.c_ubyte, ctypes.c_int, ctypes.POINTER(ctypes.c_int)]
    x.XGetKeyboardMapping.restype = ctypes.POINTER(ctypes.c_ulong)
    x.XFree.argtypes = [ctypes.c_void_p]
    display = x.XOpenDisplay(None)
    if not display:
        raise ValueError('cannot open current X11 display')
    pointer = None
    try:
        low, high, width = ctypes.c_int(), ctypes.c_int(), ctypes.c_int()
        x.XDisplayKeycodes(display, ctypes.byref(low), ctypes.byref(high))
        if not 8 <= low.value <= high.value <= 255:
            raise ValueError('invalid X11 keycode range')
        count = high.value-low.value+1
        pointer = x.XGetKeyboardMapping(display, low.value, count, ctypes.byref(width))
        if not pointer or not 1 <= width.value <= 64:
            raise ValueError('invalid X11 key mapping')
        return {low.value+i: tuple(pointer[i*width.value+j] for j in range(width.value)) for i in range(count)}
    finally:
        if pointer:
            x.XFree(pointer)
        x.XCloseDisplay(display)
