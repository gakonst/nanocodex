#!/usr/bin/env python3
"""Start Codex threads from a copied rollout and an earlier turn through the shipped CLI.

python3 scripts/tests/codex-resume-from-journey.py --binary target/debug/nanocodex
A local Responses server records every model request. Synthetic homes, requests,
and terminal transcripts are retained in ignored output/.
"""
import argparse
import errno
import fcntl
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import struct
import subprocess
import termios
import threading
import time
from uuid import uuid4

LOCAL_TOOLS = ("exec_command", "apply_patch", "write_stdin", "view_image")


def sse(events):
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


def answer(text):
    return sse([
        {"type": "response.created", "response": {"id": f"resp-{text}"}},
        {"type": "response.output_item.done", "item": {
            "type": "message", "role": "assistant", "id": f"msg-{text}",
            "content": [{"type": "output_text", "text": text}]}},
        {"type": "response.completed", "response": {"id": f"resp-{text}", "usage": {
            "input_tokens": 1, "input_tokens_details": None, "output_tokens": 1,
            "output_tokens_details": None, "total_tokens": 2}}},
    ])


class Provider:
    """Answers each prompt marker with its matching answer and records the request."""

    def __init__(self):
        self.requests = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):  # noqa: N802
                body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))))
                owner.requests.append(body)
                latest = json.dumps(body.get("input", [])[-3:])
                marker = next((m for m in ("THIRD", "SECOND", "FIRST") if f"{m}_PROMPT" in latest), "UNKNOWN")
                payload = answer(f"{marker}_ANSWER")
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *_):
                return

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}/v1"


def rollouts(home):
    return sorted((home / "sessions").rglob("rollout-*.jsonl"))


def rows(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def completed_turns(path):
    return sum(1 for r in rows(path) if r.get("type") == "event_msg"
               and r["payload"].get("type") == "task_complete")


def tui(command, env, cwd, home, turns, transcript_path):
    """Run the TUI in a PTY until its rollout has `turns` completed turns, then exit."""
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 160, 0, 0))
    child = subprocess.Popen(command, cwd=cwd, env=env, stdin=slave, stdout=slave,
                             stderr=slave, start_new_session=True)
    os.close(slave)
    transcript, done, deadline = bytearray(), False, time.monotonic() + 30
    try:
        while time.monotonic() < deadline and child.poll() is None:
            if select.select([master], [], [], 0.1)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                transcript.extend(chunk)
                if b"\x1b[6n" in chunk:
                    os.write(master, b"\x1b[1;1R")
            files = rollouts(home)
            if not done and files and completed_turns(files[-1]) >= turns:
                done = True
                time.sleep(0.5)
                os.write(master, b"\x04")
        child.wait(timeout=5)
        assert done, "the TUI turn never completed; inspect the transcript"
    finally:
        if child.poll() is None:
            child.kill()
            child.wait()
        os.close(master)
        transcript_path.write_bytes(re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(transcript)))


def tool_names(request):
    """Names of model-visible tools, including Code Mode nested declarations."""
    definitions = list(request.get("tools", []))
    for item in request.get("input", []):
        if item.get("type") == "additional_tools":
            definitions += item.get("tools", [])
    names = {d.get("name") for d in definitions}
    for d in definitions:
        names.update(re.findall(r"^### `([^`]+)`", d.get("description", ""), re.MULTILINE))
    return names


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("output/codex-resume-from") / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    artifact.mkdir(parents=True)
    provider = Provider()
    checks, outcome = [], {"success": False}
    try:
        source_home, target_home = artifact / "source-home", artifact / "target-home"
        source_ws, target_ws = artifact / "source-workspace", artifact / "target-workspace"
        for directory in (source_home, target_home, source_ws, target_ws):
            directory.mkdir()
        common = ["--api-key", "synthetic-test-key", "--api-base-url", provider.url,
                  "--responses-transport", "https", "--browser=none", "--mcp-defaults", "false",
                  "--web-search", "false", "--image-generation", "false"]
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "TERM": "xterm-256color",
               "NANOCODEX_COMPUTER": "off", "HOME": str(source_home), "CODEX_HOME": str(source_home)}

        # Two real turns recorded by the shipped CLI.
        first = subprocess.run([str(binary), "run", *common, "--cwd", str(source_ws), "FIRST_PROMPT"],
                               cwd=source_ws, env=env, capture_output=True, text=True, timeout=30)
        (artifact / "first.stdout.jsonl").write_text(first.stdout)
        assert first.returncode == 0, first.stderr
        [original] = rollouts(source_home)
        thread = rows(original)[0]["payload"]["id"]
        tui([str(binary), "resume", thread, *common, "--prompt", "SECOND_PROMPT"],
            env, source_ws, source_home, 2, artifact / "second.terminal.txt")
        control = provider.requests[-1]
        assert {"exec_command", "apply_patch"} <= tool_names(control), \
            f"control request lacks the default local tools: {tool_names(control)}"
        checks.append({"case": "control", "tools": sorted(filter(None, tool_names(control)))})

        # Copy the rollout elsewhere, as if it had been recorded on another machine.
        copied = artifact / "exported" / original.name
        copied.parent.mkdir()
        shutil.copy2(original, copied)
        digest = hashlib.sha256(copied.read_bytes()).hexdigest()
        env.update(HOME=str(target_home), CODEX_HOME=str(target_home))
        before = len(provider.requests)
        tui([str(binary), "resume", "--from", str(copied), "--at", "1", "--workspace-tools", "false",
             *common, "--cwd", str(target_ws), "--prompt", "THIRD_PROMPT"],
            env, artifact, target_home, 2, artifact / "third.terminal.txt")
        assert len(provider.requests) == before + 1, "expected exactly one model request"
        request = provider.requests[-1]
        history = json.dumps(request.get("input", []))
        assert "FIRST_PROMPT" in history and "FIRST_ANSWER" in history, "turn 1 history missing"
        assert "SECOND_PROMPT" not in history and "SECOND_ANSWER" not in history, "turn 2 leaked"
        offered = tool_names(request)
        assert not offered & set(LOCAL_TOOLS), f"local tools offered: {offered}"
        assert hashlib.sha256(copied.read_bytes()).hexdigest() == digest, "source rollout changed"
        [forked] = rollouts(target_home)
        meta = rows(forked)[0]["payload"]
        assert meta["id"] != thread and meta["cwd"] == str(target_ws.resolve()), meta
        assert completed_turns(forked) == 2, "fork did not record the new turn"
        checks.append({"case": "from-at-1", "source": str(copied), "new_thread": meta["id"],
                       "cwd": meta["cwd"], "turn_2_in_request": False,
                       "tools": sorted(filter(None, offered))})

        # A point past the end fails before any model request.
        before = len(provider.requests)
        late = subprocess.run([str(binary), "resume", "--from", str(copied), "--at", "5", *common,
                               "--cwd", str(target_ws)], cwd=artifact, env=env,
                              capture_output=True, text=True, timeout=30)
        (artifact / "late.stderr.txt").write_text(late.stderr)
        assert late.returncode != 0 and "fewer than 5" in late.stderr, late.stderr
        assert len(provider.requests) == before and len(rollouts(target_home)) == 1
        checks.append({"case": "at-past-end", "exit": late.returncode})
        outcome["success"] = True
    finally:
        provider.server.shutdown()
        (artifact / "requests.json").write_text(json.dumps(provider.requests, indent=2) + "\n")
        outcome["checks"] = checks
        (artifact / "outcome.json").write_text(json.dumps(outcome, indent=2) + "\n")
        print(f"Resume-from evidence: {artifact}")


if __name__ == "__main__":
    main()
