#!/usr/bin/env python3
"""One `nanocodex resume` picker over Codex and Claude sessions, through a real PTY.

Build separately, then run:
  python3 scripts/tests/unified-resume-picker-journey.py --binary target/debug/nanocodex

Two sessions are recorded with the shipped `nanocodex run`, a Codex one and then
a Claude one, under a throwaway HOME. `nanocodex resume` without an ID must list
both in one picker (newest first); Enter resumes the selected Claude session in
the Claude harness with its saved transcript. Esc cancels without contacting a
provider, and an unknown ID fails before any provider request. Only the HTTP
model providers are synthetic. Evidence: ignored output/unified-resume-picker/<run>/.
"""
import argparse
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import struct
import subprocess
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

CODEX_PROMPT = "codex-picker-original-prompt"
CLAUDE_PROMPT = "claude-picker-original-prompt"
FOLLOWUP = "claude-picker-followup-prompt"
ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]|\x1b[()][0-9A-B]|\x1b[=>]")


def require(condition, message):
    if not condition:
        raise AssertionError(message)


class Providers:
    """Synthetic Responses and Messages endpoints recording every request."""

    def __init__(self):
        self.requests = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):  # noqa: N802
                body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))))
                owner.requests.append({"path": self.path, "body": body})
                text = "claude-picker-complete" if FOLLOWUP in json.dumps(body) else "picker-turn-complete"
                payload = owner.messages(body["model"], text) if self.path.endswith("/messages") else owner.responses(text)
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"

    @staticmethod
    def responses(text):
        events = [
            {"type": "response.created", "response": {"id": "resp-" + uuid4().hex}},
            {"type": "response.output_item.done", "item": {
                "type": "message", "role": "assistant", "id": "msg-" + uuid4().hex,
                "content": [{"type": "output_text", "text": text}]}},
            {"type": "response.completed", "response": {"id": "resp-done", "usage": {
                "input_tokens": 1, "input_tokens_details": None, "output_tokens": 1,
                "output_tokens_details": None, "total_tokens": 2}}},
        ]
        return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()

    @staticmethod
    def messages(model, text):
        events = [
            {"type": "message_start", "message": {"id": "msg_" + uuid4().hex, "type": "message", "role": "assistant",
                                                  "model": model, "content": [], "usage": {"input_tokens": 1, "output_tokens": 0}}},
            {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}},
            {"type": "content_block_stop", "index": 0},
            {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}},
            {"type": "message_stop"},
        ]
        return "".join("data: " + json.dumps(e) + "\n\n" for e in events).encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("output/unified-resume-picker") / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    artifact.mkdir(parents=True)
    home, workspace = artifact / "home", artifact / "workspace"
    home.mkdir()
    workspace.mkdir()
    env = {"HOME": str(home), "PATH": os.environ.get("PATH", "/usr/bin:/bin"), "TERM": "xterm-256color",
           "NANOCODEX_COMPUTER": "off", "NANOCODEX_LINK_HOMES": "false"}
    providers = Providers()
    commands, checks = [], []
    outcome = {"success": False, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
               "commands": commands, "checks": checks}
    shared = ["--browser=none", "--mcp-defaults", "false", "--web-search", "false",
              "--image-generation", "false", "--subagents", "false", "--memory", "false"]
    codex = ["--api-key", "synthetic-test-key", "--api-base-url", providers.base + "/v1",
             "--responses-transport", "https"]
    claude = ["--claude-api-key", "synthetic-test-key", "--claude-messages-url", providers.base + "/v1/messages",
              "--mcp-codex-config", "false"]
    # Credentials for both families, so whichever harness the stored session
    # selects can connect; the picker itself chooses the family.
    both = [*codex, *claude, *shared]

    def run(name, command, expect_ok=True, timeout=60):
        commands.append({"name": name, "command": shlex.join(command)})
        result = subprocess.run(command, cwd=workspace, env=env, capture_output=True, text=True, timeout=timeout)
        (artifact / f"{name}.stdout").write_text(result.stdout)
        (artifact / f"{name}.stderr").write_text(result.stderr)
        require((result.returncode == 0) == expect_ok, f"{name} exit {result.returncode}: {result.stderr[-2000:]}")
        return result

    def pty_run(name, command, on_screen, deadline_s=45):
        """Drive the real terminal; on_screen(plain, write) returns True when done."""
        commands.append({"name": name, "command": shlex.join(command)})
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 180, 0, 0))
        child = subprocess.Popen(command, cwd=workspace, env=env, stdin=slave, stdout=slave, stderr=slave,
                                 start_new_session=True)
        os.close(slave)
        transcript, done = bytearray(), False
        deadline = time.monotonic() + deadline_s
        try:
            while time.monotonic() < deadline and not done:
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            break
                        raise
                    if not chunk:
                        break
                    transcript.extend(chunk)
                    if b"\x1b[6n" in chunk:
                        os.write(master, b"\x1b[1;1R")
                done = on_screen(ANSI.sub(b"", bytes(transcript)).decode(errors="replace"),
                                 lambda data: os.write(master, data))
                if child.poll() is not None:
                    done = done or on_screen(ANSI.sub(b"", bytes(transcript)).decode(errors="replace"), None)
                    break
            return done, child
        finally:
            # Leave the interactive session the way a user would, then make sure.
            for key in (b"\x04", b"\x03", b"\x03"):
                if child.poll() is not None:
                    break
                try:
                    os.write(master, key)
                except OSError:
                    break
                time.sleep(0.5)
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
            (artifact / f"{name}.pty.log").write_bytes(transcript)
            (artifact / f"{name}.terminal.txt").write_bytes(ANSI.sub(b"", bytes(transcript)))
            os.close(master)

    try:
        run("codex-run", [str(binary), "run", *codex, *shared, "--cwd", str(workspace), CODEX_PROMPT])
        time.sleep(1.1)  # distinct modification times keep the newest-first order observable
        run("claude-run", [str(binary), "run", "--claude", "--model", "claude-sonnet-5-5", *claude, *shared,
                           "--cwd", str(workspace), CLAUDE_PROMPT])
        require(len(providers.requests) == 2, f"expected one request per run: {len(providers.requests)}")
        checks.append("recorded one Codex and one Claude session with the shipped CLI")

        # Esc cancels the picker cleanly without contacting a provider.
        before = len(providers.requests)
        state = {"listed": False}

        def cancel(screen, write):
            if not state["listed"] and CODEX_PROMPT in screen and CLAUDE_PROMPT in screen:
                state["listed"] = True
                if write:
                    write(b"\x1b")
            return state["listed"] and write is None

        _, child = pty_run("picker-cancel", [str(binary), "resume", *both], cancel, deadline_s=30)
        require(state["listed"], "picker did not list the Codex and the Claude session together")
        require(len(providers.requests) == before, "cancelled picker contacted a provider")
        checks.append("one picker listed both families; Esc cancelled without provider contact")
        outcome["cancel_exit"] = child.returncode

        # Enter resumes the newest (Claude) session in the Claude harness.
        before = len(providers.requests)
        state = {"listed": False, "order_ok": None}

        def resume(screen, write):
            if not state["listed"] and CODEX_PROMPT in screen and CLAUDE_PROMPT in screen:
                state["listed"] = True
                state["order_ok"] = screen.rindex(CLAUDE_PROMPT) < screen.rindex(CODEX_PROMPT)
                if write:
                    write(b"\r")
            return "claude-picker-complete" in screen

        done, child = pty_run("picker-resume", [str(binary), "resume", *both, "--prompt", FOLLOWUP], resume)
        require(state["listed"], "picker did not list both sessions")
        require(state["order_ok"], "picker did not list the newer Claude session first")
        require(done, "the resumed session never rendered its reply; see picker-resume.terminal.txt")
        resumed = providers.requests[before:]
        require(resumed and all(r["path"].endswith("/messages") for r in resumed),
                f"selected Claude session did not resume in the Claude harness: {[r['path'] for r in resumed]}")
        history = json.dumps(resumed[0]["body"].get("messages", []))
        require(CLAUDE_PROMPT in history and FOLLOWUP in history, "resumed request lost the saved transcript")
        require(CODEX_PROMPT not in history, "resumed Claude session carried the Codex session's transcript")
        require(resumed[0]["body"]["model"] == "claude-sonnet-5-5", "resumed session lost its saved model")
        checks.append("Enter resumed the Claude session with its saved model and transcript")
        outcome["resume_exit"] = child.returncode

        # An unknown ID fails before contacting a provider.
        before = len(providers.requests)
        missing = run("resume-missing", [str(binary), "resume", "00000000-0000-7000-8000-000000000000", *both,
                                         "--prompt", "must-not-run"], expect_ok=False, timeout=20)
        require("unknown session" in missing.stderr or "no " in missing.stderr.lower(),
                f"unknown ID error is not explicit: {missing.stderr[-500:]}")
        require(len(providers.requests) == before, "unknown session contacted a provider")
        checks.append("unknown session ID failed explicitly before any provider request")
        outcome.update(success=True)
    except Exception as error:
        outcome.update(error=str(error))
        raise
    finally:
        providers.server.shutdown()
        (artifact / "provider.json").write_text(json.dumps(providers.requests, indent=2))
        (artifact / "outcome.json").write_text(json.dumps(outcome, indent=2))
        print(json.dumps({"artifact": str(artifact), **outcome}))


if __name__ == "__main__":
    main()
