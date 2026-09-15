"""Exercise the public MCP CUA facade against an owned, disposable X11 window."""
import base64
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time

WIDGET = r'''
import tkinter as tk, json, sys
from pathlib import Path
root = tk.Tk(); root.title("Nanocodex CUA integration"); root.geometry("600x360+20+20")
directory = Path(sys.argv[1]); clicks = 0
value = tk.StringVar()
def save(*_):
    (directory / "state.json").write_text(json.dumps({"text":value.get(),"clicks":clicks}))
def click():
    global clicks
    clicks += 1; button.configure(text="Clicked " + str(clicks)); save()
entry = tk.Entry(root, textvariable=value, font=("sans",20)); entry.pack(padx=25,pady=25,fill="x")
button = tk.Button(root, text="Click me", command=click, font=("sans",20)); button.pack(pady=20)
value.trace_add("write",save); root.update(); entry.focus_force(); save()
def point(widget): return {"x":widget.winfo_rootx()+widget.winfo_width()//2,"y":widget.winfo_rooty()+widget.winfo_height()//2}
(directory / "ready.json").write_text(json.dumps({"entry":point(entry),"button":point(button)}))
root.mainloop()
'''

children = []
def child(*args, **kwargs):
    process = subprocess.Popen(*args, **kwargs); children.append(process); return process

def wait_for(check, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            value = check()
            if value: return value
        except (FileNotFoundError, json.JSONDecodeError): pass
        time.sleep(0.025)
    raise AssertionError("Timed out waiting for the owned desktop")

try:
    with tempfile.TemporaryDirectory(prefix="nanocodex-cua-") as temporary:
        directory = Path(temporary)
        # This server is private to a network-isolated test container.
        read_fd, write_fd = os.pipe()
        child(["Xvfb", "-displayfd", str(write_fd), "-screen", "0", "800x600x24", "-nolisten", "tcp", "-ac"], pass_fds=[write_fd], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        os.close(write_fd)
        assert select.select([read_fd], [], [], 10)[0], "Xvfb did not start"
        display = ":" + os.read(read_fd, 128).decode().strip(); os.close(read_fd)
        environment = dict(os.environ, DISPLAY=display)
        child(["python3", "-c", WIDGET, temporary], env=environment)
        positions = wait_for(lambda: json.loads((directory / "ready.json").read_text()))
        runtime = child([os.environ.get("NANOCODEX_COMPUTER", "nanocodex-computer"), "--allow-native-control", "serve"], env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
        sequence = 0
        def rpc(method, params):
            global sequence
            sequence += 1
            runtime.stdin.write(json.dumps({"jsonrpc":"2.0","id":sequence,"method":method,"params":params})+"\n"); runtime.stdin.flush()
            assert select.select([runtime.stdout], [], [], 30)[0], "CUA response timed out"
            value = json.loads(runtime.stdout.readline())
            assert value.get("id") == sequence and "error" not in value, value
            result = value["result"]
            assert not result.get("isError"), result
            return result
        def js(code): return rpc("tools/call", {"name":"js","arguments":{"code":code}})
        rpc("initialize", {"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"nanocodex-live-test","version":"1"}})
        js("await cua.getState();")
        js("let desktop = cua.computer; await desktop.click("+json.dumps(positions["entry"])+"); await desktop.type_text({text:'Nanocodex native Ω 🧪'});")
        wait_for(lambda: json.loads((directory / "state.json").read_text())["text"] == "Nanocodex native Ω 🧪")
        js("await desktop.click("+json.dumps(positions["button"])+");")
        wait_for(lambda: json.loads((directory / "state.json").read_text())["clicks"] == 1)
        result = js("var shots = await desktop.get_screenshot(); await nodeRepl.emitImage(shots[0].data_url);")
        image = next(item for item in result["content"] if item["type"] == "image")
        data = base64.b64decode(image["data"], validate=True)
        assert data.startswith(b"\xff\xd8\xff") and data.endswith(b"\xff\xd9"), "Screenshot is not a JPEG"
        evidence = Path(os.environ.get("NANOCODEX_TEST_EVIDENCE", temporary)); evidence.mkdir(parents=True, exist_ok=True)
        (evidence / "linux-cua.jpg").write_bytes(data)
        rpc("tools/call", {"name":"js_reset","arguments":{}})
        reset = js("nodeRepl.write(typeof desktop);")
        assert "undefined" in json.dumps(reset)
        print(json.dumps({"native_text":True,"native_click":True,"screenshot_bytes":len(data),"reset":True,"state":json.loads((directory / "state.json").read_text())}))
finally:
    for process in reversed(children):
        if process.poll() is None:
            process.terminate()
            try: process.wait(timeout=3)
            except subprocess.TimeoutExpired: process.kill(); process.wait()
