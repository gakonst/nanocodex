#!/usr/bin/env python3
"""Black-box native Claude CLI acceptance; only the Messages provider is synthetic.

Run after building nanocodex-bin:
  python3 scripts/tests/claude-native-cli-journey.py --binary target/debug/nanocodex
Evidence stays in ignored output/claude-native-cli/<uuid>/.
"""
from claude_code_fixture import normalize_request, wrap_tool
import argparse
import json
from pathlib import Path
import shlex
import struct
import zlib
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def text_of(receipt):
    content = receipt.get("content", "")
    return content if isinstance(content, str) else "\n".join(
        block.get("text", "") for block in content if block.get("type") == "text"
    )


def sse(block, model):
    is_tool = block["type"] == "tool_use"
    content = dict(block)
    delta = {"type": "input_json_delta", "partial_json": json.dumps(content.pop("input"))} if is_tool else {"type": "text_delta", "text": content.pop("text")}
    content["input" if is_tool else "text"] = {} if is_tool else ""
    events = [
        {"type": "message_start", "message": {"id": "msg_" + uuid4().hex, "type": "message", "role": "assistant", "model": model, "content": [], "usage": {"input_tokens": 10, "output_tokens": 0}}},
        {"type": "content_block_start", "index": 0, "content_block": content},
        {"type": "content_block_delta", "index": 0, "delta": delta},
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": "tool_use" if is_tool else "end_turn"}, "usage": {"output_tokens": 10}},
        {"type": "message_stop"},
    ]
    return "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", type=Path, default=Path("output/claude-native-cli") / uuid4().hex)
    args = parser.parse_args()
    binary, artifact = args.binary.resolve(), args.output.resolve()
    require(binary.is_file(), f"build the CLI first: {binary}")
    workspace = artifact / "workspace"
    workspace.mkdir(parents=True)
    (workspace / "home").mkdir()
    (artifact / "outside.txt").write_text("synthetic-outside-marker")
    (workspace / "editable.txt").write_text("alpha alpha\nunique\n")
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(b"\x00\xff\x00\x00")) + chunk(b"IEND", b"")
    (workspace / "pixel.png").write_bytes(png)
    (workspace / "notebook.ipynb").write_text(json.dumps({"nbformat":4, "nbformat_minor":5,"metadata":{},"cells":[{"id":"example","cell_type":"code","execution_count":1,"metadata":{},"source":["print('old')"],"outputs":[{"output_type":"stream","name":"stdout","text":["old\n"]}]}]}))
    requests, errors = [], []
    session_ids = {}
    steps = [
        ("Read", {"file_path": "editable.txt"}, False, "alpha alpha"),
        ("Edit", {"file_path": "editable.txt", "old_string": "alpha", "new_string": "WRONG"}, True, None),
        ("Edit", {"file_path": "editable.txt", "old_string": "unique", "new_string": "verified"}, False, None),
        ("Write", {"file_path": "created.txt", "content": "native-write-effect\n"}, False, None),
        ("exec_command", {"cmd": "printf 'native-shell-receipt'; printf x >> counter.txt; exit 7"}, False, "native-shell-receipt"),
        ("Read", {"file_path": "../outside.txt"}, True, None),
        ("Read", {"file_path": "editable.txt"}, False, "verified"),
        ("TaskCreate", {"subject": "Durable task", "description": "Restored through the actual CLI"}, False, "Durable task"),
        ("TaskUpdate", {"taskId": "1", "status": "in_progress"}, False, "in_progress"),
        ("Read", {"file_path": "pixel.png"}, False, None),
        ("NotebookEdit", {"notebook_path":"notebook.ipynb","cell_id":"example","new_source":"print('native notebook')"}, False, None),
        ("Read", {"file_path":"notebook.ipynb"}, False, "native notebook"),
        ("Read", {"file_path":"pixel.png","pages":"1"}, True, None),
        ("exec_command", {"cmd":"sleep 1; printf prior-task", "yield_time_ms":1}, False, None),
        ("write_stdin", {"session_id":"__prior__", "chars":"", "yield_time_ms":10000}, False, "prior-task"),
    ]
    restore_steps = [
        ("TaskGet", {"taskId": "1"}, False, "Durable task"),
        ("TaskCreate", {"subject": "After reopen", "description": "ID watermark survives"}, False, "After reopen"),
        ("exec_command", {"cmd":"sleep 1; printf fresh-task", "yield_time_ms":1}, False, None),
        ("write_stdin", {"session_id":"__new__", "chars":"", "yield_time_ms":10000}, False, "fresh-task"),
    ]
    phase = {"name": "initial", "start": 0, "steps": steps}

    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers["content-length"]))); request = normalize_request(request, artifact)
            stage = len(requests) - phase["start"]
            current_steps = phase["steps"]
            requests.append(request)
            for index, tool in enumerate(request.get("tools", [])):
                schema = tool.get("input_schema")
                if schema is None:  # Anthropic-executed server tool.
                    continue
                forbidden = [key for key in ("oneOf", "allOf", "anyOf") if key in schema]
                if forbidden:
                    message = f"tools.{index}.custom.input_schema: input_schema does not support oneOf, allOf, or anyOf at the top level ({tool['name']})"
                    errors.append(message)
                    (artifact / "provider.json").write_text(json.dumps(requests, indent=2))
                    response = json.dumps({"type": "error", "error": {"type": "invalid_request_error", "message": message}}).encode()
                    self.send_response(400)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(response)))
                    self.end_headers()
                    self.wfile.write(response)
                    return
            try:
                require(self.path == "/v1/messages", "unexpected provider route")
                require(self.headers.get("x-api-key") == "synthetic-claude-key", "wrong synthetic authentication")
                if stage:
                    expected_id = f"{phase['name']}_{stage - 1}"
                    receipts = [block for message in request["messages"] for block in message.get("content", []) if isinstance(block, dict) and block.get("type") == "tool_result" and block.get("tool_use_id") == expected_id]
                    require(len(receipts) == 1, f"expected one receipt for {expected_id}")
                    receipt = receipts[0]
                    prior_name, prior_input, failed, marker = current_steps[stage - 1]
                    require(bool(receipt.get("is_error", False)) == failed, f"wrong error status for {expected_id}: {receipt}")
                    if marker:
                        require(marker in text_of(receipt), f"missing tool output {marker}: {receipt}")
                    if phase["name"] == "initial" and stage == 5:
                        require(json.loads(text_of(receipt))["exit_code"] == 7, "exec_command lost nonzero exit status")
                    if phase["name"] == "initial" and stage == 6:
                        require("synthetic-outside-marker" not in text_of(receipt), "Read escaped workspace")
                    if prior_name == "exec_command" and prior_input.get("yield_time_ms") == 1:
                        key = "__prior__" if phase["name"] == "initial" else "__new__"
                        session_ids[key] = json.loads(text_of(receipt))["session_id"]
                    if prior_name == "write_stdin":
                        require(json.loads(text_of(receipt)).get("exit_code") == 0, "polled process did not finish successfully")
                    if prior_name == "Read" and prior_input.get("file_path") == "pixel.png" and not failed:
                        require(any(block.get("type") == "image" and block["source"]["type"] == "base64" and block["source"]["media_type"] == "image/png" for block in receipt["content"]), "Read dropped native image block")
                    if phase["name"] == "reopen" and stage <= 2:
                        task = json.loads(text_of(receipt))["task"]
                        require(task["id"] == str(stage), "restored task ID or watermark lost")
                        if stage == 1:
                            require(task["status"] == "in_progress", "task update lost across reopen")
                if stage < len(current_steps):
                    name, arguments, _, _ = current_steps[stage]
                    arguments = {key: session_ids.get(value,value) if isinstance(value,str) else value for key,value in arguments.items()}
                    block = {"type": "tool_use", "id": f"{phase['name']}_{stage}", "name": name, "input": arguments}
                else:
                    require(stage == len(current_steps), "unexpected provider retry")
                    block = {"type": "text", "text": "native-cli-journey-complete"}
            except Exception as error:
                errors.append(str(error))
                block = {"type": "text", "text": "fixture-assertion-failed"}
            (artifact / "provider.json").write_text(json.dumps(requests, indent=2))
            response = sse(wrap_tool(block), request["model"])
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(response)))
            self.end_headers()
            self.wfile.write(response)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    command = [str(binary), "run", "--claude", "--model", "claude-sonnet-5-5", "--thinking", "medium", "--claude-api-key", "synthetic-claude-key", "--claude-messages-url", f"http://127.0.0.1:{server.server_port}/v1/messages", "--cwd", str(workspace), "--rollouts", "false", "--browser=none", "--mcp-defaults", "false", "--mcp-codex-config", "false", "--web-search", "false", "--image-generation", "false", "--subagents", "false", "--memory", "false", "--local-durability", str(artifact / "session.sqlite"), "--local-durability-state-id", "native-cli-journey", "--request-id", "native-cli-operation", "Exercise synthetic native file, process and durable recovery journey."]
    environment = {"HOME": str(workspace / "home"), "CODEX_HOME": str(workspace / "codex-home"), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "NANOCODEX_COMPUTER": "off"}
    (artifact / "scenario.json").write_text(json.dumps({"command": command, "shell_command": shlex.join(command), "environment": environment, "expected": "real file edits, safe rejection, nonzero exit receipt, yielded process polling, terminal replay without provider or effect replay", "boundary": "actual native CLI + Messages HTTP/SSE + local processes + SQLite; synthetic model provider only"}, indent=2))
    outcome = {"success": False}
    try:
        for attempt in ("first", "replay"):
            prior = len(requests)
            result = subprocess.run(command, cwd=workspace, env=environment, capture_output=True, timeout=60)
            (artifact / f"{attempt}.jsonl").write_bytes(result.stdout)
            (artifact / f"{attempt}.stderr.log").write_bytes(result.stderr)
            require(result.returncode == 0, f"{attempt} exit {result.returncode}: {result.stderr.decode(errors='replace')}")
            require(not errors, "; ".join(errors))
            require(b"native-cli-journey-complete" in result.stdout, f"{attempt} final answer absent")
            require(len(requests) == len(steps) + 1, f"unexpected provider request count: {len(requests)}")
            if attempt == "replay":
                require(len(requests) == prior, "terminal replay contacted provider")
            require((workspace / "editable.txt").read_text() == "alpha alpha\nverified\n", "ambiguous edit mutated file or exact edit failed")
            require((workspace / "created.txt").read_text() == "native-write-effect\n", "Write had no filesystem effect")
            require((workspace / "counter.txt").read_text() == "x", "exec_command effect repeated")
        phase.update(name="reopen", start=len(requests), steps=restore_steps)
        followup = list(command)
        followup[followup.index("--request-id") + 1] = "native-cli-followup"
        followup[-1] = "Read the saved task and create another after process restart."
        result = subprocess.run(followup, cwd=workspace, env=environment, capture_output=True, timeout=60)
        (artifact / "reopen.jsonl").write_bytes(result.stdout)
        (artifact / "reopen.stderr.log").write_bytes(result.stderr)
        (artifact / "reopen-command.json").write_text(json.dumps(followup, indent=2))
        require(result.returncode == 0, f"reopen failed: {result.stderr.decode(errors='replace')}")
        require(not errors, "; ".join(errors))
        require(b"native-cli-journey-complete" in result.stdout, "reopen final answer absent")
        require(len(requests) - phase["start"] == len(restore_steps) + 1, "reopen request count incorrect")
        outcome = {"success": True, "provider_requests": len(requests), "replay_provider_requests": 0, "shell_effect_count": 1, "yielded_process_output": True, "reopened_task_status": "in_progress", "next_task_id": "2", "native_image_block": True, "notebook_edit": True}
    except Exception as error:
        outcome["error"] = str(error)
        raise
    finally:
        (artifact / "outcome.json").write_text(json.dumps(outcome, indent=2))
        server.shutdown()
        print(json.dumps({"artifact": str(artifact), **outcome}))


if __name__ == "__main__":
    main()
