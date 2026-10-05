#!/usr/bin/env python3
"""Real CLI Skill + context journey. Only remote Messages inference is synthetic.

Run: python3 scripts/tests/claude-skills-cli-journey.py --binary target/debug/nanocodex
Requests, receipts, CLI transcript and scenario are saved in ignored output/.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import shlex
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4

spec = importlib.util.spec_from_file_location("native_journey", Path(__file__).with_name("claude-native-cli-journey.py"))
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)
require, sse, text_of = native.require, native.sse, native.text_of


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=Path("output/claude-skills-cli") / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    workspace = artifact / "workspace"
    (workspace / "home").mkdir(parents=True)

    def write(path, body):
        target = workspace / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(body)

    write("CLAUDE.md", "ROOT_CONTEXT_MARKER\nSee @docs/shared.md\n@../outside.md\n")
    (artifact / "outside.md").write_text("OUTSIDE_SECRET_MARKER\n")
    write("docs/shared.md", "IMPORTED_CONTEXT_MARKER\n")
    write("CLAUDE.local.md", "LOCAL_CONTEXT_MARKER\n")
    write("src/CLAUDE.md", "NESTED_CONTEXT_MARKER\n")
    write("src/main.rs", "fn main() {}\n")
    write(".claude/rules/rust.md", "---\npaths: ['src/**/*.rs']\n---\nRUST_CONTEXT_MARKER\n")
    write(".claude/rules/web.md", "---\npaths: ['web/**/*.ts']\n---\nWEB_CONTEXT_MARKER\n")
    write(".claude/skills/review/SKILL.md", "---\ndescription: Review components\nargument-hint: '[component] [mode]'\nallowed-tools: [Read, Grep]\n---\nReview $0 in $ARGUMENTS[1] mode. Full: $ARGUMENTS\n")
    write(".claude/skills/deploy/SKILL.md", "---\ndescription: DISABLED_CATALOG_MARKER\ndisable-model-invocation: true\n---\nNever automatically deploy.\n")
    write(".claude/skills/dynamic/SKILL.md", "Unsafe dynamic command !`touch unexpected`\n")
    write(".claude/skills/fork/SKILL.md", "---\ncontext: fork\n---\nUnsupported execution\n")
    write(".claude/skills/hidden/SKILL.md", "---\ndescription: OVERRIDE_HIDDEN_MARKER\n---\nHidden body\n")
    write(".claude/skills/name-only/SKILL.md", "---\ndescription: OVERRIDE_DESCRIPTION_MARKER\n---\nName-only body\n")
    write(".claude/skills/user-only/SKILL.md", "---\ndescription: OVERRIDE_USER_ONLY_MARKER\n---\nUser-only body\n")
    write(".claude/settings.json", json.dumps({"skillOverrides": {"hidden": "off", "name-only": "name-only", "user-only": "user-invocable-only", "deploy": "on"}}))
    requests, receipts, errors = [], [], []
    steps = [
        ("Skill", {"skill": "review", "args": '"core api" strict'}, False, "Review core api in strict mode"),
        ("Skill", {"skill": "deploy"}, True, "unavailable for this caller"),
        ("Skill", {"skill": "hidden"}, True, "skillOverrides"),
        ("Skill", {"skill": "user-only"}, True, "skillOverrides"),
        ("Skill", {"skill": "name-only"}, False, "Name-only body"),
        ("Skill", {"skill": "review", "caller": "user"}, True, "unsupported Skill option"),
        ("Skill", {"skill": "dynamic"}, True, "dynamic shell injection is unsupported"),
        ("Skill", {"skill": "../review"}, True, "skill name must contain"),
        ("Skill", {"skill": "review", "args": "'unclosed"}, True, "unclosed quote"),
        ("Read", {"file_path": "src/main.rs"}, False, "NESTED_CONTEXT_MARKER"),
        ("ProjectContext", {"path": "../outside.md"}, True, "workspace-relative"),
        ("Write", {"file_path": ".claude/skills/review/SKILL.md", "content": "---\ndisable-model-invocation: true\n---\nEdited during session\n"}, False, None),
        ("Skill", {"skill": "review"}, True, "unavailable for this caller"),
        ("Write", {"file_path": ".claude/skills/review/SKILL.md", "content": "Recovered $ARGUMENTS\n"}, False, None),
        ("Skill", {"skill": "review", "args": "after edit"}, False, "Recovered after edit"),
        ("Write", {"file_path": ".claude/settings.json", "content": json.dumps({"skillOverrides": {"review": "off"}})}, False, None),
        ("Skill", {"skill": "review"}, True, "skillOverrides"),
        ("Write", {"file_path": ".claude/settings.local.json", "content": json.dumps({"skillOverrides": {"review": "name-only"}})}, False, None),
        ("Skill", {"skill": "review", "args": "local precedence"}, False, "Recovered local precedence"),
        ("Write", {"file_path": ".claude/settings.local.json", "content": json.dumps({"skillOverrides": {"review": "invalid"}})}, False, None),
        ("Skill", {"skill": "review"}, True, "invalid skillOverrides"),
        ("Write", {"file_path": ".claude/settings.local.json", "content": json.dumps({"skillOverrides": {"review": "on"}})}, False, None),
        ("Skill", {"skill": "review", "args": "settings recovery"}, False, "Recovered settings recovery"),
        ("ProjectContext", {"path": "src/main.rs"}, False, "RUST_CONTEXT_MARKER"),
    ]

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["content-length"])))
            stage = len(requests)
            requests.append(request)
            try:
                require(self.path == "/v1/messages", "wrong Messages route")
                names = {tool["name"] for tool in request.get("tools", [])}
                require({"Skill", "ProjectContext", "Read", "Write"} <= names, "skill/context tools not installed")
                if stage == 0:
                    system = json.dumps(request.get("system"))
                    for marker in ["ROOT_CONTEXT_MARKER", "IMPORTED_CONTEXT_MARKER", "LOCAL_CONTEXT_MARKER"]:
                        require(marker in system, f"startup guidance missing: {marker}")
                    for marker in ["OUTSIDE_SECRET_MARKER", "NESTED_CONTEXT_MARKER", "RUST_CONTEXT_MARKER", "WEB_CONTEXT_MARKER", "DISABLED_CATALOG_MARKER", "OVERRIDE_HIDDEN_MARKER", "OVERRIDE_DESCRIPTION_MARKER", "OVERRIDE_USER_ONLY_MARKER"]:
                        require(marker not in system, f"startup scope/visibility leak: {marker}")
                if stage:
                    call_id = f"skills_{stage - 1}"
                    results = [block for message in request["messages"] for block in message.get("content", []) if isinstance(block, dict) and block.get("type") == "tool_result" and block.get("tool_use_id") == call_id]
                    require(len(results) == 1, f"missing unique receipt {call_id}")
                    receipt = results[0]
                    receipts.append(receipt)
                    name, arguments, error, marker = steps[stage - 1]
                    require(bool(receipt.get("is_error", False)) == error, f"wrong error flag: {receipt}")
                    text = text_of(receipt)
                    if marker:
                        require(marker in text, f"missing {marker}: {receipt}")
                    if name == "Read" or (name == "ProjectContext" and not error):
                        for expected in ["NESTED_CONTEXT_MARKER", "RUST_CONTEXT_MARKER", "IMPORTED_CONTEXT_MARKER"]:
                            require(expected in text, f"scoped guidance missing: {expected}")
                        for forbidden in ["WEB_CONTEXT_MARKER", "OUTSIDE_SECRET_MARKER"]:
                            require(forbidden not in text, f"context boundary leak: {forbidden}")
                if stage < len(steps):
                    name, arguments, _, _ = steps[stage]
                    block = {"type": "tool_use", "id": f"skills_{stage}", "name": name, "input": arguments}
                else:
                    require(stage == len(steps), "unexpected provider retry")
                    block = {"type": "text", "text": "skills-context-journey-complete"}
            except Exception as error:
                errors.append(str(error))
                block = {"type": "text", "text": "fixture-assertion-failed"}
            (artifact / "provider.json").write_text(json.dumps(requests, indent=2))
            (artifact / "receipts.json").write_text(json.dumps(receipts, indent=2))
            payload = sse(block, request["model"])
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    command = [str(binary), "run", "--claude", "--model", "claude-sonnet-5-5", "--thinking", "medium", "--claude-api-key", "synthetic-claude-key", "--claude-messages-url", f"http://127.0.0.1:{server.server_port}/v1/messages", "--cwd", str(workspace), "--rollouts", "false", "--browser=none", "--mcp-defaults", "false", "--mcp-codex-config", "false", "--web-search", "false", "--image-generation", "false", "--subagents", "false", "--memory", "false", "Exercise real Skill invocation and scoped project context using the synthetic project."]
    environment = {"HOME": str(workspace / "home"), "CODEX_HOME": str(workspace / "codex-home"), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "NANOCODEX_COMPUTER": "off"}
    (artifact / "scenario.json").write_text(json.dumps({"command": command, "shell_command": shlex.join(command), "environment": environment, "steps": steps, "expected": "native Skill dispatch, caller provenance enforcement, no shell expansion, live edit disable and recovery, skillOverrides startup visibility/local precedence/invalid recovery, startup root context, automatic nested Read context, import boundaries", "boundary": "actual shipped CLI and Messages HTTP/SSE; synthetic model only"}, indent=2))
    outcome = {"success": False}
    try:
        result = subprocess.run(command, cwd=workspace, env=environment, capture_output=True, timeout=60)
        (artifact / "stdout.jsonl").write_bytes(result.stdout)
        (artifact / "stderr.log").write_bytes(result.stderr)
        require(result.returncode == 0, f"CLI exit {result.returncode}: {result.stderr.decode(errors='replace')}")
        require(not errors, "; ".join(errors))
        require(b"skills-context-journey-complete" in result.stdout, "completion absent")
        require(len(requests) == len(steps) + 1, f"wrong provider count: {len(requests)}")
        require(not (workspace / "unexpected").exists(), "dynamic command ran")
        outcome = {"success": True, "provider_requests": len(requests), "tool_receipts": len(receipts), "dynamic_shell_executed": False, "automatic_scoped_read_context": True, "live_edit_disable_and_recovery": True, "skill_overrides_live_precedence_and_recovery": True}
    except Exception as error:
        outcome["error"] = str(error)
        raise
    finally:
        (artifact / "outcome.json").write_text(json.dumps(outcome, indent=2))
        server.shutdown()
        print(json.dumps({"artifact": str(artifact), **outcome}))


if __name__ == "__main__":
    main()
