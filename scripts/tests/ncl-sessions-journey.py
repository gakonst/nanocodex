#!/usr/bin/env python3
"""Local (ncl) sessions journey through the shipped CLI in a real tmux PTY.

python3 scripts/tests/ncl-sessions-journey.py --binary target/debug/ncl

Covers: resume replay, the native tui-control registration and rollout history
paging, /attach, the branch navigator (edit prompt 2 into a forked branch and
switch back), /btw, /collapse, /split and /close. A local Responses server
records every model request. Synthetic homes, requests, screen frames and the
outcome are retained in ignored output/. Requires tmux.
"""
import argparse, glob, json, os, shlex, socket, subprocess, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from uuid import uuid4

os.umask(0o022)
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", required=True, type=Path)
parser.add_argument("--output", type=Path, default=Path("output/ncl-sessions") / uuid4().hex)
args = parser.parse_args()
art = args.output.absolute(); art.mkdir(parents=True, exist_ok=True)
binary = args.binary.absolute()
if binary.name != "ncl":
    # The local command tree is selected by the invoked name.
    (art / "bin").mkdir(exist_ok=True)
    alias = art / "bin" / "ncl"
    if not alias.exists():
        os.link(binary, alias)
    binary = alias
home, ws = art / "home", art / "ws"; home.mkdir(exist_ok=True); ws.mkdir(exist_ok=True)
checks, requests = [], []

def sse(text):
    ev = [{"type": "response.created", "response": {"id": "r"}},
          {"type": "response.output_item.done", "item": {"type": "message", "role": "assistant", "id": "m" + str(len(requests)),
           "content": [{"type": "output_text", "text": text}]}},
          {"type": "response.completed", "response": {"id": "r", "usage": {"input_tokens": 1, "input_tokens_details": None,
           "output_tokens": 1, "output_tokens_details": None, "total_tokens": 2}}}]
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in ev).encode()

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))))
        requests.append(body)
        last = ""
        for item in reversed(body.get("input", [])):
            if item.get("role") == "user":
                last = " ".join(p.get("text", "") for p in item.get("content", []) if isinstance(p, dict)); break
        word = next((w for w in last.replace("\n", " ").split(" ") if w.isupper() and "_" in w), "UNKNOWN")
        if "SLOW_PROMPT" in last:
            # Keeps the main turn running long enough to collapse a /btw into it.
            time.sleep(8)
        payload = sse("ANSWER_" + word)
        self.send_response(200); self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(payload))); self.end_headers(); self.wfile.write(payload)
    def log_message(self, *a): pass

server = ThreadingHTTPServer(("127.0.0.1", 0), H)
threading.Thread(target=server.serve_forever, daemon=True).start()
url = f"http://127.0.0.1:{server.server_address[1]}/v1"
common = ["--api-key", "synthetic-test-key", "--api-base-url", url, "--responses-transport", "https", "--browser=none",
          "--mcp-defaults", "false", "--web-search", "false", "--image-generation", "false"]
env = {"PATH": "/usr/bin:/bin", "TERM": "xterm-256color", "NANOCODEX_COMPUTER": "off", "HOME": str(home), "CODEX_HOME": str(home)}
S = f"ncl-sessions-{uuid4().hex[:8]}"
frames = []

def tmux(*a, **k):
    return subprocess.run(["tmux", *a], capture_output=True, text=True, **k)

def screen():
    out = tmux("capture-pane", "-p", "-t", S + ":0.0").stdout
    frames.append(out); (art / "frames.txt").write_text("\n=====FRAME=====\n".join(frames)); return out

def wait(pred, what, timeout=40):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        s = screen()
        if pred(s): return s
        time.sleep(0.5)
    raise AssertionError(f"timed out: {what}")

def keys(*k): tmux("send-keys", "-t", S + ":0.0", *k)
def typ(t): tmux("send-keys", "-t", S + ":0.0", "-l", t)
def check(name, ok, **detail):
    checks.append({"check": name, "ok": bool(ok), **detail})
    (art / "outcome.json").write_text(json.dumps({"checks": checks, "requests": len(requests)}, indent=2))
    if not ok: raise AssertionError(name)

def rollouts(): return sorted((home / "sessions").rglob("rollout-*.jsonl"))
def completed(p): return sum(1 for l in p.read_text().splitlines() if '"task_complete"' in l)

try:
    r = subprocess.run([str(binary), "run", *common, "--cwd", str(ws), "--repeat", "2", "FIRST_PROMPT"], cwd=ws, env=env,
                       capture_output=True, text=True, timeout=90)
    (art / "run.stderr.txt").write_text(r.stderr)
    check("run records a 2-turn thread", r.returncode == 0 and len(rollouts()) == 1 and completed(rollouts()[0]) == 2)
    original = rollouts()[0]
    thread = json.loads(original.read_text().splitlines()[0])["payload"]["id"]
    tmux("kill-session", "-t", S)
    cmd = "env " + " ".join(shlex.quote(f"{k}={v}") for k, v in env.items()) + " " + shlex.join([str(binary), "resume", thread, *common])
    # remain-on-exit keeps a crashed TUI's last screen inspectable until cleanup.
    tmux("new-session", "-d", "-x", "170", "-y", "50", "-s", S, "-c", str(ws), cmd + "; echo EXITED $?",
         ";", "set-option", "-t", S, "remain-on-exit", "on")
    for k, v in env.items(): tmux("set-environment", "-t", S, k, v)
    s = wait(lambda s: s.count("ANSWER_FIRST_PROMPT") >= 2, "resume replays both turns")
    check("resume replays history", True, answers=s.count("ANSWER_FIRST_PROMPT"))

    # Native tui-control: registration kind and rollout history paging.
    regs = [json.loads(Path(p).read_text()) for p in glob.glob(str(home / "nanocodex/tui/instances/*.json"))]
    reg = next(r for r in regs if r.get("active_session_id"))
    (art / "registration.kind.txt").write_text(json.dumps({k: v for k, v in reg.items() if k != "auth_token"}, indent=2))
    check("control registers kind native", reg.get("backend") == "native" and bool((reg.get("conversation") or {}).get("rollout_path")), backend=reg.get("backend"))
    sock = socket.socket(socket.AF_UNIX); sock.connect(reg["socket_path"]); f = sock.makefile("rw")
    f.write(json.dumps({"protocol_version": 1, "instance_id": reg["instance_id"], "auth_token": reg["auth_token"]}) + "\n"); f.flush()
    hello = json.loads(f.readline()); n = [0]
    def req(method, params):
        n[0] += 1; rid = f"j{n[0]}"; f.write(json.dumps({"id": rid, "method": method, "params": params}) + "\n"); f.flush()
        while True:
            fr = json.loads(f.readline())
            if fr.get("id") == rid: return fr["result"]
    session = reg["active_session_id"]
    p1 = req("history.list", {"expected_session_id": session, "limit": 3})
    p2 = req("history.list", {"expected_session_id": session, "limit": 3, "cursor": p1["next_cursor"], "boundary": p1["boundary"]})
    again = req("history.list", {"expected_session_id": session, "limit": 3, "boundary": p1["boundary"]})
    ids1 = [x["record_id"] for x in p1["records"]]; ids2 = [x["record_id"] for x in p2["records"]]
    (art / "history.json").write_text(json.dumps({"hello": hello, "page1": p1, "page2": p2}, indent=2)[:200000])
    check("history.list pages backwards with stable cursors", ids1 and ids2 and not set(ids1) & set(ids2)
          and [x["record_id"] for x in again["records"]] == ids1, page1=ids1, page2=ids2)
    one = req("history.read", {"expected_session_id": session, "record_id": ids1[0], "boundary": p1["boundary"]})
    check("history.read returns a record", "status" not in one or one.get("status") != "rejected", keys=list(one)[:6])
    stale = req("history.list", {"expected_session_id": "not-this-session", "limit": 1})
    check("history.list rejects a stale session", "session_changed" in json.dumps(stale), result=stale)

    # /attach opens the local session picker.
    typ("/attach"); keys("Enter")
    time.sleep(3); s = screen(); keys("Escape"); time.sleep(1)
    check("/attach lists local sessions", "FIRST_PROMPT" in s and "Could not load" not in s)

    # Branch navigator: edit prompt 2 into a new branch forked after turn 1.
    keys("C-M-b")
    s = wait(lambda s: "Prompts on this branch" in s, "navigator opens", 15)
    check("Ctrl+Alt+B opens the navigator with both prompts", "1. FIRST_PROMPT" in s and "2. FIRST_PROMPT" in s)
    keys("e"); time.sleep(0.5)
    for _ in range(len("FIRST_PROMPT")): keys("BSpace")
    typ("EDITED_PROMPT"); keys("Enter")
    end = time.monotonic() + 40
    while time.monotonic() < end and len(rollouts()) < 2: time.sleep(0.5)
    forks = [p for p in rollouts() if p != original]
    check("edit forks the rollout after turn 1", len(forks) == 1 and completed(forks[0]) >= 1, forks=[str(p) for p in forks])
    fork_thread = json.loads(forks[0].read_text().splitlines()[0])["payload"]["id"]
    s = wait(lambda s: s.count("ANSWER_FIRST_PROMPT") == 1, "branch transcript shows one turn", 40)
    check("branch shows history before the edited prompt", True)
    time.sleep(5); s = screen()
    check("edited prompt submitted on the branch", "ANSWER_EDITED_PROMPT" in s or any("EDITED_PROMPT" in json.dumps(b.get("input", [])) for b in requests))
finally:
    try:
        keys("C-M-b"); time.sleep(2); s = screen(); keys("Up"); keys("Enter")
        s = wait(lambda s: s.count("ANSWER_FIRST_PROMPT") >= 2, "switch back to main", 40)
        checks.append({"check": "switch back to the original branch", "ok": True})
    except Exception as error:
        checks.append({"check": "switch back to the original branch", "ok": False, "error": str(error)})
    try:
        typ("/btw SIDE_QUESTION"); keys("Enter")
        s = wait(lambda s: "ANSWER_SIDE_QUESTION" in s, "btw answer", 40)
        side = [b for b in requests if "SIDE_QUESTION" in json.dumps(b.get("input", []))]
        checks.append({"check": "/btw answers from the forked main snapshot", "ok": bool(side) and "FIRST_PROMPT" in json.dumps(side[-1].get("input", []))})
        before = len(requests)
        typ("/collapse"); keys("Enter")
        end = time.monotonic() + 40
        while time.monotonic() < end and not any("side exploration" in json.dumps(b.get("input", [])) for b in requests[before:]):
            time.sleep(0.5)
        time.sleep(2); s = screen()
        collapsed = [b for b in requests[before:] if "side exploration" in json.dumps(b.get("input", []))
                     or ("<btw_conversation>" in json.dumps(b.get("input", [])) and "ANSWER_SIDE_QUESTION" in json.dumps(b.get("input", [])))]
        checks.append({"check": "/collapse hands the btw thread to main and closes its pane",
                       "ok": bool(collapsed) and ("Collapsed /btw" in s or "BTW Codex thread ID" in s) and "\u203a BTW" not in s,
                       "mode": "inline" if collapsed and "<btw_conversation>" in json.dumps(collapsed[0].get("input", [])) else "thread"})
        typ("/btw SPLIT_QUESTION"); keys("Enter")
        wait(lambda s: "ANSWER_SPLIT_QUESTION" in s, "split btw answer", 40)
        typ("/split"); keys("Enter")
        end = time.monotonic() + 15
        panes, refusal = "", ""
        while time.monotonic() < end:
            panes = tmux("list-panes", "-t", S, "-F", "#{pane_index} #{pane_start_command}").stdout
            s = screen()
            if panes.count("\n") >= 2: break
            if "not saved to disk" in s:
                refusal = "not saved to disk"; break
            time.sleep(0.3)
        time.sleep(2)
        split_screen = tmux("capture-pane", "-p", "-t", S + ":0.1").stdout if panes.count("\n") >= 2 else ""
        (art / "split-pane.txt").write_text(panes + "\n----\n" + split_screen)
        # Legacy parity: a fork without its own rollout cannot be resumed elsewhere and /split says so.
        checks.append({"check": "/split opens a resuming tmux pane, or refuses an unsaved fork like legacy",
                       "ok": ("resume" in panes and panes.count("\n") >= 2) or bool(refusal), "refusal": refusal})
        if panes.count("\n") < 2:
            typ("/close"); keys("Enter"); time.sleep(1)
        tmux("kill-pane", "-t", S + ":0.1")
        typ("/btw CLOSE_QUESTION"); keys("Enter")
        wait(lambda s: "ANSWER_CLOSE_QUESTION" in s, "close btw answer", 40)
        typ("/close"); keys("Enter"); time.sleep(3); s = screen()
        before = len(requests)
        typ("AFTER_CLOSE_PROMPT"); keys("Enter")
        wait(lambda s: "ANSWER_AFTER_CLOSE_PROMPT" in s, "main prompt after /close", 40)
        after = [b for b in requests[before:]]
        checks.append({"check": "/close closes the side pane; main keeps working", "ok": len(after) == 1 and "CLOSE_QUESTION" not in json.dumps(after[0].get("input", [])[-1:])})
        # Legacy parity: /collapse while main is running steers the side exchange into that turn.
        typ("SLOW_PROMPT"); keys("Enter")
        end = time.monotonic() + 20
        while time.monotonic() < end and not any("SLOW_PROMPT" in json.dumps(r.get("input", [])[-1:]) for r in requests):
            time.sleep(0.2)
        typ("/btw BUSY_SIDE"); keys("Enter")
        wait(lambda s: "ANSWER_BUSY_SIDE" in s, "btw answer while main runs", 40)
        before = len(requests)
        typ("/collapse"); keys("Enter"); time.sleep(0.5); s = screen()
        end = time.monotonic() + 40
        steered = []
        while time.monotonic() < end and not steered:
            steered = [r for r in requests[before:] if "BUSY_SIDE" in json.dumps(r.get("input", [])) and "<btw_conversation>" in json.dumps(r.get("input", []))]
            time.sleep(0.5)
        s = wait(lambda s: "ANSWER_SLOW_PROMPT" in s, "slow main turn finishes", 40)
        checks.append({"check": "/collapse while main runs steers the side exchange into that turn",
                       "ok": bool(steered) and "SLOW_PROMPT" in json.dumps(steered[0].get("input", [])) and "not collapsed" not in s})
    except Exception as error:
        checks.append({"check": "btw/split/close", "ok": False, "error": str(error)})

    # Claude parity: legacy edited a Claude session's first prompt as a fresh session,
    # refused later prompts, and switched back to the original conversation.
    claude_requests, claude_frames = [], []
    S2 = S + "-claude"
    claude_server = None
    try:
        class Claude(BaseHTTPRequestHandler):
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))))
                claude_requests.append({"path": self.path, "body": body})
                last = ""
                for message in reversed(body.get("messages", [])):
                    if message.get("role") == "user":
                        content = message.get("content")
                        last = content if isinstance(content, str) else " ".join(
                            b.get("text", "") for b in content if isinstance(b, dict))
                        break
                marker = next((m for m in ("CLAUDE_CLEARED", "CLAUDE_EDITED", "CLAUDE_SECOND", "CLAUDE_FIRST") if m in last), "UNKNOWN")
                events = [
                    {"type": "message_start", "message": {"id": "msg_" + uuid4().hex, "type": "message", "role": "assistant",
                     "model": body.get("model", "claude"), "content": [], "usage": {"input_tokens": 10, "output_tokens": 0}}},
                    {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                    {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "REPLY_" + marker}},
                    {"type": "content_block_stop", "index": 0},
                    {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 10}},
                    {"type": "message_stop"},
                ]
                payload = "".join("data: " + json.dumps(e) + "\n\n" for e in events).encode()
                self.send_response(200); self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(payload))); self.end_headers(); self.wfile.write(payload)
            def log_message(self, *a): pass

        claude_server = ThreadingHTTPServer(("127.0.0.1", 0), Claude)
        threading.Thread(target=claude_server.serve_forever, daemon=True).start()
        chome, cws = art / "claude-home", art / "claude-ws"; chome.mkdir(exist_ok=True); cws.mkdir(exist_ok=True)
        cenv = {**env, "HOME": str(chome), "CODEX_HOME": str(chome)}
        ccommon = ["--claude", "--claude-api-key", "synthetic-claude-key", "--claude-messages-url",
                   f"http://127.0.0.1:{claude_server.server_address[1]}/v1/messages", "--browser=none",
                   "--mcp-defaults", "false", "--mcp-codex-config", "false", "--web-search", "false",
                   "--image-generation", "false", "--subagents", "false", "--memory", "false"]
        ccmd = "env " + " ".join(shlex.quote(f"{k}={v}") for k, v in cenv.items()) + " " + shlex.join([str(binary), *ccommon, "--cwd", str(cws)])
        tmux("new-session", "-d", "-x", "170", "-y", "50", "-s", S2, "-c", str(cws), ccmd + "; echo EXITED $?",
             ";", "set-option", "-t", S2, "remain-on-exit", "on")
        def cscreen():
            out = tmux("capture-pane", "-p", "-t", S2 + ":0.0").stdout
            claude_frames.append(out); (art / "claude-frames.txt").write_text("\n=====FRAME=====\n".join(claude_frames)); return out
        def cwait(pred, what, timeout=40):
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                s = cscreen()
                if pred(s): return s
                time.sleep(0.5)
            raise AssertionError(f"timed out: {what}")
        def ckeys(*k): tmux("send-keys", "-t", S2 + ":0.0", *k)
        def ctyp(t): tmux("send-keys", "-t", S2 + ":0.0", "-l", t)
        # Long workspace paths can displace the optional Enter-send footer hint.
        def composer_visible(screen):
            lines = [line.strip() for line in screen.splitlines()]
            tops = [i for i, line in enumerate(lines) if line.startswith("╭─") and "%/" in line]
            return bool(tops) and any(line.startswith("╰─") for line in lines[tops[-1] + 1:])
        def composer_text(screen):
            lines = screen.splitlines()
            tops = [i for i, line in enumerate(lines) if line.strip().startswith("╭─") and "%/" in line]
            return "\n".join(lines[tops[-1]:]) if tops else ""
        cwait(composer_visible, "claude composer", 40)
        ctyp("CLAUDE_FIRST"); ckeys("Enter"); cwait(lambda s: "REPLY_CLAUDE_FIRST" in s, "claude first answer")
        ctyp("CLAUDE_SECOND"); ckeys("Enter"); cwait(lambda s: "REPLY_CLAUDE_SECOND" in s, "claude second answer")
        ckeys("C-M-b")
        s = cwait(lambda s: "Prompts on this branch" in s, "claude navigator", 20)
        checks.append({"check": "Claude navigator lists the journal's prompts", "ok": "1. CLAUDE_FIRST" in s and "2. CLAUDE_SECOND" in s})
        ckeys("e"); time.sleep(0.5)
        for _ in range(len("CLAUDE_SECOND")): ckeys("BSpace")
        ctyp("CLAUDE_LATER"); ckeys("Enter"); time.sleep(1); s = cscreen()
        checks.append({"check": "Claude later-prompt edit is refused like legacy", "ok": "ncl rewind" in s})
        ckeys("Up"); ckeys("e"); time.sleep(0.5)
        for _ in range(len("CLAUDE_FIRST")): ckeys("BSpace")
        before = len(claude_requests)
        ctyp("CLAUDE_EDITED"); ckeys("Enter")
        s = cwait(lambda s: "REPLY_CLAUDE_EDITED" in s, "edited first prompt answer", 60)
        edited = [r for r in claude_requests[before:] if "CLAUDE_EDITED" in json.dumps(r["body"].get("messages", []))]
        history = json.dumps(edited[0]["body"].get("messages", [])) if edited else ""
        checks.append({"check": "Claude first-prompt edit starts a fresh session with only the edited prompt",
                       "ok": bool(edited) and "CLAUDE_FIRST" not in history and "CLAUDE_SECOND" not in history
                       and "REPLY_CLAUDE_SECOND" not in s})
        ckeys("C-M-b"); s = cwait(lambda s: "Branches" in s and "(current)" in s, "claude branches", 20)
        ckeys("Up"); ckeys("Enter")
        s = cwait(lambda s: "REPLY_CLAUDE_FIRST" in s and "REPLY_CLAUDE_SECOND" in s, "switch back to the original Claude session", 60)
        checks.append({"check": "switch back to the original Claude session replays it", "ok": "REPLY_CLAUDE_EDITED" not in s})
        # The reopened session keeps Claude's resolved launch: its footer shows the
        # Claude model, and /clear starts a fresh Claude (not Codex) session.
        checks.append({"check": "switched-back Claude session shows its Claude model",
                       "ok": "claude-opus-5-5" in composer_text(s) and "gpt-" not in s})
        ctyp("/clear"); ckeys("Enter")
        cwait(lambda s: composer_visible(s) and "REPLY_CLAUDE_FIRST" not in s, "cleared Claude session", 30)
        before = len(claude_requests)
        ctyp("CLAUDE_CLEARED"); ckeys("Enter")
        s = cwait(lambda s: "REPLY_CLAUDE_CLEARED" in s, "cleared Claude session answer", 60)
        cleared = [r for r in claude_requests[before:] if "CLAUDE_CLEARED" in json.dumps(r["body"].get("messages", []))]
        history = json.dumps(cleared[0]["body"].get("messages", [])) if cleared else ""
        checks.append({"check": "/clear after switching back starts a fresh Claude session",
                       "ok": bool(cleared) and "CLAUDE_FIRST" not in history
                       and "claude-opus-5-5" in composer_text(s) and "gpt-" not in s})
    except Exception as error:
        checks.append({"check": "Claude branch editing", "ok": False, "error": str(error)})
    finally:
        tmux("kill-session", "-t", S2)
        if claude_server:
            claude_server.shutdown()
        (art / "claude-requests.json").write_text(json.dumps(claude_requests, indent=1)[:2000000])
    screen()
    (art / "requests.json").write_text(json.dumps(requests, indent=1)[:2000000])
    (art / "outcome.json").write_text(json.dumps({"success": all(c["ok"] for c in checks), "checks": checks, "requests": len(requests)}, indent=2))
    tmux("kill-session", "-t", S)
    server.shutdown()
    success = all(c["ok"] for c in checks)
    print(json.dumps({"success": success, "checks": len(checks)}))
    print(f"ncl sessions evidence: {art}")
    sys.exit(0 if success else 1)
