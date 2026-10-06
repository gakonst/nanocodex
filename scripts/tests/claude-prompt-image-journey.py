#!/usr/bin/env python3
"""Actual CLI + Messages HTTP + SQLite image crash/replay journey, no live auth.

Build nanocodex-bin, then:
  python3 scripts/tests/claude-prompt-image-journey.py --binary target/debug/nanocodex
Writes inspectable commands, provider requests, JSONL and outcomes under output/.
"""
import argparse
import base64
import json
from pathlib import Path
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a9e8AAAAASUVORK5CYII="


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=Path("output/claude-prompt-images") / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    artifact.mkdir(parents=True)
    workspace = artifact / "workspace"
    workspace.mkdir()
    (workspace / "home").mkdir()
    image = workspace / "pixel.png"
    image.write_bytes(base64.b64decode(PNG))
    requests, errors = [], []
    received, release = threading.Event(), threading.Event()

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["content-length"])))
            requests.append(body)
            try:
                require(self.path == "/v1/messages", "wrong Messages route")
                require(self.headers.get("x-api-key") == "synthetic", "wrong synthetic key")
                blocks = body["messages"][-1]["content"]
                require([block["type"] for block in blocks] == ["text", "image"], "text/image order lost")
                require(blocks[0]["text"] == "Inspect the attached synthetic pixel.", "prompt text changed")
                require(blocks[1]["source"] == {"type": "base64", "media_type": "image/png", "data": PNG}, "frozen image bytes changed")
            except Exception as error:
                errors.append(str(error))
            (artifact / "provider.json").write_text(json.dumps(requests, indent=2))
            received.set()
            if len(requests) == 1:
                release.wait(60)
                return
            frames = [
                {"type":"message_start","message":{"id":"synthetic","role":"assistant","model":body["model"],"content":[],"usage":{"input_tokens":1,"output_tokens":0}}},
                {"type":"content_block_start","index":0,"content_block":{"type":"text","text":"frozen-image-restored"}},
                {"type":"content_block_stop","index":0},
                {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}},
                {"type":"message_stop"},
            ]
            response = "".join("data: " + json.dumps(frame) + "\n\n" for frame in frames).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(response)))
            self.end_headers()
            self.wfile.write(response)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    command = [str(binary), "run", "--claude", "--model", "claude-sonnet-5-5", "--thinking", "medium", "--claude-api-key", "synthetic", "--claude-messages-url", f"http://127.0.0.1:{server.server_port}/v1/messages", "--cwd", str(workspace), "--rollouts", "false", "--browser=none", "--mcp-defaults", "false", "--mcp-codex-config", "false", "--web-search", "false", "--image-generation", "false", "--subagents", "false", "--memory", "false", "--local-durability", str(artifact / "session.sqlite"), "--local-durability-state-id", "image-session", "--request-id", "image-operation", "--image", str(image), "Inspect the attached synthetic pixel."]
    environment = {"HOME": str(workspace / "home"), "CODEX_HOME": str(workspace / "codex-home"), "PATH": "/usr/bin:/bin", "NANOCODEX_COMPUTER": "off"}
    (artifact / "scenario.json").write_text(json.dumps({"command": command, "environment": environment, "expected": "SIGKILL during first HTTP request; delete local file; SQLite resume sends identical image bytes; terminal replay emits answer without HTTP"}, indent=2))
    outcome = {"success": False}
    first = None
    try:
        with (artifact / "crashed.jsonl").open("wb") as stdout, (artifact / "crashed.stderr").open("wb") as stderr:
            first = subprocess.Popen(command, cwd=workspace, env=environment, stdout=stdout, stderr=stderr)
            require(received.wait(30), "CLI did not reach provider: " + (artifact / "crashed.stderr").read_text())
            first.kill()
            first.wait(timeout=10)
        image.unlink()
        for phase in ("resume", "terminal-replay"):
            result = subprocess.run(command, cwd=workspace, env=environment, capture_output=True, timeout=60)
            (artifact / f"{phase}.jsonl").write_bytes(result.stdout)
            (artifact / f"{phase}.stderr").write_bytes(result.stderr)
            require(result.returncode == 0, f"{phase} failed: {result.stderr.decode(errors='replace')}")
            require(b"frozen-image-restored" in result.stdout, f"{phase} omitted answer")
            require(not errors, "; ".join(errors))
            require(len(requests) == 2, f"{phase} unexpected HTTP count: {len(requests)}")
        outcome = {"success": True, "provider_requests": 2, "terminal_replay_requests": 0, "crashed_exit": first.returncode, "deleted_image_resumed": True, "identical_frozen_bytes": True}
    except Exception as error:
        outcome["error"] = str(error)
        raise
    finally:
        if first is not None and first.poll() is None:
            first.kill()
            first.wait(timeout=10)
        release.set()
        server.shutdown()
        (artifact / "outcome.json").write_text(json.dumps(outcome, indent=2))
        print(json.dumps({"artifact": str(artifact), **outcome}))


if __name__ == "__main__":
    main()
