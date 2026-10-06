#!/usr/bin/env python3
"""Local TLS pages for claude_web public-API acceptance (synthetic data only)."""
import json
from pathlib import Path
import ssl
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

cert, key, log = sys.argv[1:]


class Pages(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        with Path(log).open("a") as out:
            out.write(json.dumps({"path": self.path, "headers": dict(self.headers)}) + "\n")
        redirects = {"/start": "/final", "/private": f"https://127.0.0.1:{self.server.server_port}/secret", "/downgrade": f"http://pages.fixture.test:{self.server.server_port}/secret", "/loop": "/loop"}
        if self.path in redirects:
            self.send_response(302)
            self.send_header("Location", redirects[self.path])
            self.send_header("Set-Cookie", "fixture=must-not-follow")
            self.end_headers()
            return
        body = ("<h1>Synthetic page title</h1>" + ("💡" * 40000 if self.path == "/large" else "")).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream" if self.path == "/binary" else "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.path == "/slow":
            time.sleep(22)
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass


server = ThreadingHTTPServer(("127.0.0.1", 0), Pages)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(cert, key)
server.socket = context.wrap_socket(server.socket, server_side=True)
print(server.server_port, flush=True)
server.serve_forever()
