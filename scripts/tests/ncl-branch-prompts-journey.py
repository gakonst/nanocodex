#!/usr/bin/env python3
"""Branch-navigator prompt identity journey (Claude) through the shipped ncl TUI.

python3 scripts/tests/ncl-branch-prompts-journey.py --binary target/debug/ncl

Edits prompts in the Ctrl+Alt+B navigator across repeated prompts, two long
prompts sharing their first 500 characters, nested branches, and a branch whose
history Claude auto-compaction pruned, then Before/Through branches made with
`ncl rewind` (including a media-only prompt) after their source moved on. A local Messages stub records every request, so
each edit is checked against the exact history the provider receives. Frames,
requests and the outcome are retained in ignored output/. Requires tmux.
"""
import argparse, base64, json, os, shlex, subprocess, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from uuid import uuid4

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", required=True, type=Path)
parser.add_argument("--output", type=Path, default=Path("output/ncl-branch-prompts") / uuid4().hex)
args = parser.parse_args()
art = args.output.absolute(); art.mkdir(parents=True, exist_ok=True)
binary = args.binary.absolute()
if binary.name != "ncl":
    (art / "bin").mkdir(exist_ok=True)
    alias = art / "bin" / "ncl"
    if not alias.exists():
        os.link(binary, alias)
    binary = alias
home, ws = art / "home", art / "ws"; home.mkdir(exist_ok=True); ws.mkdir(exist_ok=True)
checks, requests, frames = [], [], []
LONG = "LONG_PREFIX " + "x" * 520
L1, L2 = LONG + " LONG_ONE", LONG + " LONG_TWO"

def text_of(message):
    content = message.get("content")
    return content if isinstance(content, str) else " ".join(b.get("text", "") for b in content if isinstance(b, dict))

class Claude(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))))
        requests.append({"path": self.path, "body": body})
        last = next((text_of(m) for m in reversed(body.get("messages", [])) if m.get("role") == "user"), "")
        words = [w for w in last.replace("\n", " ").split(" ") if w.isupper() and "_" in w and w != "LONG_PREFIX"]
        reply = "REPLY_" + words[-1] if words else "SUMMARY_OF_EARLIER_WORK"
        events = [
            {"type": "message_start", "message": {"id": "msg_" + uuid4().hex, "type": "message", "role": "assistant",
             "model": body.get("model", "claude"), "content": [], "usage": {"input_tokens": 2_000_000 if "BIG_CONTEXT" in last else 10, "output_tokens": 0}}},
            {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": reply}},
            {"type": "content_block_stop", "index": 0},
            {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 10}},
            {"type": "message_stop"},
        ]
        payload = "".join("data: " + json.dumps(e) + "\n\n" for e in events).encode()
        self.send_response(200); self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(payload))); self.end_headers(); self.wfile.write(payload)
    def log_message(self, *a): pass

server = ThreadingHTTPServer(("127.0.0.1", 0), Claude)
threading.Thread(target=server.serve_forever, daemon=True).start()
env = {"PATH": "/usr/bin:/bin", "TERM": "xterm-256color", "NANOCODEX_COMPUTER": "off", "HOME": str(home), "CODEX_HOME": str(home)}
common = ["--claude", "--claude-api-key", "synthetic-claude-key", "--claude-messages-url",
          f"http://127.0.0.1:{server.server_address[1]}/v1/messages", "--browser=none",
          "--mcp-defaults", "false", "--mcp-codex-config", "false", "--web-search", "false",
          "--image-generation", "false", "--subagents", "false", "--memory", "false"]
S = f"ncl-branch-prompts-{uuid4().hex[:8]}"
def tmux(*a): return subprocess.run(["tmux", *a], capture_output=True, text=True)
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
    (art / "outcome.json").write_text(json.dumps({"checks": checks}, indent=2))
def composer_visible(s):
    lines = [line.strip() for line in s.splitlines()]
    tops = [i for i, line in enumerate(lines) if line.startswith("╭─") and "%/" in line]
    return bool(tops) and any(line.startswith("╰─") for line in lines[tops[-1] + 1:])
def row_text(line): return line.strip(" │").lstrip("›").strip()
def rows(s):
    # Numbered prompt rows of the navigator, in order.
    out = []
    for line in s.splitlines():
        head = row_text(line).split(". ", 1)
        if len(head) == 2 and head[0].isdigit():
            out.append((int(head[0]), head[1].strip(" │")))
    return out
def navigator(what):
    keys("C-M-b")
    wait(lambda s: "Prompts on this branch" in s, what, 20); time.sleep(1); s = screen()
    return s, rows(s)
def select(number, what):
    for _ in range(16):
        lines = screen().splitlines()
        marks = [i for i, line in enumerate(lines) if "│›" in line.replace(" ", "")]
        target = [i for i, line in enumerate(lines) if row_text(line).startswith(f"{number}. ")]
        if marks and target and marks[-1] == target[0]:
            return
        on_prompt = bool(marks) and row_text(lines[marks[-1]])[:1].isdigit()
        if marks and target and not on_prompt:
            keys("Tab")
        else:
            keys("Down" if marks and target and target[0] > marks[-1] else "Up")
        time.sleep(0.3)
    raise AssertionError(f"navigator never selected {what}")
def edit(number, old, new, marker):
    select(number, f"prompt {number}")
    keys("e"); time.sleep(0.5)
    for start in range(0, len(old), 100):
        keys(*(["BSpace"] * min(100, len(old) - start)))
    before = len(requests)
    typ(new); keys("Enter")
    end = time.monotonic() + 60
    while time.monotonic() < end:
        found = [r for r in requests[before:] if marker in json.dumps(r["body"].get("messages", []))]
        if found:
            wait(lambda s: "REPLY_" + marker in s, f"{marker} answer", 60)
            return user_prompts(found[0])
        time.sleep(0.3)
    raise AssertionError(f"no request for {marker}")
def user_prompts(request):
    # Claude prefixes a branch with its intended rewind notice; it is not a prompt.
    return [t for t in (text_of(m) for m in request["body"]["messages"] if m.get("role") == "user")
            if not t.startswith("This conversation was explicitly rewound")]
def submit(text, marker):
    typ(text); keys("Enter"); wait(lambda s: "REPLY_" + marker in s, f"{marker} answer", 60)
def short(r): return [(n, t[:40]) for n, t in r]
def brief(users): return [u[:40] + ("..." + u[-8:] if len(u) > 48 else "") for u in users]

PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+ip1sAAAAASUVORK5CYII="
def ncl(*argv, timeout=90):
    result = subprocess.run([str(binary), *argv], cwd=ws, env=env, capture_output=True, text=True, timeout=timeout)
    (art / "cli.log").open("a").write(json.dumps({"argv": list(argv), "code": result.returncode,
        "stdout": result.stdout[-4000:], "stderr": result.stderr[-4000:]}) + "\n")
    if result.returncode != 0:
        raise AssertionError(f"ncl {argv[:2]} exited {result.returncode}: {result.stderr[-600:]}")
    return result.stdout
def run_turn(state, request_id, prompt, marker, image=None):
    # One headless durable turn of a chosen session in the shared session store.
    before = len(requests)
    ncl("run", *common, "--rollouts", "false", "--cwd", str(ws), "--local-durability", str(home / "sessions.sqlite"),
        "--local-durability-state-id", state, "--request-id", request_id,
        *(["--image", str(image)] if image else []), prompt)
    found = [r for r in requests[before:] if marker in json.dumps(r["body"].get("messages", []))]
    if not found: raise AssertionError(f"no provider request for {marker}")
    return found[-1]
def has_image(message): return isinstance(message.get("content"), list) and any(b.get("type") == "image" for b in message["content"])
def rewind(source, *selection):
    out = json.loads(ncl("rewind", source, "--mode", "conversation", *selection, "--restore"))
    return out["branch_session"]
def boundary_phase():
    # Before/Through branches made with the public rewind CLI keep exactly the
    # source turns at their boundary: a media-only prompt included, and source
    # turns submitted after the branch never included.
    image = ws / "pixel.png"; image.write_bytes(base64.b64decode(PNG))
    src = "boundary-src-" + uuid4().hex[:8]
    run_turn(src, "b-one", "B_ONE", "B_ONE"); run_turn(src, "b-two", "B_TWO", "B_TWO")
    media = run_turn(src, "b-media", " ", "REPLY_B_TWO", image=image)
    run_turn(src, "b-four", "B_FOUR", "B_FOUR")
    preview = json.loads(ncl("rewind", src, "--mode", "conversation"))
    ids = [c["checkpoint"] for c in preview["checkpoints"]]
    check("source lists its 4 turns, media-only prompt with its image input and no text preview",
          ids == ["b-one", "b-two", "b-media", "b-four"] and preview["checkpoints"][2]["preview"] is None
          and "image" in json.dumps(preview["checkpoints"][2]["input"]) and has_image(media["body"]["messages"][-1]),
          checkpoints=[(c["checkpoint"], c["preview"]) for c in preview["checkpoints"]])
    through = rewind(src, "--through", "b-media")
    before = rewind(src, "--before", "b-media")
    run_turn(src, "b-five", "B_FIVE", "B_FIVE")
    for name, branch, marker, kept in [("through b-media", through, "T_NEXT", ["B_ONE", "B_TWO", "IMAGE"]),
                                       ("before b-media", before, "BF_NEXT", ["B_ONE", "B_TWO"])]:
        request = run_turn(branch, "next-" + marker, marker, marker)
        users = ["IMAGE" if has_image(m) else text_of(m) for m in request["body"]["messages"] if m.get("role") == "user"]
        users = [u for u in users if not u.startswith("This conversation was explicitly rewound")]
        check(f"{name} branch sends exactly its kept source turns, never later source turns",
              users == kept + [marker] and "B_FOUR" not in json.dumps(request["body"]) and "B_FIVE" not in json.dumps(request["body"]),
              users=brief(users))
    # The navigator of the Through branch, opened after the source moved on.
    keys("C-c"); time.sleep(0.5); keys("C-c"); time.sleep(1)
    tmux("respawn-pane", "-k", "-t", S + ":0.0", "env " + " ".join(shlex.quote(f"{k}={v}") for k, v in env.items()) + " "
         + shlex.join([str(binary), "resume", *common, "--cwd", str(ws), through]))
    wait(composer_visible, "resumed through branch", 40)
    s, r = navigator("through branch navigator")
    texts = [t for _, t in r]
    check("through branch navigator lists B_ONE,B_TWO,(media-only),T_NEXT and no later source turn",
          [n for n, _ in r][-1:] == [4] and texts[:2] == ["B_ONE", "B_TWO"] and texts[-1] == "T_NEXT"
          and "B_FOUR" not in s and "B_FIVE" not in s, rows=short(r))


cmd = "env " + " ".join(shlex.quote(f"{k}={v}") for k, v in env.items()) + " " + shlex.join([str(binary), *common, "--cwd", str(ws)])
tmux("new-session", "-d", "-x", "200", "-y", "60", "-s", S, "-c", str(ws), cmd + "; echo EXITED $?", ";", "set-option", "-t", S, "remain-on-exit", "on")
try:
    wait(composer_visible, "claude composer", 40)
    submit("SAME_PROMPT", "SAME_PROMPT"); submit("SAME_PROMPT", "SAME_PROMPT")
    submit(L1, "LONG_ONE"); submit(L2, "LONG_TWO")
    s, r = navigator("root navigator")
    check("root lists 4 prompts incl. duplicates and shared-prefix long prompts",
          [n for n, _ in r] == [1, 2, 3, 4] and r[0][1] == r[1][1] == "SAME_PROMPT" and all(t.startswith("LONG_PREFIX") for _, t in r[2:]), rows=short(r))
    users = edit(4, L2, "EDIT_FOUR", "EDIT_FOUR")
    check("editing long prompt 4 keeps exactly SAME,SAME,LONG_ONE",
          users[:-1] == ["SAME_PROMPT", "SAME_PROMPT", L1] and users[-1].endswith("EDIT_FOUR"), users=brief(users))
    s, r = navigator("branch 1 navigator")
    check("branch 1 lists SAME,SAME,LONG_ONE,EDIT_FOUR",
          [t[:11] for _, t in r] == ["SAME_PROMPT", "SAME_PROMPT", "LONG_PREFIX", "EDIT_FOUR"], rows=short(r))
    users = edit(2, "SAME_PROMPT", "EDIT_TWO", "EDIT_TWO")
    check("nested edit of inherited duplicate prompt 2 keeps one SAME", users == ["SAME_PROMPT", "EDIT_TWO"], users=brief(users))
    s, r = navigator("branch 2 navigator")
    check("branch 2 lists SAME,EDIT_TWO", [t for _, t in r] == ["SAME_PROMPT", "EDIT_TWO"], rows=short(r))
    keys("Escape"); time.sleep(0.5)
    submit("NEXT_THREE", "NEXT_THREE")
    s, r = navigator("branch 2 navigator after a turn")
    users = edit(3, "NEXT_THREE", "EDIT_THREE", "EDIT_THREE")
    check("third-level edit keeps SAME,EDIT_TWO", users == ["SAME_PROMPT", "EDIT_TWO", "EDIT_THREE"], users=brief(users))
    s, r = navigator("branch 3 navigator")
    check("branch 3 lists SAME,EDIT_TWO,EDIT_THREE", [t for _, t in r] == ["SAME_PROMPT", "EDIT_TWO", "EDIT_THREE"], rows=short(r))
    keys("Escape"); time.sleep(0.5)
    # A large reported context makes Claude auto-compact before the next turn,
    # pruning earlier prompts from the provider history (a public flow).
    submit("BIG_CONTEXT", "BIG_CONTEXT")
    before = len(requests)
    submit("AFTER_COMPACT", "AFTER_COMPACT")
    turn = [r for r in requests[before:] if "AFTER_COMPACT" in json.dumps(r["body"].get("messages", []))][-1]
    after = user_prompts(turn)
    check("auto-compaction pruned the earlier prompts from provider history",
          "SAME_PROMPT" not in json.dumps(turn["body"]["messages"]) and len(requests) - before >= 2,
          provider_users_after_compact=brief(after), requests_for_turn=len(requests) - before)
    expected = ["SAME_PROMPT", "EDIT_TWO", "EDIT_THREE", "BIG_CONTEXT", "AFTER_COMPACT"]
    s, r = navigator("compacted branch 3 navigator")
    check("compacted branch 3 still lists every prompt", [t for _, t in r] == expected, rows=short(r))
    if [t for _, t in r][-1:] == ["AFTER_COMPACT"]:
        users = edit(len(r), "AFTER_COMPACT", "EDIT_AFTER", "EDIT_AFTER")
        check("editing the newest prompt of the compacted branch keeps the prior prompts (summary)",
              "AFTER_COMPACT" not in json.dumps(users) and users[-1] == "EDIT_AFTER", users=brief(users))
        s, r2 = navigator("branch 4 navigator")
        check("branch 4 lists the prompts kept before EDIT_AFTER", [t for _, t in r2] == expected[:-1] + ["EDIT_AFTER"], rows=short(r2))
        if [t for _, t in r2][:2] == ["SAME_PROMPT", "EDIT_TWO"]:
            users = edit(2, "EDIT_TWO", "EDIT_PRUNED", "EDIT_PRUNED")
            check("editing an inherited prompt of a compacted lineage keeps exactly SAME", users == ["SAME_PROMPT", "EDIT_PRUNED"], users=brief(users))
    keys("Escape"); time.sleep(0.5)
    boundary_phase()
except Exception as error:
    check("journey", False, error=str(error))
finally:
    screen()
    tmux("kill-session", "-t", S)
    server.shutdown()
    (art / "requests.json").write_text(json.dumps(requests, indent=1)[:4000000])
    success = all(c["ok"] for c in checks)
    (art / "outcome.json").write_text(json.dumps({"success": success, "checks": checks}, indent=2))
    print(json.dumps({"success": success, "checks": [(c["check"], c["ok"]) for c in checks]}, indent=1))
    print(f"evidence: {art}")
    sys.exit(0 if success else 1)
