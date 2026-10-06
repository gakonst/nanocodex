#!/usr/bin/env python3
"""Run the shipped native CLI against loopback Messages; retain requests and real effects."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from uuid import uuid4


def require(ok, message):
    if not ok:
        raise AssertionError(message)


def sse(blocks, model):
    events = [{"type": "message_start", "message": {"id": "msg_" + uuid4().hex, "role": "assistant", "model": model, "content": [], "usage": {"input_tokens": 10, "output_tokens": 0}}}]
    for index, block in enumerate(blocks):
        kind = block["type"]
        start = dict(block)
        if kind == "tool_use":
            delta = {"type": "input_json_delta", "partial_json": json.dumps(start.pop("input"))}
            start["input"] = {}
        else:
            key = "thinking" if kind == "thinking" else "text"
            delta = {"type": key + "_delta", key: start.pop(key)}
            start[key] = ""
            if kind == "thinking":
                start["signature"] = ""
        events += [{"type": "content_block_start", "index": index, "content_block": start}, {"type": "content_block_delta", "index": index, "delta": delta}, {"type": "content_block_stop", "index": index}]
        if kind == "thinking":
            events.insert(len(events) - 1, {"type": "content_block_delta", "index": index, "delta": {"type": "signature_delta", "signature": block["signature"]}})
    events += [{"type": "message_delta", "delta": {"stop_reason": "tool_use" if blocks[-1]["type"] == "tool_use" else "end_turn"}, "usage": {"output_tokens": 5}}, {"type": "message_stop"}]
    return "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()


def receipt(body):
    for message in reversed(body["messages"]):
        if not isinstance(message["content"], list):
            continue
        for block in reversed(message["content"]):
            if block.get("type") == "tool_result":
                return block
    return {}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=Path("output/claude-fork-cli") / uuid4().hex)
    args = parser.parse_args()
    artifact = args.output.resolve()
    workspace = artifact / "workspace"
    (workspace / "home").mkdir(parents=True)
    marker = "unique-full-parent-history-" + uuid4().hex
    signature = "native-signed-thinking-" + uuid4().hex
    requests, errors, counts = [], [], {}
    boundary = []
    cancelled_arrived = threading.Event()
    def tool(label, step, name, input):
        return {"type": "tool_use", "id": f"{label}-{step}", "name": name, "input": input}
    def response(body):
        nonlocal boundary
        # Only appended user text identifies children; inherited Agent tool input does not.
        user_text = []
        for message in body["messages"]:
            if message["role"] == "user":
                content = message["content"]
                user_text += [content] if isinstance(content, str) else [b.get("text", "") for b in content if b.get("type") == "text"]
        text = "\n".join(user_text)
        label = next((s for s in ["FORK_CANCEL_EXEC", "FORK_CHILD_EXEC", "CLEAN_CHILD_EXEC"] if s in text), "root")
        step = counts.get(label, 0)
        counts[label] = step + 1
        requests.append({"label": label, "step": step, "request": body})
        if label == "root" and step == 13:
            require(receipt(body).get("is_error", False) and "overrides are unsupported" in json.dumps(receipt(body)), "fork accepted unsupported harness override")
        else:
            require(not receipt(body).get("is_error", False), f"{label}/{step} failed tool: {receipt(body)}")
        if label != "root":
            if step == 0:
                if label.startswith("FORK"):
                    require(marker in json.dumps(body["messages"]), "fork lost unique parent history")
                    require(signature in json.dumps(body["messages"]), "fork lost native thinking signature")
                    require(body["model"] == "claude-sonnet-5-5", "fork changed model")
                    require(body["messages"][:len(boundary)] == boundary, "fork transcript prefix is not the complete native history")
                    require(not any(b.get("name") == "Agent" for b in body["messages"][-2].get("content", []) if isinstance(b, dict)), "incomplete Agent call leaked into fork boundary")
                else:
                    require(marker not in json.dumps(body["messages"]), "clean child inherited parent history")
            if label == "FORK_CHILD_EXEC" and step == 0:
                deadline = time.monotonic() + 10
                while not (workspace / "parent-available.txt").exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                require((workspace / "parent-available.txt").exists(), "fork blocked parent from making progress while child provider was pending")
            if label == "FORK_CANCEL_EXEC":
                cancelled_arrived.set()
                time.sleep(3)
                return [{"type": "text", "text": "cancelled-provider-late-response"}]
            if step == 0:
                return [tool(label, step, "TaskList", {})]
            if step == 1:
                require("parent-task-only" not in json.dumps(receipt(body)), "child shares parent task board")
                return [tool(label, step, "Bash", {"command": f"printf x >> {label}.txt"})]
            if step == 2:
                return [tool(label, step, "TaskCreate", {"subject": label + "-task-only", "description": "child independent board"})]
            if step == 3:
                return [tool(label, step, "SubmitResult", {"output": label + "-private-result"})]
            return [{"type": "text", "text": "Child complete."}]
        root = [
            ("Bash", {"command": "printf x >> parent-effect.txt"}),
            ("TaskCreate", {"subject": "parent-task-only", "description": "must not copy to child"}),
            ("Agent", {"description": "history fork", "prompt": "FORK_CHILD_EXEC inspect inherited conversation", "subagent_type": "fork", "model": "haiku"}),
            ("Bash", {"command": "printf parent-available > parent-available.txt"}),
            ("TaskOutput", {"task_id": "agent-1", "block": True, "timeout": 20000}),
            ("TaskGet", {"taskId": "1"}),
            ("Agent", {"description": "clean child", "prompt": "CLEAN_CHILD_EXEC fresh context", "run_in_background": True}),
            ("TaskOutput", {"task_id": "agent-2", "block": True, "timeout": 20000}),
            ("Agent", {"description": "cancel fork", "prompt": "FORK_CANCEL_EXEC wait for stop", "subagent_type": "fork"}),
            ("TaskStop", {"task_id": "agent-3"}),
            ("TaskOutput", {"task_id": "agent-3", "block": False}),
            ("TaskList", {}),
            ("Agent", {"description": "reject override", "prompt": "must never run", "subagent_type": "fork", "harness": "codex"}),
            ("Bash", {"command": "printf survived > parent-survived.txt"}),
        ]
        if step in (2, 8):
            boundary = body["messages"]
        if step == 4:
            require("FORK_CHILD_EXEC-private-result" not in json.dumps(body["messages"]), "background output leaked before TaskOutput")
        if step == 5:
            require("FORK_CHILD_EXEC-private-result" in json.dumps(receipt(body)), "TaskOutput did not retrieve child result")
        if step == 6:
            require("parent-task-only" in json.dumps(receipt(body)), "child replaced parent task board")
        if step == 9:
            require(cancelled_arrived.wait(10), "cancel child never reached actual provider")
        if step == 11:
            require("interrupted" in json.dumps(receipt(body)), "TaskStop did not interrupt child")
        if step == 12:
            require("parent-task-only" in json.dumps(receipt(body)) and "EXEC-task-only" not in json.dumps(receipt(body)), "parent board polluted by child")
        if step >= len(root):
            return [{"type": "text", "text": "native-fork-cli-complete"}]
        name, input = root[step]
        blocks = [tool(label, step, name, input)]
        if step == 0:
            blocks.insert(0, {"type": "thinking", "thinking": "Synthetic provider reasoning retained natively", "signature": signature})
        return blocks
    class Provider(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass
        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                wire = sse(response(body), body["model"])
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(wire)))
                self.end_headers()
                self.wfile.write(wire)
            except (BrokenPipeError, ConnectionResetError):
                pass
            except Exception as error:
                errors.append(str(error))
                self.send_error(500, str(error))
    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    command = [str(args.binary.resolve()), "run", "--claude", "--model", "claude-sonnet-5-5", "--thinking", "medium", "--claude-api-key", "synthetic", "--claude-messages-url", f"http://127.0.0.1:{server.server_port}/v1/messages", "--rollouts", "false", "--browser=none", "--mcp-defaults", "false", "--mcp-codex-config", "false", "--web-search", "false", "--image-generation", "false", "--memory", "false", "--subagents", "true", "--cwd", str(workspace), marker]
    env = {"HOME": str(workspace / "home"), "CODEX_HOME": str(workspace / "codex-home"), "PATH": os.environ["PATH"], "NANOCODEX_COMPUTER": "off"}
    (artifact / "scenario.json").write_text(json.dumps({"command": command, "expected": "native full-history fork, signed thinking, no effect replay, independent boards, clean spawn, actual stop, parent availability"}, indent=2))
    try:
        result = subprocess.run(command, cwd=workspace, env=env, capture_output=True, text=True, timeout=90)
        (artifact / "stdout.txt").write_text(result.stdout)
        (artifact / "stderr.txt").write_text(result.stderr)
        require(not errors, str(errors))
        require(result.returncode == 0, result.stderr)
        require("native-fork-cli-complete" in result.stdout, "missing final parent answer")
        for name in ["parent-effect", "FORK_CHILD_EXEC", "CLEAN_CHILD_EXEC"]:
            require((workspace / (name + ".txt")).read_text() == "x", f"{name} effect missing or replayed")
        require((workspace / "parent-available.txt").read_text() == "parent-available", "parent blocked")
        require((workspace / "parent-survived.txt").read_text() == "survived", "child cancellation affected parent")
        (artifact / "outcome.json").write_text(json.dumps({"success": True, "requests": counts, "signature": signature, "marker": marker, "errors": errors}, indent=2))
        print(artifact)
    finally:
        (artifact / "provider.json").write_text(json.dumps(requests, indent=2))
        (artifact / "errors.json").write_text(json.dumps(errors, indent=2))
        server.shutdown()


if __name__ == "__main__":
    main()
