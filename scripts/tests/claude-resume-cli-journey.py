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
import hashlib
import json
from pathlib import Path
import re
import shlex
import sqlite3
import subprocess
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
    # Bare `nanocodex` is the managed client; the local tree is selected by name.
    ncl = artifact / "bin" / "ncl"
    ncl.parent.mkdir()
    ncl.symlink_to(binary)
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
    legacy_prompt = "Continue the current task from the interrupted response. legacy-prefix-marker"

    def user_texts(messages):
        texts = []
        for index, message in enumerate(messages):
            content = message["content"]
            if message["role"] == "user":
                texts.append((index, content if isinstance(content, str) else
                              "".join(b.get("text", "") for b in content if b.get("type") == "text")))
        return texts

    def steer_block(request, stage):
        """Repeated prompts, a steer typed while a cell runs, and legacy text."""
        messages = request["messages"]
        if stage == 0:
            require(user_texts(messages)[-1][1] == "repeat-marker-prompt", "first repeated prompt missing")
            code = 'const r = await tools.exec_command({cmd: "sleep 4; printf slept-marker"}); text(JSON.stringify(r));'
            return {"type": "tool_use", "id": "steer_0", "name": "exec", "input": {"code": code}}
        if stage == 1:
            receipt_at = next(i for i, m in enumerate(messages) if isinstance(m["content"], list)
                              and any(b.get("tool_use_id") == "steer_0" for b in m["content"]))
            steer_at = [i for i, text in user_texts(messages) if "steer-marker-text" in text]
            require(len(steer_at) == 1 and steer_at[0] >= receipt_at, f"steer not delivered once after the receipt: {steer_at}")
            checks.append("steer: typed while the cell ran, delivered once after its receipt")
            return {"type": "text", "text": "steer-first-complete"}
        if stage == 2:
            repeated = [i for i, text in user_texts(messages) if text == "repeat-marker-prompt"]
            require(len(repeated) == 2, f"repeated prompt not admitted twice: {repeated}")
            return {"type": "text", "text": "steer-repeat-complete"}
        if stage == 3:
            require(user_texts(messages)[-1][1] == legacy_prompt, "legacy-prefixed user prompt altered")
            return {"type": "text", "text": "steer-legacy-complete"}
        raise AssertionError("unexpected steer provider retry")

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
                elif name not in ("initial", "steer") and stage == 0:
                    for marker in ("original-resume-prompt", "initial-resume-complete", "committed-shell-once"):
                        require(marker in history, f"{name} lost prior transcript marker {marker}")
                    if name == "picker":
                        require("explicit-resume-complete" in history, "picker lost explicit resume transcript")
                    checks.append(name + ": prior transcript and saved model restored")
                if stage and name not in ("paste", "steer"):
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
                if name == "steer":
                    block = steer_block(request, stage)
                elif stage < len(steps[name]):
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

    frames = {}

    def tmux(*argv):
        return subprocess.run(["tmux", *argv], capture_output=True, text=True)

    def start(name, command, env):
        """Run the CLI in a real tmux terminal; frames are the rendered screen."""
        record(name, command, env)
        session = f"claude-resume-{name}-{uuid4().hex[:8]}"
        # The pane's shell records the CLI's exit status itself. tmux 3.4 can
        # mark a pane dead without ever rendering "Pane is dead" (CI run
        # 38022367862: pane_dead=1, empty status, CLI left <defunct>).
        exit_path = artifact / f"{name}.exit"
        exit_path.unlink(missing_ok=True)
        shell = ("env -i " + " ".join(shlex.quote(f"{k}={v}") for k, v in env.items()) + " " + shlex.join(command)
                 + "; echo $? > " + shlex.quote(str(exit_path)))
        tmux("new-session", "-d", "-x", "170", "-y", "80", "-s", session, "-c", str(launch), shell,
             ";", "set-option", "-t", session, "remain-on-exit", "on")
        return session

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
                raise AssertionError(f"{name}: timed out; see {name}.frames.txt\n" + timeout_evidence(session, screen))
            time.sleep(0.4)

    def timeout_evidence(session, screen):
        """Inline diagnostics: CI does not upload output/claude-resume-cli."""
        parts = ["--- last frame ---", "\n".join(line.rstrip() for line in screen.splitlines() if line.strip())[-4000:]]
        panes = tmux("list-panes", "-t", session, "-F", "#{pane_pid} dead=#{pane_dead} status=#{pane_dead_status}")
        parts += ["--- pane ---", (panes.stdout + panes.stderr).strip()]
        pid = panes.stdout.split(" ", 1)[0].strip()
        if pid.isdigit():
            tree = subprocess.run(["ps", "-o", "pid,ppid,stat,wchan:24,etime,args", "--forest", "-s", pid], capture_output=True, text=True)
            parts += ["--- processes ---", tree.stdout.strip()]
        logs = sorted((home / ".local/state/nanocodex/logs").glob("tui-*.log"), key=lambda path: path.stat().st_mtime)
        if logs:
            text = re.sub(r"\x1b\[[0-9;]*m", "", logs[-1].read_text(errors="replace"))
            parts += [f"--- {logs[-1].name} (tail) ---", "\n".join(line[:400] for line in text.splitlines()[-40:])]
        return "\n".join(parts)

    def close(name, session):
        # Ctrl+C asks for confirmation; a second Ctrl+C quits.
        tmux("send-keys", "-t", session + ":0.0", "C-c")
        time.sleep(0.3)
        tmux("send-keys", "-t", session + ":0.0", "C-c")
        # The pane's shell writes the CLI's own exit status once it returns.
        exit_path = artifact / f"{name}.exit"
        began = time.monotonic()
        screen = screen_until(name, session, lambda screen: exit_path.exists() and exit_path.read_text().strip() != "", 60)
        checks.append(f"{name}: exited {time.monotonic() - began:.1f}s after Ctrl+C")
        tmux("kill-session", "-t", session)
        status = exit_path.read_text().strip()
        require(status == "0", f"{name} did not exit cleanly (status {status}): {screen.strip()[-200:]}")

    def run_pty(name, command, env, picker=False, session_id=None):
        session = start(name, command, env)
        try:
            if picker:
                # The unified picker lists both harnesses; the row shows the
                # saved prompt preview, harness and (width-truncated) ID.
                screen_until(name, session, lambda screen: "Resume a thread" in screen
                             and "original-resume-prompt" in screen and "· claude ·" in screen
                             and session_id[:8] in screen)
                checks.append("picker displayed the saved Claude session and its first prompt")
                tmux("send-keys", "-t", session + ":0.0", "Enter")
            screen = screen_until(name, session, lambda screen: f"{name}-resume-complete" in screen)
            require("original-resume-prompt" in screen and "initial-resume-complete" in screen,
                    f"{name}: resumed terminal does not show the saved transcript")
            checks.append(f"{name}: resumed terminal replays the saved transcript")
            close(name, session)
        finally:
            tmux("kill-session", "-t", session)

    outcome = {"success": False, "boundary": "actual native CLI, native default journal, real PTY input and HTTP/SSE; external model only is synthetic"}
    try:
        outcome["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
        milestone("Started normal persistence + explicit resume + picker journey.")
        initial = [str(ncl), "run", *common, "--model", "claude-sonnet-5-5", "--thinking", "medium", "--cwd", str(workspace), "original-resume-prompt"]
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
            command = [str(ncl), "resume", *([session_id] if name == "explicit" else []), *common,
                       "--prompt", f"{name}-followup-prompt"]
            run_pty(name, command, resume_env, picker=name == "picker", session_id=session_id)
            require(not errors, "; ".join(errors))
            require(len(requests) - phase["start"] == len(steps[name]) + 1, f"unexpected {name} provider count")
            require((workspace / "counter.txt").read_text() == "x", "committed shell repeated on resume")
            require(not (launch / "counter.txt").exists(), "resume used launch directory")
            milestone(f"Process {name} passed; saved model/workspace/transcript and task watermark retained; shell not replayed.")
        # Rich replay: paste an image into the real TUI, run nested and failing
        # Code Mode cells, then replay the checkpoint in a fresh process.
        image_path = artifact / "pasted.png"
        image_path.write_bytes(png)
        def tui(name, wait_for):
            session = start(name, [str(ncl), "resume", session_id, *common], resume_env)
            return session, screen_until(name, session, wait_for)

        def turn_region(screen):
            lines = screen.splitlines()
            start = max(i for i, line in enumerate(lines) if "after-image-marker" in line)
            return lines[start:]

        def tool_rows(region):
            rows = [re.search(r"([✓×◌◇?]) (Tools|Code|Shell)  (\S.*?)(?:\s{2,}|$)", line) for line in region]
            return [(row[1], row[2], row[3].strip()) for row in rows if row]

        def prompt_row(screen):
            return re.search(r"before-image-marker\s+\[Image #1\]\s+after-image-marker", screen)

        def submit(target, text):
            tmux("send-keys", "-t", target, "-l", text)
            tmux("send-keys", "-t", target, "Enter")

        phase.update(name="steer", start=len(requests))
        session, _ = tui("steer", lambda screen: "picker-resume-complete" in screen)
        target = session + ":0.0"
        submit(target, "repeat-marker-prompt")
        deadline = time.monotonic() + 20
        while len(requests) - phase["start"] < 1 and time.monotonic() < deadline:
            time.sleep(0.2)
        time.sleep(1.5)  # The cell's nested shell is now sleeping.
        submit(target, "steer-marker-text")
        screen_until("steer", session, lambda screen: "steer-first-complete" in screen)
        submit(target, "repeat-marker-prompt")
        screen_until("steer", session, lambda screen: "steer-repeat-complete" in screen)
        submit(target, legacy_prompt)
        steer_live = screen_until("steer", session, lambda screen: "steer-legacy-complete" in screen)
        close("steer", session)
        require(len(requests) - phase["start"] == 4, f"unexpected steer provider count {len(requests) - phase['start']}")
        milestone("Process steer passed; repeated prompts, a mid-cell steer and legacy-prefixed text admitted.")

        phase.update(name="paste", start=len(requests))
        session, _ = tui("paste", lambda screen: "steer-legacy-complete" in screen)
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
        # The turn's work is one "Tools" workflow: the cell's two nested shells,
        # then the throwing cell.
        def workflow(rows):
            header = [row for row in rows if row[1] == "Tools"]
            require(len(header) == 1 and header[0][2].startswith("3 calls") and "1 failed" in header[0][2],
                    f"workflow header lacks 3 calls with 1 failure: {rows}")
            return [row for row in rows if row[1] != "Tools"]

        live_cells = workflow(live_rows)
        require([row[:2] for row in live_cells] == [("✓", "Shell"), ("✓", "Shell"), ("×", "Code")],
                f"live cards differ from two nested shells and a failed cell: {live_rows}")
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
        replay_cells = workflow(rows)
        require([row[1] for row in replay_cells] == [row[1] for row in live_cells] and replay_cells[-1][0] == "×",
                f"replayed tool outcomes differ: {rows} vs {live_rows}")
        # Nested receipts retain status, not shell output: an exit status that
        # was never recorded must replay as outcome unknown (?), never as success.
        require(all(r[0] in (l[0], "?") for r, l in zip(replay_cells[:2], live_cells[:2])),
                f"replayed nested shells differ: {replay_cells} vs {live_cells}")
        require(not any("Tools " in line and ("__CLI_NESTED_RESULT__" in line or '{"' in line)
                        for line in replay.splitlines()), "raw structured receipt in replayed workflow header")
        for leaked in ("data:image", "base64", "Harness recovery notice", "Historical context"):
            require(leaked not in replay, f"replay shows internal or private text: {leaked}")
        # User rows: each admitted prompt once, the steer once and in order, and
        # user text that resembles an old harness instruction is kept.
        for screen, label in ((steer_live, "live"), (replay, "replay")):
            first = screen.find("repeat-marker-prompt")
            require(screen.count("repeat-marker-prompt") == 2 and screen.count("steer-marker-text") == 1
                    and first < screen.find("steer-marker-text") < screen.rfind("repeat-marker-prompt"),
                    f"{label}: repeated prompts/steer rows wrong; see frames")
            require(screen.count("legacy-prefix-marker") == 1
                    and "Continue the current task from the interrupted response." in screen,
                    f"{label}: legacy-prefixed user prompt missing")
        checks.append("replay: one image prompt row, actual completed/failed outcomes and 2 nested calls, no internal text")
        milestone("Process replay passed; resumed prompt, outcomes and nested counts match the live screen.")
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
                command = [str(ncl), "resume", *extra, *common, "--prompt", "must-not-contact-provider"]
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
