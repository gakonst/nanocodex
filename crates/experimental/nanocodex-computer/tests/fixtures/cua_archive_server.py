"""Isolated HTTPS archive endpoint for the real curl downloader journey."""
import http.server
import json
import pathlib
import ssl
import subprocess
import sys
import threading
import time
import zipfile

root = pathlib.Path(sys.argv[1])
if len(sys.argv) == 3:
    with zipfile.ZipFile(root / "upstream.zip") as source, zipfile.ZipFile(sys.argv[2]) as result:
        assert result.testzip() is None
        expected = [name for name in source.namelist() if not name.endswith("app.asar")]
        assert result.namelist() == expected
        for name in expected:
            assert source.read(name) == result.read(name), name
    print(json.dumps({"entries": len(expected), "crc_and_contents": "verified"}))
    sys.exit(0)

with zipfile.ZipFile(root / "upstream.zip", "w", compression=zipfile.ZIP_STORED) as archive:
    for name in ["Info.plist", "MacOS/Codex", "_CodeSignature/CodeResources"]:
        archive.writestr("Codex.app/Contents/" + name, b"fixture")
    archive.writestr("Codex.app/Contents/Resources/cua_node/bin/node", b"native fixture\n" * (4 * 1024 * 1024))
    for number in range(8):
        archive.writestr(f"Codex.app/Contents/Resources/cua_node/lib/{number}.mjs", b"export default true;")
    archive.writestr("Codex.app/Contents/Resources/app.asar", b"excluded" * (1024 * 1024))
blob = (root / "upstream.zip").read_bytes()
(root / "cert.conf").write_text("[req]\ndistinguished_name=dn\nx509_extensions=v3\nprompt=no\n[dn]\nCN=localhost\n[v3]\nsubjectAltName=DNS:localhost\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,digitalSignature,keyEncipherment\n")
subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", str(root / "key.pem"), "-out", str(root / "cert.pem"), "-days", "1", "-config", str(root / "cert.conf")], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
lock = threading.Lock()
active = 0

class Endpoint(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def do_GET(self):
        global active
        start, end = map(int, self.headers["Range"].removeprefix("bytes=").split("-"))
        payload = end - start + 1 > 65557
        with lock:
            active += 1
            with (root / "requests.jsonl").open("a") as trace:
                trace.write(json.dumps({"path": self.path, "start": start, "end": end, "active": active, "event": "start"}) + "\n")
        try:
            if payload:
                time.sleep(0.3)
            if self.path == "/failure" and payload:
                self.send_error(503)
                return
            self.send_response(200 if self.path == "/oversize" and payload else 206)
            total = len(blob) + (1 if self.path == "/changed" and payload else 0)
            range_start = start + (1 if self.path == "/wrongrange" and payload else 0)
            if not (self.path == "/missingrange" and payload):
                self.send_header("Content-Range", f"bytes {range_start}-{end}/{total}")
            length = len(blob) if self.path == "/oversize" and payload else end - start + 1
            if self.path == "/short" and payload:
                length -= 1
            self.send_header("Content-Length", str(length))
            self.end_headers()
            self.wfile.write(blob[start:start + length])
        except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
            pass
        finally:
            with lock:
                active -= 1

http.server.ThreadingHTTPServer.request_queue_size = 32
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Endpoint)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(root / "cert.pem", root / "key.pem")
server.socket = context.wrap_socket(server.socket, server_side=True)
print(json.dumps({"url": f"https://localhost:{server.server_port}", "length": len(blob)}), flush=True)
server.serve_forever()
