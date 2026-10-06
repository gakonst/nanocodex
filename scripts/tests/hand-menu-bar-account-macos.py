#!/usr/bin/env python3
"""Exercise the real AppKit menu + CLI against a synthetic loopback account.

Requires a macOS GUI session with Accessibility granted to its launcher. Does
not install services, log out the user, sign in, or invoke Hand mutations. Uses
a separate temporary bundle, credential store, and preferences domain. The real
local service is observed only. Login submission and a fresh OS-user installation
are covered separately; this journey asserts actual visible menu state.
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import threading
import time
import uuid

AX_SOURCE = r'''
import AppKit
import ApplicationServices
let pid = pid_t(CommandLine.arguments[1])!
guard AXIsProcessTrusted() else { fputs("Accessibility permission is required\n", stderr); exit(2) }
let app = AXUIElementCreateApplication(pid)
var rows: [[String: Any]] = []
var elements: [(String, String, AXUIElement)] = []
func walk(_ element: AXUIElement, _ depth: Int) {
    if depth > 8 || rows.count > 3000 { return }
    func attribute(_ name: String) -> CFTypeRef? {
        var value: CFTypeRef?
        AXUIElementCopyAttributeValue(element, name as CFString, &value)
        return value
    }
    let role = attribute(kAXRoleAttribute) as? String ?? ""
    let title = attribute(kAXTitleAttribute) as? String ?? ""
    if role == kAXMenuItemRole || role == kAXMenuBarItemRole {
        rows.append(["role":role, "title":title, "enabled":attribute(kAXEnabledAttribute) as? Bool ?? false])
        elements.append((role, title, element))
    }
    for child in (attribute(kAXChildrenAttribute) as? [AXUIElement] ?? []).prefix(2500) { walk(child, depth + 1) }
}
walk(app, 0)
if CommandLine.arguments.count > 2 {
    let title = CommandLine.arguments[2]
    guard title == "Refresh Status" else { exit(64) }
    guard let target = elements.first(where: { $0.0 == kAXMenuItemRole && $0.1 == title }) else { exit(3) }
    var result = AXUIElementPerformAction(target.2, kAXPressAction as CFString)
    if result != .success, let bar = elements.first(where: { $0.0 == kAXMenuBarItemRole }) {
        _ = AXUIElementPerformAction(bar.2, kAXPressAction as CFString)
        result = AXUIElementPerformAction(target.2, kAXPressAction as CFString)
    }
    if result != .success { exit(4) }
}
let data = try JSONSerialization.data(withJSONObject: rows, options: [.sortedKeys])
print(String(data: data, encoding: .utf8)!)
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--helper", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args()
    for path in (args.cli, args.helper):
        if not path.is_absolute() or not path.is_file():
            parser.error("--cli and --helper must name absolute existing executables")
    evidence = args.evidence.resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    ax_source = evidence / "menu-accessibility.swift"
    ax_source.write_text(AX_SOURCE)
    reader = evidence / "menu-accessibility"
    subprocess.run(["xcrun", "swiftc", str(ax_source), "-o", str(reader)], check=True)
    mode = {"name": "ready"}
    requests = []
    synthetic_key = "ncx_live_" + "a" * 12 + "_" + "b" * 43

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            requests.append({"method": "GET", "path": self.path, "scenario": mode["name"]})
            assert self.headers.get("Authorization") == "Bearer " + synthetic_key
            status = 200
            if mode["name"] == "network" and self.path == "/v1/me":
                status, value = 503, {}
            elif mode["name"] == "expired":
                status, value = 401, {}
            elif self.path == "/v1/me":
                value = {"authentication": "api_key", "user": {"id": "synthetic-user", "name": "Synthetic account"},
                         "organization": {"id": "synthetic-org"}, "team": {"id": "synthetic-team"}, "role": "owner"}
            elif self.path == "/v1/account/hands":
                if mode["name"] == "denied":
                    status, value = 403, {}
                else:
                    value = {"data": [{"id": "synthetic-mac", "name": "Synthetic Mac", "capabilities": ["native", "filesystem", "process"]},
                                      {"id": "vm:synthetic-vm", "name": "Build VM", "capabilities": ["shell", "vm"]}]}
            elif self.path == "/v1/account/hands/screens":
                value = {"surfaces": [{"machine_id": "synthetic-screen", "machine_name": "Synthetic screen", "transport": "frames-v1"}]}
            else:
                status, value = 404, {}
            payload = json.dumps(value).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    origin = "http://127.0.0.1:" + str(server.server_port)
    receipt = {"cli": str(args.cli), "helper": str(args.helper), "scenarios": [], "requests": requests}
    process = None
    bundle_id = "com.nanocodex.hand-menu-journey." + uuid.uuid4().hex
    try:
        with tempfile.TemporaryDirectory(prefix="native-menu-", dir=evidence) as temporary:
            root = Path(temporary)
            contents = root / "Menu Journey.app/Contents"
            executable = contents / "MacOS/MenuJourney"
            executable.parent.mkdir(parents=True)
            shutil.copy2(args.helper, executable)
            (contents / "Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": bundle_id,
                "CFBundleName": "Hand Menu Journey", "CFBundleExecutable": "MenuJourney", "CFBundlePackageType": "APPL", "LSUIElement": True}))
            subprocess.run(["/usr/bin/codesign", "--force", "--sign", "-", str(contents.parent)], check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            credential = root / "account.json"
            env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(root), "CODEX_HOME": str(root),
                   "NANOCODEX_DIR": str(root / "install"), "NANOCODEX_ACCOUNT_FILE": str(credential),
                   "NANOCODEX_MANAGED_URL": origin, "NO_COLOR": "1"}
            with (evidence / "helper-stderr.log").open("wb") as log:
                process = subprocess.Popen([str(executable), "--cli", str(args.cli)], cwd=root, env=env,
                                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=log)

                def snapshot():
                    if process.poll() is not None:
                        raise RuntimeError("Native helper exited unexpectedly")
                    result = subprocess.run([str(reader), str(process.pid)], capture_output=True, check=True, timeout=10)
                    return json.loads(result.stdout)

                def refresh():
                    subprocess.run([str(reader), str(process.pid), "Refresh Status"], capture_output=True, check=True, timeout=10)

                def expect(name, required, absent=(), sign_in=None):
                    deadline = time.monotonic() + 25
                    while time.monotonic() < deadline:
                        rows = snapshot()
                        titles = [row["title"] for row in rows]
                        text = "\n".join(titles)
                        enabled = next((row["enabled"] for row in rows if row["title"] == "Sign In…"), None)
                        if (all(value in text for value in required) and all(value not in text for value in absent)
                                and (sign_in is None or enabled == sign_in)):
                            receipt["scenarios"].append({"name": name, "menu": rows})
                            (evidence / (name + ".json")).write_text(json.dumps(rows, indent=2))
                            return
                        time.sleep(0.2)
                    raise AssertionError(f"{name}: unexpected native menu: {titles}")

                expect("signed-out", ["Menu companion: Running", "Account: Signed out", "Sign in to view"], sign_in=True)
                assert not requests, "Signed-out menu made account requests"
                credential.write_text(json.dumps({"version": 1, "accounts": {origin: {"api_key": synthetic_key}}}))
                credential.chmod(0o600)
                refresh()
                expect("signed-in", ["Account: Signed in · Synthetic account", "Synthetic Mac · Connected", "Build VM · Connected", "Synthetic screen · Screen advertised"], sign_in=False)
                mode["name"] = "network"
                refresh()
                expect("network-error", ["Account: Unable to verify", "Server unavailable"], ["Synthetic Mac", "Build VM", "Synthetic screen", "Account: Signed out"], False)
                mode["name"] = "denied"
                refresh()
                expect("permission-denied", ["Account: Signed in", "Connected Hands: Access denied"], ["Synthetic Mac · Connected"], False)
                mode["name"] = "expired"
                refresh()
                expect("expired", ["Account: Sign-in expired", "Sign in again to view"], ["Synthetic Mac", "Build VM"], True)
                mode["name"] = "ready"
                refresh()
                expect("recovered", ["Synthetic Mac · Connected", "Build VM · Connected", "Account: Signed in"], sign_in=False)
                process.terminate()
                process.wait(timeout=10)
                process = None
                assert not (root / "install").exists(), "Status polling created install/update state"
                receipt["passed"] = True
    finally:
        if process is not None:
            process.terminate()
            process.wait(timeout=10)
        server.shutdown()
        subprocess.run(["/usr/bin/defaults", "delete", bundle_id], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        (evidence / "receipt.json").write_text(json.dumps(receipt, indent=2))
    print(json.dumps({"passed": True, "scenarios": len(receipt["scenarios"]), "evidence": str(evidence)}))


if __name__ == "__main__":
    main()
