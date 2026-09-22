#!/usr/bin/env python3
"""Test MCP provider: real guest desktop I/O, no upstream runtime dependency."""
import ctypes
import json
import subprocess
import sys
import time

for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request["method"]
    if method == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {}},
                  "serverInfo": {"name": "guest-desktop-test", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "desktop", "description": "Guest screenshot and input fixture",
                             "inputSchema": {"type": "object", "properties": {
                                 "action": {"type": "string"}, "text": {"type": "string"},
                                 "key": {"type": "integer"}}, "required": ["action"]}}]}
    elif method == "tools/call":
        assert request["params"]["name"] == "desktop"
        # Readiness of the transport can precede the terminal receiving focus.
        # Wait for the actual X input focus before testing keyboard delivery.
        if request["params"]["arguments"]["action"] == "type":
            x11 = ctypes.CDLL("libX11.so.6")
            x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
            x11.XOpenDisplay.restype = ctypes.c_void_p
            x11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
            x11.XDefaultRootWindow.restype = ctypes.c_ulong
            x11.XGetInputFocus.argtypes = [ctypes.c_void_p, ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_int)]
            x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
            display = x11.XOpenDisplay(None)
            assert display, "guest display unavailable"
            try:
                root = x11.XDefaultRootWindow(display)
                deadline = time.monotonic() + 5
                focus, revert = ctypes.c_ulong(), ctypes.c_int()
                while True:
                    x11.XGetInputFocus(display, ctypes.byref(focus), ctypes.byref(revert))
                    if focus.value not in (0, 1, root):
                        break
                    assert time.monotonic() < deadline, "terminal did not receive focus"
                    time.sleep(0.025)
            finally:
                x11.XCloseDisplay(display)
        reply = json.loads(subprocess.check_output([
            "/usr/local/bin/nanocodex-vm-guest", "--desktop-request",
            json.dumps(request["params"]["arguments"]), "/run/nanocodex-hand-desktop"]))
        assert reply["status"] == "ok", reply
        content = [{"type": "text", "text": json.dumps({"status": reply["status"]})}]
        if "jpeg" in reply:
            content.append({"type": "image", "mimeType": "image/jpeg", "data": reply["jpeg"]})
        result = {"content": content, "isError": False}
    else:
        raise AssertionError(method)
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
