#!/usr/bin/env python3
"""CODEX_THREAD_ID and NANOCODEX_ROOT_SESSION_ID through the shipped local CLI.

Build separately, then run:
  python3 scripts/tests/session-identity-cli-journey.py --binary target/debug/ncl
(the local CLI tree; an ncl hard link to the nanocodex binary selects it)

The CLI is launched with spoofed CODEX_THREAD_ID / NANOCODEX_ROOT_SESSION_ID,
as if from inside another agent's shell. For each family (Codex, Claude):
  root    ncl run: Code Mode exec runs exec_command and a stdio MCP tool, then
          spawn_agent + wait_agent; the child runs the same probes.
  branch  ncl rewind ROOT --through 1 --restore, then ncl resume BRANCH in a
          PTY runs the probes again.
Every shell and MCP process must see its own session as CODEX_THREAD_ID and the
root session as NANOCODEX_ROOT_SESSION_ID, never the spoofed values. Claude
command hooks (--claude-hooks) must see the same identity as the hook payload.
Only the HTTP model providers are synthetic; identities come from the public
JSONL stream, rewind output and the probes themselves.
Evidence: ignored output/session-identity-cli/<run>/.
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
import shutil
import struct
import subprocess
import sys
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

ROOT_PROMPT = "IDENTITY_ROOT_PROMPT"
BRANCH_PROMPT = "IDENTITY_BRANCH_PROMPT"
SPOOF = {"CODEX_THREAD_ID": "spoofed-parent-thread", "NANOCODEX_ROOT_SESSION_ID": "spoofed-parent-root"}
DOLLAR = chr(36)
PROBE = ('printf "IDS<%s|%s>" "' + DOLLAR + '{CODEX_THREAD_ID-unset}" "'
         + DOLLAR + '{NANOCODEX_ROOT_SESSION_ID-unset}"')
LABELLED = re.compile(r'(SHELL|MCP|META)<([^|<>\\"]*)\|([^<>\\"]*)>')
ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]|\x1b[()][0-9A-B]|\x1b[=>]")
REPO = Path(__file__).resolve().parents[2]
FIXTURE = REPO / "crates/nanocodex-oai-tools/tests/fixtures/mcp-stdio-server.mjs"

# Code Mode source run by the synthetic model. Labels are built by
# concatenation so the source itself never matches LABELLED in history.
PROBES = r'''
const names = ALL_TOOLS.map(t => t.name);
const shell = await tools.exec_command({cmd: %s});
const ids = JSON.stringify(shell).match(/IDS<([^|>]*)\|([^>]*)>/);
text("SHE" + "LL<" + (ids ? ids[1] + "|" + ids[2] : "nomatch|" + JSON.stringify(shell).slice(0, 200).replace(/[<>|]/g, " ")) + ">");
if (names.includes("WaitForMcpServers")) await tools.WaitForMcpServers({});
const echo = names.find(n => /fixture/.test(n) && /echo$/.test(n));
if (!echo) throw new Error("no fixture MCP tool in " + JSON.stringify(names));
const mcp = JSON.stringify(await tools[echo]({message: "__environment__"}));
const env = mcp.match(/CODEX_THREAD_ID\W+([0-9A-Za-z-]+)\W+NANOCODEX_ROOT_SESSION_ID\W+([0-9A-Za-z-]+)/);
text("MC" + "P<" + (env ? env[1] + "|" + env[2] : "nomatch|" + mcp.slice(0, 200).replace(/[<>|]/g, " ")) + ">");
const meta = JSON.stringify(await tools[echo]({message: "__metadata__"}));
const call = meta.match(/thread_id\W+([0-9A-Za-z-]+)/), owner = meta.match(/session_id\W+([0-9A-Za-z-]+)/);
// Codex sends x-codex-turn-metadata {thread_id, session_id}; Claude sends
// nanocodex/invocation {session_id}. Both must name the calling session.
text("ME" + "TA<" + (call ? call[1] : owner ? owner[1] : "none") + "|" + (owner ? owner[1] : "none") + ">");
text("METARAW " + meta.slice(0, 400).replace(/[<>|]/g, " "));
''' % json.dumps(PROBE)
SPAWN = r'''
const spawned = await tools.spawn_agent({role: "identity child", task: "CHILD_" + "IDENTITY_TASK: run the probes", output_contract: {kind: "string"}});
const child = Number(JSON.stringify(spawned).match(/agent_id\D+(\d+)/)[1]);
const waited = await tools.wait_agent({agent_ids: [child], timeout_ms: 90000});
text("WAITED " + JSON.stringify(waited).slice(0, 600));
'''
SUBMIT = 'text("SUBMITTED " + JSON.stringify(await tools.submit_result({output: "child probes done"})));'
HEADER = '// @exec: {"yield_time_ms":120000}\n'


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def labelled(text):
    """Newest SHELL/MCP probe results in a serialized tool output."""
    found = {}
    for kind, thread, root in LABELLED.findall(text):
        found[kind] = (thread, root)
    return found


class Provider:
    """Synthetic Responses + Messages endpoints driving a scripted Code Mode plan."""

    def __init__(self, artifact):
        self.requests, self.errors = [], []
        self.artifact = artifact
        self.lock = threading.Lock()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_POST(self):  # noqa: N802
                body = json.loads(self.rfile.read(int(self.headers.get("content-length", "0"))))
                claude = self.path.endswith("/messages")
                try:
                    step = owner.plan(body, claude)
                except Exception as error:  # surfaced by the journey
                    owner.errors.append(repr(error))
                    step = ("text", "fixture-error")
                payload = owner.claude_sse(step, body["model"]) if claude else owner.responses_sse(step)
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"

    @staticmethod
    def tool_outputs(body, claude):
        if claude:
            return [b for m in body.get("messages", []) if isinstance(m.get("content"), list)
                    for b in m["content"] if isinstance(b, dict) and b.get("type") == "tool_result"]
        return [i for i in body.get("input", []) if i.get("type") in ("custom_tool_call_output", "function_call_output")]

    def plan(self, body, claude):
        text = json.dumps(body)
        role = "child" if ROOT_PROMPT not in text else "branch" if BRANCH_PROMPT in text else "root"
        outputs = self.tool_outputs(body, claude)
        # History comes first: the newest output belongs to this agent's current turn.
        current = json.dumps(outputs[-1]) if outputs else ""
        with self.lock:
            self.requests.append({"role": role, "family": "claude" if claude else "codex",
                                  "probes": labelled(current), "body": body})
            (self.artifact / "provider.json").write_text(json.dumps(self.requests, indent=2))
        attempts = sum(1 for r in self.requests if r["role"] == role and r["family"] == self.requests[-1]["family"])
        if attempts > 8:
            raise AssertionError(f"{role} kept retrying; see provider.json")
        if role == "root":
            return ("text", "IDENTITY_ROOT_DONE") if "WAITED" in current else ("exec", PROBES + SPAWN)
        if role == "branch":
            # The branch keeps the root's first turn, whose outputs also carry probes.
            return ("text", "IDENTITY_BRANCH_DONE") if self.branch_probed(body, claude) else ("exec", PROBES)
        if "SUBMITTED" in current:
            return ("text", "IDENTITY_CHILD_DONE")
        return ("exec", SUBMIT) if "SHE" "LL<" in current else ("exec", PROBES)

    def branch_probed(self, body, claude):
        """Whether a tool output follows the branch prompt in this request."""
        text = json.dumps(body.get("messages") if claude else body.get("input"))
        tail = text.rsplit(BRANCH_PROMPT, 1)[-1]
        return "SHE" "LL<" in tail and "MC" "P<" in tail

    @staticmethod
    def claude_sse(step, model):
        kind, value = step
        if kind == "exec":
            block = {"type": "tool_use", "id": "toolu_" + uuid4().hex[:12], "name": "exec", "input": {}}
            delta = {"type": "input_json_delta", "partial_json": json.dumps({"code": HEADER + value})}
        else:
            block = {"type": "text", "text": ""}
            delta = {"type": "text_delta", "text": value}
        events = [
            {"type": "message_start", "message": {"id": "msg_" + uuid4().hex, "type": "message", "role": "assistant",
                                                  "model": model, "content": [], "usage": {"input_tokens": 1, "output_tokens": 0}}},
            {"type": "content_block_start", "index": 0, "content_block": block},
            {"type": "content_block_delta", "index": 0, "delta": delta},
            {"type": "content_block_stop", "index": 0},
            {"type": "message_delta", "delta": {"stop_reason": "tool_use" if kind == "exec" else "end_turn"},
             "usage": {"output_tokens": 1}},
            {"type": "message_stop"},
        ]
        return "".join("data: " + json.dumps(e) + "\n\n" for e in events).encode()

    @staticmethod
    def responses_sse(step):
        kind, value = step
        ident = "call_" + uuid4().hex[:12]
        if kind == "exec":
            output = {"type": "custom_tool_call", "name": "exec", "call_id": ident, "input": HEADER + value}
        else:
            output = {"type": "message", "role": "assistant", "id": "msg_" + ident,
                      "content": [{"type": "output_text", "text": value}]}
        event = {"type": "response.completed", "response": {"id": "resp_" + ident, "status": "completed", "output": [output],
                 "usage": {"input_tokens": 1, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 1,
                           "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 2}}}
        return ("data: " + json.dumps(event) + "\n\n").encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("output/session-identity-cli") / uuid4().hex)
    parser.add_argument("--family", choices=["codex", "claude", "both"], default="both")
    args = parser.parse_args()
    binary, artifact = args.binary.absolute(), args.output.resolve()
    artifact.mkdir(parents=True)
    provider = Provider(artifact)
    commands, checks, failures = [], [], []
    outcome = {"success": False, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
               "spoofed_environment": SPOOF, "checks": checks, "failures": failures, "commands": commands}
    node = shutil.which("node")
    require(node, "node is required for the stdio MCP fixture")

    def expect(label, observed, thread, root):
        # Collected, so every scenario reports its own evidence.
        if observed is None:
            failures.append(f"{label}: no probe result")
        elif observed != (thread, root):
            failures.append(f"{label}: CODEX_THREAD_ID|NANOCODEX_ROOT_SESSION_ID = {observed[0]}|{observed[1]}, "
                            f"expected {thread}|{root}")
        else:
            checks.append(f"{label}: {thread}|{root}")

    families = ["codex", "claude"] if args.family == "both" else [args.family]
    try:
        for family in families:
            directory = artifact / family
            home, workspace = directory / "home", directory / "workspace"
            home.mkdir(parents=True)
            workspace.mkdir()
            hook = directory / "hook.py"
            hook.write_text("import json, os, sys\n"
                            "p = json.load(sys.stdin)\n"
                            f"with open({str(directory / 'hooks.jsonl')!r}, 'a') as f:\n"
                            "    f.write(json.dumps({'event': p.get('hook_event_name'), 'session_id': p.get('session_id'),"
                            " 'thread': os.environ.get('CODEX_THREAD_ID'),"
                            " 'root': os.environ.get('NANOCODEX_ROOT_SESSION_ID')}) + '\\n')\n"
                            "print('{}')\n")
            hooks = directory / "hooks.json"
            hooks.write_text(json.dumps({"hooks": {"PreToolUse": [{"matcher": "^exec_command$", "hooks": [
                {"type": "command", "command": f"{shlex.quote(sys.executable)} {shlex.quote(str(hook))}"}]}]}}))
            env = {"HOME": str(home), "PATH": os.environ.get("PATH", "/usr/bin:/bin"), "TERM": "xterm-256color",
                   "NANOCODEX_COMPUTER": "off", "NANOCODEX_LINK_HOMES": "false", **SPOOF}
            flags = ["--browser=none", "--mcp-defaults", "false", "--mcp-codex-config", "false", "--web-search", "false",
                     "--image-generation", "false", "--memory", "false", "--subagents", "true",
                     "--mcp-stdio", f"fixture={node}", "--mcp-arg", f"fixture={FIXTURE}",
                     "--api-key", "synthetic-test-key", "--api-base-url", provider.base + "/v1",
                     "--responses-transport", "https", "--claude-api-key", "synthetic-test-key",
                     "--claude-messages-url", provider.base + "/v1/messages"]
            if family == "claude":
                flags += ["--claude-hooks", str(hooks)]
            model = "claude-sonnet-5-5" if family == "claude" else "gpt-6.1-sol"

            def run(name, command, expect_ok=True, timeout=180):
                commands.append({"name": name, "command": shlex.join(command), "env": SPOOF})
                result = subprocess.run(command, cwd=workspace, env=env, capture_output=True, text=True, timeout=timeout)
                (directory / f"{name}.stdout").write_text(result.stdout)
                (directory / f"{name}.stderr").write_text(result.stderr)
                require((result.returncode == 0) == expect_ok, f"{name} exit {result.returncode}: {result.stderr[-1500:]}")
                return result

            first = len(provider.requests)
            result = run(f"{family}-root", [str(binary), "run", "--harness", family, "--model", model, *flags,
                                            "--cwd", str(workspace), ROOT_PROMPT])
            require("IDENTITY_ROOT_DONE" in result.stdout, f"{family} root did not finish; see {family}-root.stdout")
            events = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
            root = next(e["payload"]["session_id"] for e in events if e.get("payload", {}).get("session_id"))
            require(root not in SPOOF.values(), f"{family} root session id is spoofed: {root}")
            mine = [r for r in provider.requests[first:] if r["family"] == family]

            def probe(role, kind):
                return next((r["probes"][kind] for r in mine if r["role"] == role and kind in r["probes"]), None)

            expect(f"{family} root exec_command", probe("root", "SHELL"), root, root)
            expect(f"{family} root stdio MCP", probe("root", "MCP"), root, root)
            expect(f"{family} root stdio MCP call _meta thread_id|session_id", probe("root", "META"), root, root)
            child_shell = probe("child", "SHELL")
            child = child_shell[0] if child_shell else None
            if not child or child in (root, "unset", *SPOOF.values()):
                failures.append(f"{family} child exec_command did not see a distinct own session: {child_shell}")
            else:
                expect(f"{family} child exec_command", child_shell, child, root)
                # One stdio MCP process serves the root and its children, so its
                # environment names the session that started it (the root); each
                # call names its caller in the MCP request _meta instead.
                expect(f"{family} child stdio MCP process", probe("child", "MCP"), root, root)
                expect(f"{family} child stdio MCP call _meta thread_id|session_id", probe("child", "META"), child, child)
            if family == "claude":
                log = directory / "hooks.jsonl"
                records = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
                outcome["claude_hooks"] = records
                wrong = [r for r in records if r["thread"] != r["session_id"] or r["root"] != root]
                sessions = {r["session_id"] for r in records}
                if not records or wrong or root not in sessions or (child and child not in sessions):
                    failures.append(f"claude hooks: sessions {sorted(sessions)} (root {root}, child {child}); mismatched {wrong}")
                else:
                    checks.append(f"claude PreToolUse hooks ({len(records)}) saw their payload session and root {root}, "
                                  "for the root and the child")

            # A branch of the root's history, continued through the real TUI.
            branched = json.loads(run(f"{family}-branch", [str(binary), "rewind", root, "--mode", "conversation",
                                                          "--through", "1", "--restore"]).stdout)
            branch = branched["branch_session"]
            first = len(provider.requests)

            def answered():
                return any(r["role"] == "branch" and "IDENTITY_BRANCH_DONE" in json.dumps(r["body"]) or
                           r["role"] == "branch" and provider.branch_probed(r["body"], family == "claude")
                           for r in provider.requests[first:])

            done = pty_run(directory, f"{family}-resume-branch", [str(binary), "resume", branch, *flags,
                           "--prompt", BRANCH_PROMPT], env, workspace, commands, answered)
            branch_requests = [r for r in provider.requests[first:] if r["role"] == "branch"]
            require(done and branch_requests, f"{family} branch never finished; see {family}-resume-branch.terminal.txt")
            final = branch_requests[-1]["body"]
            tail = json.dumps(final.get("messages") if family == "claude" else final.get("input")).rsplit(BRANCH_PROMPT, 1)[-1]
            got = labelled(tail)
            expect(f"{family} branch exec_command", got.get("SHELL"), branch, root)
            expect(f"{family} branch stdio MCP", got.get("MCP"), branch, root)
            if family == "claude":
                log = directory / "hooks.jsonl"
                records = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
                mine = [r for r in records if r["session_id"] == branch]
                if not mine or any((r["thread"], r["root"]) != (branch, root) for r in mine):
                    failures.append(f"claude branch hooks: {mine}")
                else:
                    checks.append(f"claude branch PreToolUse hook: {branch}|{root}")
            outcome[family] = {"root": root, "child": child, "branch": branch}
        require(not provider.errors, f"provider errors: {provider.errors}")
        require(not failures, "; ".join(failures))
        outcome["success"] = True
    except Exception as error:
        outcome["error"] = str(error)
        raise
    finally:
        provider.server.shutdown()
        (artifact / "outcome.json").write_text(json.dumps(outcome, indent=2))
        print(json.dumps({"artifact": str(artifact), **{k: v for k, v in outcome.items() if k != "commands"}}))


def pty_run(directory, name, command, env, cwd, commands, finished, deadline_s=90):
    """Drive the real terminal until finished() and a completed turn render, then leave like a user."""
    commands.append({"name": name, "command": shlex.join(command)})
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 180, 0, 0))
    child = subprocess.Popen(command, cwd=cwd, env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
    os.close(slave)
    transcript, settled = bytearray(), None
    deadline = time.monotonic() + deadline_s
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
            if finished() and b"Turn completed" in ANSI.sub(b"", bytes(transcript)):
                settled = settled or time.monotonic()
                if time.monotonic() - settled > 1:
                    return True
        return False
    finally:
        for key in (b"\x04", b"\x03", b"\x03"):
            if child.poll() is not None:
                break
            try:
                os.write(master, key)
            except OSError:
                break
            time.sleep(0.5)
        if child.poll() is None:
            child.kill()
            child.wait()
        (directory / f"{name}.pty.log").write_bytes(transcript)
        (directory / f"{name}.terminal.txt").write_bytes(ANSI.sub(b"", bytes(transcript)))
        os.close(master)


if __name__ == "__main__":
    main()
