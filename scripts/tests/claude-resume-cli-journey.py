#!/usr/bin/env python3
"""Real native Claude resume across processes, including the terminal picker.

Build separately, then run (the rich replay phases require tmux):
  python3 scripts/tests/claude-resume-cli-journey.py --binary target/debug/nanocodex
Only the external Messages HTTP/SSE provider is synthetic. Evidence: ignored output/.

The last phases paste an image into a real `ncl --claude` TUI, run Code Mode
cells with nested and failing tools, then resume in a fresh process and compare
the replayed prompt row and tool cards with the live screen.
"""
from claude_code_fixture import normalize_request, wrap_tool
import argparse
import base64
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
                    ("exec_command", {"cmd": "printf x >> counter.txt; printf committed-shell-once"})],
        "explicit": [("TaskGet", {"taskId": "1"}),
                     ("TaskCreate", {"subject": "After explicit resume", "description": "Check saved ID watermark"}),
                     ("Read", {"file_path": "workspace-marker.txt"})],
        "picker": [("TaskGet", {"taskId": "2"}),
                   ("TaskCreate", {"subject": "After picker resume", "description": "Check second restart watermark"}),
                   ("Read", {"file_path": "counter.txt"})],
    }
    nested_code = ('const a = await tools.exec_command({cmd: "printf nested-one-marker"});\n'
                   'const b = await tools.exec_command({cmd: "printf nested-two-marker"});\n'
                   'text("nested-done:" + JSON.stringify(a).includes("nested-one-marker") + JSON.stringify(b).includes("nested-two-marker"));')
    steps["paste"] = [("exec", {"code": nested_code}),
                      ("exec", {"code": 'throw new Error("intentional-failure-marker");'})]
    png = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==")
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
                if name == "paste":
                    require("data:image" not in history and "[Image #" not in history,
                            "image reached the model as text instead of a native block")
                if name == "paste" and stage == 0:
                    prompt = request["messages"][-1]
                    blocks = prompt["content"] if isinstance(prompt["content"], list) else []
                    images = [b for b in blocks if b.get("type") == "image"]
                    texts = "".join(b.get("text", "") for b in blocks if b.get("type") == "text")
                    require(prompt["role"] == "user" and len(images) == 1, f"pasted prompt lacks one native image: {prompt}")
                    require(images[0]["source"]["type"] == "base64" and images[0]["source"]["media_type"] == "image/png",
                            f"unexpected image source {images[0]['source'].get('type')}")
                    require(base64.b64decode(images[0]["source"]["data"])[:8] == png[:8], "image bytes are not the pasted PNG")
                    require(blocks.index(images[0]) not in (0, len(blocks) - 1)
                            and texts.split() == ["before-image-marker", "after-image-marker"],
                            f"image not between its caption parts: {texts!r}")
                    checks.append("paste: one user message with a native base64 PNG between its text parts")
                if name == "paste" and stage:
                    call_id = f"paste_{stage - 1}"
                    receipts = [b for m in request["messages"] for b in m.get("content", [])
                                if isinstance(b, dict) and b.get("type") == "tool_result" and b.get("tool_use_id") == call_id]
                    require(len(receipts) == 1, f"missing/duplicate receipt {call_id}")
                    output, failed = text_of(receipts[0]), receipts[0].get("is_error", False)
                    if stage == 1:
                        require(not failed and "nested-done:truetrue" in output, f"nested cell failed: {receipts[0]}")
                    else:
                        require(failed and "intentional-failure-marker" in output, f"throwing cell not failed: {receipts[0]}")
                    checks.append(f"paste: actual {'failed' if failed else 'completed'} receipt for {call_id}")
                elif name != "initial" and stage == 0:
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
                    if prior_tool == "exec_command":
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
                    if not selected and b"Resume a Claude session" in transcript and session_id.encode() in transcript:
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
        manifests = list((home / "codex/claude/sessions").glob("*.json"))
        require(len(manifests) == 1, "normal run must register exactly one native session")
        manifest = json.loads(manifests[0].read_text())
        session_id = manifest["id"]
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
        # Rich replay: paste an image into the real TUI, run nested and failing
        # Code Mode cells, then replay the checkpoint in a fresh process.
        ncl = artifact / "bin" / "ncl"
        ncl.parent.mkdir()
        ncl.symlink_to(binary)  # The local command tree is selected by name.
        image_path = artifact / "pasted.png"
        image_path.write_bytes(png)
        frames = {}

        def tmux(*argv):
            return subprocess.run(["tmux", *argv], capture_output=True, text=True)

        def tui(name, wait_for):
            session = f"claude-resume-{name}-{uuid4().hex[:8]}"
            command = [str(ncl), "resume", session_id, *common]
            record(name, command, resume_env)
            shell = "env -i " + " ".join(shlex.quote(f"{k}={v}") for k, v in resume_env.items()) + " " + shlex.join(command)
            tmux("new-session", "-d", "-x", "170", "-y", "60", "-s", session, "-c", str(launch), shell + "; echo EXITED $?",
                 ";", "set-option", "-t", session, "remain-on-exit", "on")
            return session, screen_until(name, session, wait_for)

        def screen_until(name, session, predicate, timeout=40):
            deadline = time.monotonic() + timeout
            while True:
                screen = tmux("capture-pane", "-p", "-t", session + ":0.0").stdout
                frames.setdefault(name, []).append(screen)
                (artifact / f"{name}.frames.txt").write_text("\n=====FRAME=====\n".join(frames[name]))
                if errors:
                    raise AssertionError("; ".join(errors))
                if predicate(screen):
                    return screen
                if time.monotonic() > deadline:
                    raise AssertionError(f"{name}: timed out; see {name}.frames.txt")
                time.sleep(0.4)

        def close(name, session):
            tmux("send-keys", "-t", session + ":0.0", "C-d")
            screen = screen_until(name, session, lambda screen: "EXITED" in screen, 15)
            tmux("kill-session", "-t", session)
            require("EXITED 0" in screen, f"{name} did not exit cleanly")

        def turn_region(screen):
            lines = screen.splitlines()
            start = max(i for i, line in enumerate(lines) if "after-image-marker" in line)
            return lines[start:]

        def tool_rows(region):
            rows = [re.search(r"([✓×◌◇?]) (Batch|Code|Shell)  (\S.*?)(?:\s{2,}|$)", line) for line in region]
            return [(row[1], row[2], row[3].strip()) for row in rows if row]

        def prompt_row(screen):
            return re.search(r"before-image-marker\s+\[Image #1\]\s+after-image-marker", screen)

        phase.update(name="paste", start=len(requests))
        session, _ = tui("paste", lambda screen: "paste-resume-complete" not in screen and "picker-resume-complete" in screen)
        target = session + ":0.0"
        tmux("send-keys", "-t", target, "-l", "before-image-marker ")
        tmux("set-buffer", "-b", "a54-image", str(image_path))
        tmux("paste-buffer", "-p", "-d", "-b", "a54-image", "-t", target)
        screen_until("paste", session, lambda screen: "[Image #1]" in screen, 15)
        tmux("send-keys", "-t", target, "-l", " after-image-marker")
        tmux("send-keys", "-t", target, "Enter")
        live = screen_until("paste", session, lambda screen: "paste-resume-complete" in screen)
        close("paste", session)
        require(len(requests) - phase["start"] == 3, f"unexpected paste provider count {len(requests) - phase['start']}")
        live_rows = tool_rows(turn_region(live))
        require(prompt_row(live), "live prompt row lacks its image placeholder")
        cards = [row for row in live_rows if row[1] != "Shell"]
        require([row[:2] for row in cards] == [("✓", "Batch"), ("×", "Code")],
                f"live cards differ from a 2-call batch and a failed cell: {live_rows}")
        milestone("Process paste passed; native image block, nested batch and failed cell rendered live.")

        phase.update(name="replay", start=len(requests))
        session, replay = tui("replay", lambda screen: "paste-resume-complete" in screen)
        close("replay", session)
        require(len(requests) == phase["start"], "replay contacted the provider")
        region = turn_region(replay)
        rows = tool_rows(region)
        (artifact / "replay-comparison.json").write_text(json.dumps({"live": live_rows, "replay": rows}, indent=2))
        require(replay.count("after-image-marker") == 1 and prompt_row(replay),
                "replayed image prompt is missing its placeholder or split into extra user rows")
        replay_cards = [row for row in rows if row[1] != "Shell"]
        require([row[:2] for row in replay_cards] == [row[:2] for row in cards],
                f"replayed tool outcomes differ: {rows} vs {live_rows}")
        require(replay_cards[0][2] == cards[0][2] == "2 tools", f"replayed batch lost its nested calls: {rows}")
        # Nested receipts retain status, not shell output: an exit status that
        # was never recorded must replay as outcome unknown (?), never as success.
        shells, live_shells = [r for r in rows if r[1] == "Shell"], [r for r in live_rows if r[1] == "Shell"]
        require(len(shells) == len(live_shells) and all(r[0] in (l[0], "?") for r, l in zip(shells, live_shells)),
                f"replayed nested shells differ: {shells} vs {live_shells}")
        for leaked in ("data:image", "base64", "Harness recovery notice", "Continue the current task",
                       "Historical context", "Host Stop hook"):
            require(leaked not in replay, f"replay shows internal or private text: {leaked}")
        checks.append("replay: one image prompt row, actual completed/failed outcomes and 2 nested calls, no internal text")
        milestone("Process replay passed; resumed prompt, outcomes and nested counts match the live screen.")
        # Public error paths; no journal fabrication or private-state mutation.
        for name, extra, expected in (
            ("missing-session", ["absent-session-id"], "unknown session"),
            ("workspace-mismatch", [session_id, "--cwd", str(launch)], "--cwd requested"),
            ("persistence-disabled", [session_id, "--rollouts", "false"], "requires native persistence"),
            ("deleted-workspace", [session_id], "failed to resolve the resumed Claude workspace"),
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
