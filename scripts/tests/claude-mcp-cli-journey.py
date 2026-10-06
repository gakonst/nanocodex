#!/usr/bin/env python3
"""Actual Claude CLI + external MCP HTTP/stdio servers; synthetic Messages endpoint.

python3 scripts/tests/claude-mcp-cli-journey.py --binary target/debug/nanocodex
"""
import argparse
import importlib.util
import json
from pathlib import Path
import shlex
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

spec = importlib.util.spec_from_file_location("native_cli", Path(__file__).with_name("claude-native-cli-journey.py"))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)
require, text_of, sse = helper.require, helper.text_of, helper.sse
PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jD1sAAAAASUVORK5CYII="
SCHEMA = {"type": "object", "properties": {"message": {"type": "string"}}, "required": ["message"], "additionalProperties": False}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/nanocodex")
    args = parser.parse_args()
    binary = Path(args.binary).resolve()
    root = Path(__file__).resolve().parents[2]
    artifact = root / "output" / "claude-mcp-cli" / str(uuid4())
    workspace = artifact / "workspace"
    workspace.mkdir(parents=True)
    (workspace / "home").mkdir()
    messages, rpc, errors = [], [], []
    remote = {"removed": False, "schema": SCHEMA}
    phase = {"name": "ordinary"}
    frozen_received = threading.Event()
    release_frozen = threading.Event()
    recovery_messages = []
    hook = workspace / "hook.py"
    hook.write_text("""import json, sys
p = json.load(sys.stdin)
with open('hooks.jsonl', 'a') as f: f.write(json.dumps(p) + '\\n')
if p['hook_event_name'] == 'PreToolUse' and p['tool_input'].get('message') == 'denied':
    print(json.dumps({'hookSpecificOutput': {'hookEventName': 'PreToolUse', 'permissionDecision': 'deny', 'permissionDecisionReason': 'synthetic MCP denial'}}))
else: print('{}')
""")
    hooks = artifact / "hooks.json"
    hooks.write_text(json.dumps({"hooks": {event: [{"matcher": "^mcp__", "hooks": [{"type": "command", "command": "/usr/bin/python3 " + shlex.quote(str(hook))}]}] for event in ["PreToolUse", "PostToolUse", "PostToolUseFailure"]}}))
    steps = [
        ("WaitForMcpServers", {}, False, "complete"),
        ("ToolSearch", {"query": "select:mcp__http__inspect,mcp__stdio__echo"}, False, "input_schema"),
        ("mcp__stdio__echo", {"message": "__metadata__"}, False, "fixture:__metadata__"),
        ("mcp__http__inspect", {"message": "image"}, False, "native-http-image"),
        ("mcp__http__inspect", {"message": "failure"}, True, "native-http-error"),
        ("mcp__http__inspect", {"message": "audio"}, True, "cannot be represented"),
        ("mcp__http__inspect", {"message": "denied"}, True, "synthetic MCP denial"),
        ("mcp__http__inspect", {"message": "retire"}, False, "native-http-image"),
        ("ToolSearch", {"query": "Inspect synthetic content"}, False, '"tools":[]'),
        ("ListMcpResourcesTool", {}, False, "fixture://http"),
        ("ReadMcpResourceTool", {"server": "http", "uri": "fixture://http"}, False, "HTTP resource body"),
        ("ReadMcpResourceTool", {"server": "http", "uri": "fixture://missing"}, True, "Synthetic missing resource"),
        ("ReadMcpResourceTool", {"server": "unknown", "uri": "file:///etc/passwd"}, True, "unavailable"),
        ("ToolSearch", {"query": "inspect", "max_results": 0}, True, "between 1 and 32"),
        ("ToolSearch", {"query": "inspect", "authorization": "model-supplied-token"}, True, "unknown field"),
    ]

    class Server(BaseHTTPRequestHandler):
        def log_message(self, *_): pass
        def do_GET(self):
            self.send_response(405); self.end_headers()
        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["content-length"])))
            if self.path == "/mcp":
                if self.headers.get("Authorization") != "Bearer synthetic-mcp-configuration-token":
                    errors.append("MCP configured authorization missing or replaced")
                    self.send_response(401); self.end_headers(); return
                rpc.append(request)
                (artifact / "mcp.json").write_text(json.dumps(rpc, indent=2))
                if "id" not in request:
                    self.send_response(202); self.end_headers(); return
                method = request["method"]
                result, error = None, None
                if method == "initialize":
                    result = {"protocolVersion": request["params"]["protocolVersion"], "capabilities": {"tools": {}, "resources": {}}, "serverInfo": {"name": "synthetic-http", "version": "1"}}
                elif method == "tools/list":
                    result = {"tools": [] if remote["removed"] else [{"name": "inspect", "description": "Inspect synthetic content", "inputSchema": remote["schema"]}]}
                elif method == "tools/call":
                    message = request["params"]["arguments"]["message"]
                    if message == "retire": remote["removed"] = True
                    content = [{"type": "text", "text": "native-http-error" if message == "failure" else "native-http-image"}]
                    if message == "image": content.append({"type": "image", "mimeType": "image/png", "data": PNG})
                    if message == "audio": content.append({"type": "audio", "mimeType": "audio/wav", "data": "UklGRg=="})
                    result = {"content": content, "isError": message == "failure", "structuredContent": {"marker": message}, "_meta": {"fixture": "native-preserved"}}
                elif method == "resources/list":
                    result = {"resources": [{"uri": "fixture://http", "name": "HTTP resource", "mimeType": "text/plain"}]}
                elif method == "resources/templates/list": result = {"resourceTemplates": []}
                elif method == "resources/read":
                    if request["params"]["uri"] == "fixture://missing": error = {"code": -32002, "message": "Synthetic missing resource"}
                    else: result = {"contents": [{"uri": "fixture://http", "mimeType": "text/plain", "text": "HTTP resource body"}, {"uri": "fixture://pixel", "mimeType": "image/png", "blob": PNG}]}
                else: error = {"code": -32601, "message": "unknown method"}
                body = json.dumps({"jsonrpc": "2.0", "id": request["id"], **({"error": error} if error else {"result": result})}).encode()
                self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body); return
            if phase["name"] == "recovery":
                index = len(recovery_messages)
                recovery_messages.append(request)
                try:
                    if index == 0:
                        block = {"type": "tool_use", "id": "ready", "name": "WaitForMcpServers", "input": {}}
                    elif index == 1:
                        block = {"type": "tool_use", "id": "find", "name": "ToolSearch", "input": {"query": "select:mcp__http__inspect"}}
                    elif index in (2, 3):
                        if index == 2:
                            frozen_received.set()
                            require(release_frozen.wait(15), "test did not release frozen request")
                        else:
                            # Observe actual catalog startup completion in the shipped
                            # CLI log before returning this in-flight tool response.
                            deadline = time.monotonic() + 10
                            while time.monotonic() < deadline:
                                log = (artifact / "frozen-resumed.stderr").read_text()
                                if any('mcp.server_start' in line and 'completed' in line and 'http' in line and 'mcp.transport_connect' not in line for line in log.splitlines()):
                                    break
                                time.sleep(0.01)
                            else: raise AssertionError("reopened HTTP MCP catalog did not finish startup")
                            require(request["tools"] == recovery_messages[2]["tools"], "restore replaced admitted tool definitions")
                        block = {"type": "tool_use", "id": "frozen-call", "name": "mcp__http__inspect", "input": {"message": "image"}}
                    else:
                        receipt = [b for m in request["messages"] for b in m["content"] if b.get("type") == "tool_result" and b.get("tool_use_id") == "frozen-call"][-1]
                        require(receipt.get("is_error") is True, "same-name changed schema was executed")
                        require("changed since admission" in text_of(receipt), "missing changed-schema denial")
                        current = next(t for t in request["tools"] if t["name"] == "mcp__http__inspect")
                        require(current["input_schema"] == remote["schema"], "next request failed to admit current schema")
                        block = {"type": "text", "text": "frozen-mcp-journey-complete"}
                except Exception as error:
                    errors.append(str(error)); block = {"type": "text", "text": "fixture-assertion-failed"}
                (artifact / "recovery-messages.json").write_text(json.dumps(recovery_messages, indent=2))
                body = sse(block, request["model"])
                try:
                    self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError): pass
                return
            stage = len(messages)
            messages.append(request)
            try:
                require(self.path == "/v1/messages", "unexpected request path")
                tools = {tool["name"]: tool for tool in request["tools"]}
                require({"ToolSearch", "WaitForMcpServers", "ListMcpResourcesTool", "ReadMcpResourceTool"} <= tools.keys(), "native discovery tools missing")
                require(not ({"exec", "wait", "tool_search", "exec_command"} & tools.keys()), "Codex tools advertised in Claude")
                if stage:
                    if stage < 9:
                        require(tools["mcp__http__inspect"]["input_schema"] == SCHEMA, "remote schema changed or dynamic discovery was frozen")
                        require(tools["mcp__http__inspect"].get("defer_loading") is True, "MCP schema was eagerly exposed")
                    else:
                        require("mcp__http__inspect" not in tools, "removed remote MCP tool remained advertised")
                    receipt = [b for m in request["messages"] for b in m["content"] if b.get("type") == "tool_result" and b.get("tool_use_id") == f"mcp_{stage-1}"][-1]
                    name, arguments, failed, marker = steps[stage-1]
                    require(bool(receipt.get("is_error", False)) == failed, f"wrong {name} error status: {receipt}")
                    require(marker in text_of(receipt), f"missing {name} marker {marker}: {receipt}")
                    if arguments.get("message") == "image" or (name == "ReadMcpResourceTool" and arguments["uri"] == "fixture://http"):
                        require(any(b.get("type") == "image" and b["source"]["data"] == PNG for b in receipt["content"]), "native image lost")
                    if stage == 2:
                        require({b.get("tool_name") for b in receipt["content"] if b.get("type") == "tool_reference"} == {"mcp__http__inspect", "mcp__stdio__echo"}, "native ToolSearch references missing")
                if stage < len(steps):
                    name, arguments, _, _ = steps[stage]
                    block = {"type": "tool_use", "id": f"mcp_{stage}", "name": name, "input": arguments}
                else: block = {"type": "text", "text": "native-mcp-journey-complete"}
            except Exception as error:
                errors.append(str(error)); block = {"type": "text", "text": "fixture-assertion-failed"}
            (artifact / "messages.json").write_text(json.dumps(messages, indent=2))
            body = sse(block, request["model"])
            self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Server)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    command = [str(binary), "run", "--claude", "--model", "claude-sonnet-5-5", "--thinking", "medium", "--claude-api-key", "synthetic-claude-key", "--claude-messages-url", f"http://127.0.0.1:{server.server_port}/v1/messages", "--cwd", str(workspace), "--rollouts", "false", "--browser=none", "--mcp-defaults", "false", "--mcp-codex-config", "false", "--web-search", "false", "--image-generation", "false", "--subagents", "false", "--memory", "false", "--mcp", f"http=http://127.0.0.1:{server.server_port}/mcp", "--mcp-bearer-env", "http=SYNTHETIC_MCP_TOKEN", "--mcp-stdio", "stdio=/usr/bin/node", "--mcp-arg", f"stdio={root}/crates/nanocodex-oai-tools/tests/fixtures/mcp-stdio-server.mjs", "--claude-hooks", str(hooks), "--local-durability", str(artifact / "session.sqlite"), "--local-durability-state-id", "native-mcp-journey", "--request-id", "native-mcp-operation", "Exercise native MCP tools and resources."]
    environment = {"HOME": str(workspace / "home"), "CODEX_HOME": str(workspace / "codex-home"), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "NANOCODEX_COMPUTER": "off", "SYNTHETIC_MCP_TOKEN": "synthetic-mcp-configuration-token"}
    (artifact / "scenario.json").write_text(json.dumps({"command": command, "shell_command": shlex.join(command), "environment": environment, "expected": "real MCP HTTP+stdio exact schema, native image/error/structured metadata, resources and validation"}, indent=2))
    outcome = {"success": False}
    try:
        result = subprocess.run(command, cwd=workspace, env=environment, capture_output=True, timeout=90)
        (artifact / "cli.jsonl").write_bytes(result.stdout); (artifact / "stderr.log").write_bytes(result.stderr)
        require(result.returncode == 0, f"CLI exit {result.returncode}: {result.stderr.decode(errors='replace')}")
        require(not errors, "; ".join(errors))
        require(len(messages) == len(steps) + 1, "incorrect Messages request count")
        require("synthetic-mcp-configuration-token" not in json.dumps(messages), "MCP authorization leaked to model")
        require(b"native-mcp-journey-complete" in result.stdout, "missing final answer")
        require("native-preserved" in result.stdout.decode(), "MCP metadata lost in public event stream")
        require('"marker":"audio"' in result.stdout.decode().replace(" ", ""), "unsupported media structured result lost")
        require(sum(r["method"] == "tools/call" for r in rpc) == 4, "unexpected MCP call retry")
        replay = subprocess.run(command, cwd=workspace, env=environment, capture_output=True, timeout=90)
        (artifact / "replay.jsonl").write_bytes(replay.stdout); (artifact / "replay.stderr.log").write_bytes(replay.stderr)
        require(replay.returncode == 0 and b"native-mcp-journey-complete" in replay.stdout, "durable MCP terminal replay failed")
        require(len(messages) == len(steps) + 1, "terminal recovery repeated Messages request")
        require(sum(r["method"] == "tools/call" for r in rpc) == 4, "recovery repeated MCP effect")
        hook_log = [json.loads(line) for line in (workspace / "hooks.jsonl").read_text().splitlines()]
        require(any(e["hook_event_name"] == "PostToolUseFailure" for e in hook_log), "dynamic error post-hook missing")
        require(not any(r["method"] == "tools/call" and r["params"]["arguments"].get("message") == "denied" for r in rpc), "hook denial dispatched remote effect")
        hooks_before = len(hook_log)
        phase["name"] = "recovery"
        remote["removed"] = False
        recovery = [str(artifact / "frozen.sqlite") if v == str(artifact / "session.sqlite") else v for v in command]
        with (artifact / "frozen-first.jsonl").open("wb") as out, (artifact / "frozen-first.stderr").open("wb") as err:
            process = subprocess.Popen(recovery, cwd=workspace, env=environment, stdout=out, stderr=err)
            try:
                require(frozen_received.wait(15), "CLI did not reach frozen request")
                process.kill(); process.wait(timeout=5)
            finally:
                if process.poll() is None: process.kill(); process.wait(timeout=5)
                release_frozen.set()
        remote["schema"] = {**SCHEMA, "properties": {"message": {"type": "string"}, "new_authority": {"type": "boolean"}}}
        with (artifact / "frozen-resumed.stderr").open("wb") as stderr:
            resumed = subprocess.run(recovery, cwd=workspace, env=environment, stdout=subprocess.PIPE, stderr=stderr, timeout=90)
        (artifact / "frozen-resumed.jsonl").write_bytes(resumed.stdout)
        resumed.stderr = (artifact / "frozen-resumed.stderr").read_bytes()
        require(resumed.returncode == 0, f"frozen reopen failed: {resumed.stderr.decode(errors='replace')}")
        require(not errors, "; ".join(errors))
        require(b"frozen-mcp-journey-complete" in resumed.stdout, "frozen journey incomplete")
        require(sum(r["method"] == "tools/call" for r in rpc) == 4, "same-name replacement or replay dispatched an effect")
        require(len((workspace / "hooks.jsonl").read_text().splitlines()) == hooks_before, "changed schema ran replacement hooks")
        outcome.update(success=True, requests=len(messages), recovery_requests=len(recovery_messages), mcp_requests=len(rpc), checks=len(steps))
    finally:
        (artifact / "outcome.json").write_text(json.dumps(outcome, indent=2)); server.shutdown()
        print(artifact)
    print(json.dumps(outcome))

if __name__ == "__main__": main()
