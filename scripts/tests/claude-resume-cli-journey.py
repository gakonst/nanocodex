#!/usr/bin/env python3
"""Real native Claude resume across processes, including the terminal picker.

Build separately, then run:
  python3 scripts/tests/claude-resume-cli-journey.py --binary target/debug/nanocodex
Only the external Messages HTTP/SSE provider is synthetic. Evidence: ignored output/.
"""
from claude_code_fixture import normalize_request, wrap_tool
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
import sqlite3
import struct
import subprocess
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def text_of(block):
    content = block.get("content", "")
    return content if isinstance(content, str) else "\n".join(
        item.get("text", "") for item in content if item.get("type") == "text")


def sse(block, model):
    tool = block["type"] == "tool_use"
    content = dict(block)
    delta = ({"type": "input_json_delta", "partial_json": json.dumps(content.pop("input"))}
             if tool else {"type": "text_delta", "text": content.pop("text")})
    content["input" if tool else "text"] = {} if tool else ""
    events = [
        {"type": "message_start", "message": {"id": "msg_" + uuid4().hex, "type": "message", "role": "assistant", "model": model, "content": [], "usage": {"input_tokens": 10, "output_tokens": 0}}},
        {"type": "content_block_start", "index": 0, "content_block": content},
        {"type": "content_block_delta", "index": 0, "delta": delta},
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": "tool_use" if tool else "end_turn"}, "usage": {"output_tokens": 10}},
        {"type": "message_stop"},
    ]
    return "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("output/claude-resume-cli") / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    artifact.mkdir(parents=True)
    workspace, launch, home = (artifact / name for name in ("workspace", "launch-elsewhere", "home"))
    for path in (workspace, launch, home):
        path.mkdir()
    (workspace / "workspace-marker.txt").write_text("saved-workspace-visible")
    environment = {"HOME": str(home), "CODEX_HOME": str(home / "codex"),
                   "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "TERM": "xterm-256color",
                   "NANOCODEX_COMPUTER": "off"}
    requests, errors, commands, checks = [], [], [], []
    phase = {"name": "initial", "start": 0}
    steps = {
        "initial": [("TaskCreate", {"subject": "Resume durable task", "description": "Preserve task state between native processes"}),
                    ("TaskUpdate", {"taskId": "1", "status": "in_progress"}),
                    ("Bash", {"command": "printf x >> counter.txt; printf committed-shell-once"})],
        "explicit": [("TaskGet", {"taskId": "1"}),
                     ("TaskCreate", {"subject": "After explicit resume", "description": "Check saved ID watermark"}),
                     ("Read", {"file_path": "workspace-marker.txt"})],
        "picker": [("TaskGet", {"taskId": "2"}),
                   ("TaskCreate", {"subject": "After picker resume", "description": "Check second restart watermark"}),
                   ("Read", {"file_path": "counter.txt"})],
    }
    progress = Path("output/claude-resume-progress.md")

    def milestone(message):
        with progress.open("a") as stream:
            stream.write(f"\n- {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())} `{artifact}`: {message}\n")

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["content-length"]))); request = normalize_request(request, artifact)
            name, stage = phase["name"], len(requests) - phase["start"]
            requests.append({"phase": name, "request": request})
            try:
                require(self.path == "/v1/messages", f"unexpected route {self.path}")
                require(self.headers.get("x-api-key") == "synthetic-resume-key", "wrong provider authentication")
                require(request["model"] == "claude-sonnet-5-5", f"saved model lost: {request['model']}")
                history = json.dumps(request["messages"])
                if name != "initial" and stage == 0:
                    for marker in ("original-resume-prompt", "initial-resume-complete", "committed-shell-once"):
                        require(marker in history, f"{name} lost prior transcript marker {marker}")
                    if name == "picker":
                        require("explicit-resume-complete" in history, "picker lost explicit resume transcript")
                    checks.append(name + ": prior transcript and saved model restored")
                if stage:
                    call_id = f"{name}_{stage - 1}"
                    receipts = [b for m in request["messages"] for b in m.get("content", [])
                                if isinstance(b, dict) and b.get("type") == "tool_result" and b.get("tool_use_id") == call_id]
                    require(len(receipts) == 1, f"missing/duplicate receipt {call_id}")
                    receipt = receipts[0]
                    require(not receipt.get("is_error", False), f"tool failed: {receipt}")
                    output = text_of(receipt)
                    prior_tool = steps[name][stage - 1][0]
                    if prior_tool == "TaskUpdate":
                        update = json.loads(output)
                        require(update["success"] and update["statusChange"]["to"] == "in_progress", "TaskUpdate failed")
                    if prior_tool in ("TaskCreate", "TaskGet"):
                        task = json.loads(output)["task"]
                        expected = "1" if name == "initial" or (name == "explicit" and stage == 1) else "2" if name == "explicit" or stage == 1 else "3"
                        require(task["id"] == expected, f"wrong task watermark: {task}")
                        if name == "explicit" and stage == 1:
                            require(task["status"] == "in_progress", f"task status not restored: {task}")
                    if prior_tool == "Bash":
                        require("committed-shell-once" in output, "missing committed shell receipt")
                    if prior_tool == "Read":
                        marker = "saved-workspace-visible" if name == "explicit" else "x"
                        require(marker in output, f"saved workspace Read failed: {output}")
                if stage < len(steps[name]):
                    tool, arguments = steps[name][stage]
                    block = {"type": "tool_use", "id": f"{name}_{stage}", "name": tool, "input": arguments}
                else:
                    require(stage == len(steps[name]), "unexpected provider retry")
                    block = {"type": "text", "text": f"{name}-resume-complete"}
            except Exception as error:
                errors.append(str(error))
                block = {"type": "text", "text": "resume-fixture-assertion-failed"}
            (artifact / "provider.json").write_text(json.dumps(requests, indent=2))
            response = sse(wrap_tool(block), request["model"])
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(response)))
            self.end_headers()
            self.wfile.write(response)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    common = ["--claude", "--claude-api-key", "synthetic-resume-key", "--claude-messages-url",
              f"http://127.0.0.1:{server.server_port}/v1/messages", "--browser=none", "--mcp-defaults", "false",
              "--mcp-codex-config", "false", "--web-search", "false", "--image-generation", "false",
              "--subagents", "false", "--memory", "false"]

    def record(name, command, env):
        commands.append({"phase": name, "command": command, "shell_command": shlex.join(command),
                         "cwd": str(launch), "environment": env})
        (artifact / "commands.json").write_text(json.dumps(commands, indent=2))

    def run_pty(name, command, env, picker=False, session_id=None):
        record(name, command, env)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 180, 0, 0))
        child = subprocess.Popen(command, cwd=launch, env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        transcript = bytearray()
        selected, done, sent_exit = not picker, False, 0
        deadline = time.monotonic() + 40
        try:
            while time.monotonic() < deadline:
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
                    (artifact / f"{name}.pty.log").write_bytes(transcript)
                    if b"\x1b[6n" in chunk:
                        os.write(master, b"\x1b[1;1R")
                    if not selected and b"resumable sessions" in transcript and session_id.encode() in transcript:
                        checks.append("picker displayed the saved session ID")
                        os.write(master, b"\r")
                        selected = True
                    if f"{name}-resume-complete".encode() in transcript:
                        done = True
                    if b"resume-fixture-assertion-failed" in transcript:
                        raise AssertionError("; ".join(errors))
                if done and time.monotonic() - sent_exit > 0.5:
                    os.write(master, b"\x04")
                    sent_exit = time.monotonic()
                if child.poll() is not None:
                    break
            require(selected, "picker never displayed/selectable saved session")
            require(done, f"{name} terminal never rendered final response; see PTY transcript")
            if child.poll() is None and done:
                child.wait(timeout=5)  # PTY EOF can precede the process exit notification.
            require(child.poll() is not None, f"{name} did not exit on Ctrl-D")
            require(child.returncode == 0, f"{name} exit {child.returncode}")
        finally:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
            (artifact / f"{name}.pty.log").write_bytes(transcript)
            plain = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(transcript))
            (artifact / f"{name}.terminal.txt").write_bytes(plain)
            os.close(master)

    outcome = {"success": False, "boundary": "actual native CLI, native default journal, real PTY input and HTTP/SSE; external model only is synthetic"}
    try:
        outcome["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
        milestone("Started normal persistence + explicit resume + picker journey.")
        initial = [str(binary), "run", *common, "--model", "claude-sonnet-5-5", "--thinking", "medium", "--cwd", str(workspace), "original-resume-prompt"]
        record("initial", initial, environment)
        result = subprocess.run(initial, cwd=launch, env=environment, capture_output=True, timeout=40)
        (artifact / "initial.jsonl").write_bytes(result.stdout)
        (artifact / "initial.stderr.log").write_bytes(result.stderr)
        require(result.returncode == 0, f"initial exit {result.returncode}: {result.stderr.decode(errors='replace')}")
        require(not errors, "; ".join(errors))
        require(b"initial-resume-complete" in result.stdout, "initial final answer missing")
        require((workspace / "counter.txt").read_text() == "x", "initial shell counter incorrect")
        # The shared durable store is the session authority for every harness.
        store = sqlite3.connect(f"file:{home / 'codex/sessions.sqlite'}?mode=ro", uri=True)
        try:
            heads = store.execute("SELECT state_id, payload FROM nanocodex_durable_states").fetchall()
        finally:
            store.close()
        require(len(heads) == 1, "normal run must record exactly one durable session")
        record_head = json.loads(heads[0][1])["nanocodex_session"]
        manifest = {"id": record_head["session_id"], "workspace": record_head.get("workspace"), "model": record_head["model"]}
        session_id = manifest["id"]
        require(session_id == heads[0][0], "session record identity differs from its state")
        (artifact / "session-manifest.json").write_text(json.dumps(manifest, indent=2))
        require(manifest["workspace"] == str(workspace), "saved workspace mismatch")
        require(manifest["model"] == "claude-sonnet-5-5", "saved model mismatch")
        milestone(f"Process A passed; session {session_id}, task 1 in_progress, shell counter x.")
        resume_env = {**environment, "ANTHROPIC_MODEL": "claude-opus-5-5"}
        for name in ("explicit", "picker"):
            phase.update(name=name, start=len(requests))
            command = [str(binary), "resume", *([session_id] if name == "explicit" else []), *common,
                       "--prompt", f"{name}-followup-prompt"]
            run_pty(name, command, resume_env, picker=name == "picker", session_id=session_id)
            require(not errors, "; ".join(errors))
            require(len(requests) - phase["start"] == len(steps[name]) + 1, f"unexpected {name} provider count")
            require((workspace / "counter.txt").read_text() == "x", "committed shell repeated on resume")
            require(not (launch / "counter.txt").exists(), "resume used launch directory")
            milestone(f"Process {name} passed; saved model/workspace/transcript and task watermark retained; shell not replayed.")
        # Public error paths; no journal fabrication or private-state mutation.
        for name, extra, expected in (
            ("missing-session", ["absent-session-id"], "unknown session"),
            ("workspace-mismatch", [session_id, "--cwd", str(launch)], "--cwd requested"),
            ("persistence-disabled", [session_id, "--rollouts", "false"], "requires session persistence"),
            ("deleted-workspace", [session_id], "failed to resolve the resumed workspace"),
        ):
            moved = artifact / "workspace-temporarily-moved"
            if name == "deleted-workspace":
                workspace.rename(moved)
            try:
                command = [str(binary), "resume", *extra, *common, "--prompt", "must-not-contact-provider"]
                record(name, command, resume_env)
                before = len(requests)
                result = subprocess.run(command, cwd=launch, env=resume_env, capture_output=True, timeout=10)
                (artifact / f"{name}.stdout.log").write_bytes(result.stdout)
                (artifact / f"{name}.stderr.log").write_bytes(result.stderr)
                require(result.returncode != 0, f"{name} unexpectedly succeeded")
                require(expected in result.stderr.decode(errors="replace"), f"{name} wrong error: {result.stderr!r}")
                require(len(requests) == before, f"{name} contacted provider")
                checks.append(name + ": rejected before provider")
            finally:
                if name == "deleted-workspace":
                    moved.rename(workspace)
        outcome.update(success=True, session_id=session_id, provider_requests=len(requests), shell_effect_count=1,
                       saved_model="claude-sonnet-5-5", restored_task_status="in_progress", next_task_ids=["2", "3"], checks=checks,
                       limitations=["Foreign journal rejection not exercised: no foreign-provider journal generated in this native-only journey."])
        milestone("All three processes and four error paths passed.")
    except Exception as error:
        outcome.update(error=str(error), checks=checks, provider_errors=errors)
        milestone("FAILED: " + str(error))
        raise
    finally:
        (artifact / "outcome.json").write_text(json.dumps(outcome, indent=2))
        server.shutdown()
        print(json.dumps({"artifact": str(artifact), **outcome}))


if __name__ == "__main__":
    main()
