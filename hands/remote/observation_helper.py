"""Fixed observation helper. No executable, code, or path comes from agent input."""
import json
import math
import os
import stat
import sys
import time

MAX_FILE = 65536
MAX_DATA = 8192
MAX_NODES = 128
MAX_DEPTH = 8
MAX_TEXT = 512


def unavailable(code):
    return {"status": "unavailable", "error": code}


def external(path, context):
    if not context:
        return unavailable("context_required")
    flags = os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW
    fd = os.open(path, flags)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid():
            return {"status": "error", "error": "unsafe_snapshot_file"}
        raw = stream.read(MAX_FILE + 1)
    if len(raw) > MAX_FILE:
        return {"status": "error", "error": "snapshot_too_large"}
    snapshot = json.loads(raw)
    if (not isinstance(snapshot, dict)
            or set(snapshot) != {"schemaVersion", "capturedAt", "app", "window", "data"}
            or type(snapshot["schemaVersion"]) is not int
            or snapshot["schemaVersion"] != 1
            or isinstance(snapshot["capturedAt"], bool)
            or not isinstance(snapshot["capturedAt"], int)
            or not 0 < snapshot["capturedAt"] <= int(time.time() * 1000) + 1000
            or not isinstance(snapshot["data"], dict)
            or any(not isinstance(snapshot[key], str)
                   or not 0 < len(snapshot[key].encode("utf-8")) <= MAX_TEXT
                   for key in ("app", "window"))):
        return {"status": "error", "error": "invalid_snapshot"}
    # Compare before exposing data, timestamps, or identity from unrelated apps.
    if snapshot["app"] != context["app"] or snapshot["window"] != context["window"]:
        return unavailable("context_mismatch")
    count = 0

    def bounded(value, depth=0):
        nonlocal count
        count += 1
        if count > 2048 or depth > MAX_DEPTH:
            return False
        if isinstance(value, str):
            return len(value.encode("utf-8")) <= MAX_TEXT
        if isinstance(value, dict):
            return all(isinstance(k, str) and len(k.encode("utf-8")) <= MAX_TEXT
                       and bounded(v, depth + 1) for k, v in value.items())
        if isinstance(value, list):
            return all(bounded(v, depth + 1) for v in value)
        return value is None or type(value) in (bool, int) or (type(value) is float and math.isfinite(value))

    data = snapshot["data"]
    if not bounded(data) or len(json.dumps(data, allow_nan=False, ensure_ascii=False, separators=(",", ":")).encode()) > MAX_DATA:
        return {"status": "error", "error": "snapshot_limits_exceeded"}
    return {"status": "ok", "capturedAt": snapshot["capturedAt"], "data": data}


def clipped(text):
    return text.encode("utf-8")[:MAX_TEXT].decode("utf-8", errors="ignore")


def atspi(context):
    if not os.environ.get("DBUS_SESSION_BUS_ADDRESS"):
        return unavailable("session_bus_unavailable")
    try:
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
    except (ImportError, ValueError):
        return unavailable("atspi_not_installed")
    try:
        Atspi.set_timeout(100, 100)
        desktop = Atspi.get_desktop(0)
    except Exception:
        return unavailable("accessibility_bus_unavailable")
    if desktop is None:
        return unavailable("accessibility_bus_unavailable")
    selected = None
    app_name = None
    # Inspection is bounded even if a broken bus returns very large child counts.
    for i in range(min(desktop.get_child_count(), 64)):
        app = desktop.get_child_at_index(i)
        name = app.get_name() or ""
        if context and name != context["app"]:
            continue
        for j in range(min(app.get_child_count(), 64)):
            window = app.get_child_at_index(j)
            if context:
                matches = (window.get_name() or "") == context["window"]
            else:
                matches = window.get_state_set().contains(Atspi.StateType.ACTIVE)
            if matches:
                selected, app_name = window, name
                break
        if selected is not None:
            break
    if selected is None:
        return unavailable("matching_window_unavailable")
    nodes = []
    partial = False
    pending = [(selected, None, 0)]
    started = time.monotonic()
    while pending and len(nodes) < MAX_NODES and time.monotonic() - started < .35:
        node, parent, depth = pending.pop()
        try:
            # Password nodes and every descendant are excluded before reading names.
            if node.get_role() == Atspi.Role.PASSWORD_TEXT:
                continue
            role = node.get_role_name() or ""
            name = node.get_name() or ""
            states = node.get_state_set()
            item = {"parent": parent, "role": role[:80], "name": clipped(name),
                    "states": {key: states.contains(state) for key, state in (
                        ("showing", Atspi.StateType.SHOWING),
                        ("enabled", Atspi.StateType.ENABLED),
                        ("focused", Atspi.StateType.FOCUSED))}}
            try:
                rect = node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
                bounds = {key: int(getattr(rect, key)) for key in ("x", "y", "width", "height")}
                if (all(abs(value) <= 1000000 for value in bounds.values())
                        and bounds["width"] >= 0 and bounds["height"] >= 0):
                    item["bounds"] = bounds
            except Exception:
                pass  # Text-only accessibles need not expose Component.

            if len(name.encode("utf-8")) > MAX_TEXT:
                partial = True
            index = len(nodes)
            nodes.append(item)
            children = node.get_child_count()
            if depth >= MAX_DEPTH:
                partial |= children > 0
            else:
                room = MAX_NODES - len(nodes) - len(pending)
                partial |= children > room
                for child in range(min(children, max(0, room)) - 1, -1, -1):
                    pending.append((node.get_child_at_index(child), index, depth + 1))
            if len(json.dumps(nodes, ensure_ascii=False, separators=(",", ":")).encode()) > MAX_DATA - 1200:
                nodes.pop()
                partial = True
                break
        except Exception:
            partial = True
    partial |= bool(pending)
    return {"status": "partial" if partial else "ok", "capturedAt": int(time.time() * 1000),
            "data": {"app": clipped(app_name), "window": clipped(selected.get_name() or ""), "coordinateSpace": "atspi_reported_screen", "boundsVerified": False, "nodes": nodes}}


def main():
    try:
        request = json.loads(sys.stdin.buffer.read(4097))
        if sys.argv[1] == "external":
            result = external(sys.argv[2], request.get("context"))
        elif sys.argv[1] == "atspi":
            result = atspi(request.get("context"))
        else:
            result = {"status": "error", "error": "unknown_provider"}
    except FileNotFoundError:
        result = unavailable("snapshot_missing")
    except Exception:
        # Never echo paths, bus addresses, file contents, or GI exception text.
        result = {"status": "error", "error": "provider_failed"}
    print(json.dumps(result, allow_nan=False, ensure_ascii=False, separators=(",", ":")))


if __name__ == "__main__":
    main()
