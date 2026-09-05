#!/usr/bin/env python3
"""Capture equivalent stock Codex and Nanocodex Code Mode request sequences."""

from __future__ import annotations

import argparse
import base64
import copy
import difflib
import hashlib
import json
import os
import re
import signal
import socket
import socketserver
import struct
import subprocess
import tempfile
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


@dataclass(frozen=True)
class Scenario:
    name: str
    prompt: str
    probe: str
    expected_requests: int = 2
    image_fixture: bool = False
    transport: str = "https"
    stateful: str | None = None


BASIC_PROBE = r"""
const command = await tools.exec_command({ cmd: "printf parity", login: false });
text(command.output);
const patch = await tools.apply_patch("*** Begin Patch\n*** Add File: parity.txt\n+parity\n*** End Patch");
text(patch);
"""
SHELL_CONTINUATION_PROBE = r"""
const command = await tools.exec_command({
  cmd: "read value; printf 'received:%s' \"$value\"",
  login: false,
  tty: true,
  yield_time_ms: 250,
});
text(command.session_id !== undefined);
const completed = await tools.write_stdin({
  session_id: command.session_id,
  chars: "parity\n",
  yield_time_ms: 5000,
});
text(completed.output);
if (completed.session_id !== undefined) {
  await tools.write_stdin({
    session_id: completed.session_id,
    chars: "",
    yield_time_ms: 1000,
  });
}
"""
FAILURE_PROBE = r"""
try {
  await tools.exec_command({ cmd: "true", yield_time_ms: -1 });
} catch (error) {
  text(error);
}
"""
IMAGE_PROBE = r"""
const result = await tools.view_image({ path: "pixel.png", detail: "original" });
image(result.image_url, result.detail);
text(result.detail);
"""
CANCELLATION_PROBE = r"""
const result = await tools.exec_command({
  cmd: "sh -c 'echo $$ > cancel-child.pid; sleep 3; echo leaked > cancel-leak.txt'",
  login: false,
  yield_time_ms: 30000,
});
text(result.output);
"""
SHELL_OUTPUT_LIMIT_PROBE = r"""
const result = await tools.exec_command({
  cmd: "i=1; while [ $i -le 80 ]; do printf 'line-%03d-xxxxxxxxxxxxxxxx\\n' \"$i\"; i=$((i+1)); done",
  login: false,
  max_output_tokens: 40,
});
text(result);
"""
SCENARIOS = (
    Scenario(
        name="basic",
        prompt="Run the requested parity probe and report only done.",
        probe=BASIC_PROBE,
    ),
    Scenario(
        name="shell_continuation",
        prompt="Run the shell continuation parity probe and report only done.",
        probe=SHELL_CONTINUATION_PROBE,
    ),
    Scenario(
        name="tool_failure",
        prompt="Run the tool failure parity probe and report only done.",
        probe=FAILURE_PROBE,
    ),
    Scenario(
        name="image",
        prompt="Run the image parity probe and report only done.",
        probe=IMAGE_PROBE,
        image_fixture=True,
    ),
    Scenario(
        name="reconnect_replay",
        prompt="Run the reconnect parity probe and report only done.",
        probe='text("continued");',
        expected_requests=4,
        transport="websocket",
        stateful="reconnect_replay",
    ),
    Scenario(
        name="compaction",
        prompt="Run the compaction parity probe and report only done.",
        probe='text("tool completed");',
        expected_requests=4,
        transport="websocket",
        stateful="compaction",
    ),
    Scenario(
        name="cancellation",
        prompt="Run the cancellation parity probe and report only done.",
        probe=CANCELLATION_PROBE,
        expected_requests=2,
        transport="websocket",
        stateful="cancellation",
    ),
    Scenario(
        name="shell_output_limit",
        prompt="Run the bounded shell-output parity probe and report only done.",
        probe=SHELL_OUTPUT_LIMIT_PROBE,
        stateful="shell_output_limit",
    ),
)
PIXEL_PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk"
    "YAAAAAYAAjCB0C8AAAAASUVORK5CYII="
)
APPROVAL_ONLY_DECLARATIONS = (
    '  // User-facing approval question for `require_escalated`; omit otherwise.\n'
    "  justification?: string;\n",
    "  // Reusable approval prefix for `cmd`, only with "
    '`sandbox_permissions: "require_escalated"`; for example ["git", "pull"].\n'
    "  prefix_rule?: Array<string>;\n",
    "  // Per-command sandbox override. Defaults to `use_default`; use "
    '`require_escalated` for unsandboxed execution.\n'
    '  sandbox_permissions?: "use_default" | "require_escalated";\n',
)
SUPPORTED_SURFACE_EXCLUSIONS = [
    "client/internal-turn request metadata and prompt-cache identity",
    "absolute workspace spelling, measured wall time, and opaque shell chunk IDs",
    "approval-only exec_command arguments (Nanocodex has no approval subsystem)",
    "the WebSocket-only response.create request envelope",
    (
        "stock Codex's final HTTPS fallback after a WebSocket abort; Nanocodex "
        "reconnects its supported Responses WebSocket and performs the same full replay"
    ),
]
PREFIXED_UUID = re.compile(
    r"^(?P<prefix>[A-Za-z][A-Za-z0-9]*)_"
    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"
)
ID_CAPABLE_ITEM_TYPES = frozenset(
    {
        "additional_tools",
        "message",
        "agent_message",
        "reasoning",
        "local_shell_call",
        "function_call",
        "function_call_output",
        "tool_search_call",
        "custom_tool_call",
        "custom_tool_call_output",
        "tool_search_output",
        "web_search_call",
        "image_generation_call",
        "compaction",
        "context_compaction",
    }
)


def sse(events: list[dict[str, Any]]) -> bytes:
    chunks = []
    for event in events:
        chunks.append(
            f"event: {event['type']}\ndata: {json.dumps(event, separators=(',', ':'))}\n\n"
        )
    return "".join(chunks).encode()


def response_created(response_id: str) -> dict[str, Any]:
    return {"type": "response.created", "response": {"id": response_id}}


def response_completed(
    response_id: str,
    *,
    input_tokens: int = 0,
) -> dict[str, Any]:
    return {
        "type": "response.completed",
        "response": {
            "id": response_id,
            "usage": {
                "input_tokens": input_tokens,
                "input_tokens_details": None,
                "output_tokens": 0,
                "output_tokens_details": None,
                "total_tokens": input_tokens,
            },
        },
    }


def tool_call_event(scenario: Scenario, call_id: str = "call-parity") -> dict[str, Any]:
    return {
        "type": "response.output_item.done",
        "item": {
            "id": f"ctc_{call_id}",
            "type": "custom_tool_call",
            "call_id": call_id,
            "name": "exec",
            "input": scenario.probe,
        },
    }


def assistant_event(response_id: str = "msg-parity") -> dict[str, Any]:
    return {
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "id": response_id,
            "content": [{"type": "output_text", "text": "done"}],
        },
    }


def scripted_responses(scenario: Scenario) -> list[bytes]:
    return [
        sse(
            [
                response_created("resp-parity-tool"),
                tool_call_event(scenario),
                response_completed("resp-parity-tool"),
            ]
        ),
        sse(
            [
                response_created("resp-parity-final"),
                assistant_event(),
                response_completed("resp-parity-final"),
            ]
        ),
    ]


class CaptureServer:
    def __init__(self, scenario: Scenario) -> None:
        self.requests: list[dict[str, Any]] = []
        self.transports: list[str] = []
        self.responses = scripted_responses(scenario)
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:  # noqa: N802
                length = int(self.headers.get("content-length", "0"))
                body = self.rfile.read(length)
                encoding = self.headers.get("content-encoding")
                if encoding:
                    self.send_error(415, f"unsupported request encoding {encoding}")
                    return
                try:
                    owner.requests.append(json.loads(body))
                    owner.transports.append("https")
                except json.JSONDecodeError:
                    self.send_error(400, "request body is not JSON")
                    return
                index = len(owner.requests) - 1
                if index >= len(owner.responses):
                    self.send_error(409, "unexpected extra Responses request")
                    return
                payload = owner.responses[index]
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, _format: str, *_args: object) -> None:
                return

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    @property
    def base_url(self) -> str:
        host, port = self.server.server_address
        return f"http://{host}:{port}/v1"

    def __enter__(self) -> CaptureServer:
        self.thread.start()
        return self

    def __exit__(self, *_args: object) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


class WebSocketConnection:
    def __init__(
        self,
        request: socket.socket,
        reader: Any,
        writer: Any,
        headers: dict[str, str],
    ) -> None:
        self.request = request
        self.reader = reader
        self.writer = writer
        self._handshake(headers)

    def _handshake(self, headers: dict[str, str]) -> None:
        key = headers.get("sec-websocket-key")
        if key is None:
            raise RuntimeError("websocket upgrade omitted Sec-WebSocket-Key")
        accept = base64.b64encode(
            hashlib.sha1(
                f"{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11".encode()
            ).digest()
        ).decode()
        self.writer.write(
            (
                "HTTP/1.1 101 Switching Protocols\r\n"
                "Upgrade: websocket\r\n"
                "Connection: Upgrade\r\n"
                f"Sec-WebSocket-Accept: {accept}\r\n"
                "x-reasoning-included: true\r\n"
                "openai-model: gpt-5.6-sol\r\n"
                "\r\n"
            ).encode()
        )
        self.writer.flush()

    def _read_exact(self, length: int) -> bytes:
        value = self.reader.read(length)
        if value is None or len(value) != length:
            raise EOFError("websocket frame ended unexpectedly")
        return value

    def receive_json(self) -> dict[str, Any]:
        while True:
            first, second = self._read_exact(2)
            opcode = first & 0x0F
            masked = bool(second & 0x80)
            length = second & 0x7F
            if length == 126:
                length = struct.unpack("!H", self._read_exact(2))[0]
            elif length == 127:
                length = struct.unpack("!Q", self._read_exact(8))[0]
            mask = self._read_exact(4) if masked else b""
            payload = self._read_exact(length)
            if masked:
                payload = bytes(
                    byte ^ mask[index % 4] for index, byte in enumerate(payload)
                )
            if opcode == 0x8:
                raise EOFError("websocket client sent a close frame")
            if opcode == 0x9:
                self._send_frame(0xA, payload)
                continue
            if opcode != 0x1:
                raise RuntimeError(f"unexpected websocket opcode {opcode}")
            value = json.loads(payload)
            if not isinstance(value, dict):
                raise RuntimeError("websocket request was not a JSON object")
            return value

    def send_json(self, value: dict[str, Any]) -> None:
        self._send_frame(
            0x1,
            json.dumps(value, separators=(",", ":")).encode(),
        )

    def _send_frame(self, opcode: int, payload: bytes) -> None:
        header = bytearray([0x80 | opcode])
        if len(payload) < 126:
            header.append(len(payload))
        elif len(payload) <= 0xFFFF:
            header.append(126)
            header.extend(struct.pack("!H", len(payload)))
        else:
            header.append(127)
            header.extend(struct.pack("!Q", len(payload)))
        self.writer.write(header)
        self.writer.write(payload)
        self.writer.flush()

    def abort(self) -> None:
        self.request.shutdown(socket.SHUT_RDWR)
        self.request.close()

    def close(self) -> None:
        try:
            self._send_frame(0x8, b"")
        except OSError:
            pass
        self.writer.close()
        self.reader.close()
        self.request.close()


class WebSocketScenarioServer:
    def __init__(self, scenario: Scenario) -> None:
        self.scenario = scenario
        self.requests: list[dict[str, Any]] = []
        self.transports: list[str] = []
        self.paths: list[str] = []
        self.errors: list[str] = []
        self.done = threading.Event()
        self.release = threading.Event()
        self.lock = threading.Lock()
        self.connection_count = 0
        owner = self

        class Server(socketserver.ThreadingTCPServer):
            allow_reuse_address = True
            daemon_threads = True

        class Handler(socketserver.BaseRequestHandler):
            def handle(self) -> None:
                try:
                    reader = self.request.makefile("rb")
                    writer = self.request.makefile("wb")
                    request_line = reader.readline()
                    if not request_line:
                        raise RuntimeError("client closed before sending an HTTP request")
                    method, path, _version = request_line.decode("latin-1").split(" ", 2)
                    headers: dict[str, str] = {}
                    while line := reader.readline():
                        if line == b"\r\n":
                            break
                        name, value = line.decode("latin-1").split(":", 1)
                        headers[name.strip().lower()] = value.strip()
                    if headers.get("upgrade", "").lower() != "websocket":
                        owner._handle_http(method, path, reader, writer, headers)
                        return
                    connection = WebSocketConnection(
                        self.request,
                        reader,
                        writer,
                        headers,
                    )
                    with owner.lock:
                        connection_index = owner.connection_count
                        owner.connection_count += 1
                    owner._handle_connection(connection, connection_index)
                except Exception as error:
                    owner.errors.append(f"{type(error).__name__}: {error}")
                    owner.done.set()

        self.server = Server(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    @property
    def base_url(self) -> str:
        host, port = self.server.server_address
        return f"http://{host}:{port}/v1"

    def _receive(self, connection: WebSocketConnection) -> dict[str, Any]:
        request = connection.receive_json()
        with self.lock:
            self.requests.append(request)
            self.transports.append("websocket")
            self.paths.append("/v1/responses")
        return request

    def _handle_http(
        self,
        method: str,
        path: str,
        reader: Any,
        writer: Any,
        headers: dict[str, str],
    ) -> None:
        if method != "POST":
            raise RuntimeError(f"unexpected HTTP fallback method {method}")
        length = int(headers.get("content-length", "0"))
        body = reader.read(length)
        request = json.loads(body)
        if not isinstance(request, dict):
            raise RuntimeError("HTTP fallback request was not a JSON object")
        with self.lock:
            self.requests.append(request)
            self.transports.append("https")
            self.paths.append(path)
        if path.endswith("/responses/compact"):
            payload = json.dumps(
                {
                    "output": [
                        {
                            "type": "compaction",
                            "encrypted_content": "opaque-summary",
                        }
                    ]
                },
                separators=(",", ":"),
            ).encode()
            writer.write(
                (
                    "HTTP/1.1 200 OK\r\n"
                    "Content-Type: application/json\r\n"
                    f"Content-Length: {len(payload)}\r\n"
                    "Connection: close\r\n"
                    "\r\n"
                ).encode()
            )
            writer.write(payload)
            writer.flush()
            return
        payload = sse(
            [
                response_created("resp-final"),
                assistant_event(),
                response_completed("resp-final"),
            ]
        )
        writer.write(
            (
                "HTTP/1.1 200 OK\r\n"
                "Content-Type: text/event-stream\r\n"
                f"Content-Length: {len(payload)}\r\n"
                "Connection: close\r\n"
                "\r\n"
            ).encode()
        )
        writer.write(payload)
        writer.flush()
        self.done.set()

    @staticmethod
    def _complete(
        connection: WebSocketConnection,
        response_id: str,
        items: list[dict[str, Any]],
        *,
        input_tokens: int = 0,
    ) -> None:
        connection.send_json(response_created(response_id))
        for item in items:
            connection.send_json(item)
        connection.send_json(
            response_completed(response_id, input_tokens=input_tokens)
        )

    def _handle_connection(
        self,
        connection: WebSocketConnection,
        connection_index: int,
    ) -> None:
        if self.scenario.stateful == "compaction":
            self._handle_compaction(connection, connection_index)
            return
        if self.scenario.stateful == "cancellation":
            self._handle_cancellation(connection, connection_index)
            return
        if self.scenario.stateful != "reconnect_replay":
            raise RuntimeError(f"unsupported stateful scenario {self.scenario.stateful}")
        if connection_index == 0:
            self._receive(connection)
            self._complete(connection, "resp-warmup", [])
            self._receive(connection)
            self._complete(
                connection,
                "resp-tool",
                [tool_call_event(self.scenario)],
            )
            self._receive(connection)
            connection.abort()
            return
        if connection_index == 1:
            self._receive(connection)
            self._complete(
                connection,
                "resp-final",
                [assistant_event()],
            )
            self.done.set()
            connection.close()
            return
        raise RuntimeError(f"unexpected websocket connection {connection_index}")

    def _handle_compaction(
        self,
        connection: WebSocketConnection,
        connection_index: int,
    ) -> None:
        if connection_index != 0:
            raise RuntimeError(f"unexpected websocket connection {connection_index}")
        self._receive(connection)
        self._complete(connection, "resp-warmup", [])
        self._receive(connection)
        self._complete(
            connection,
            "resp-tool",
            [tool_call_event(self.scenario)],
            input_tokens=372_001,
        )
        next_request = self._receive(connection)
        input_items = next_request.get("input")
        is_trigger = (
            isinstance(input_items, list)
            and bool(input_items)
            and input_items[-1] == {"type": "compaction_trigger"}
        )
        if is_trigger:
            self._complete(
                connection,
                "resp-compact",
                [
                    {
                        "type": "response.output_item.done",
                        "item": {
                            "id": "cmp-server-id",
                            "type": "compaction",
                            "encrypted_content": "opaque-summary",
                        },
                    }
                ],
                input_tokens=120,
            )
            self._receive(connection)
        self._complete(connection, "resp-final", [assistant_event()])
        self.done.set()
        connection.close()

    def _handle_cancellation(
        self,
        connection: WebSocketConnection,
        connection_index: int,
    ) -> None:
        if connection_index != 0:
            raise RuntimeError(f"unexpected websocket connection {connection_index}")
        self._receive(connection)
        self._complete(connection, "resp-warmup", [])
        self._receive(connection)
        self._complete(
            connection,
            "resp-cancel-tool",
            [tool_call_event(self.scenario, call_id="call-cancel")],
        )
        self.done.set()
        self.release.wait(timeout=15)
        connection.close()

    def __enter__(self) -> WebSocketScenarioServer:
        self.thread.start()
        return self

    def __exit__(self, *_args: object) -> None:
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


def run(command: list[str], environment: dict[str, str]) -> dict[str, Any]:
    try:
        completed = subprocess.run(
            command,
            check=False,
            capture_output=True,
            env=environment,
            stdin=subprocess.DEVNULL,
            text=True,
            timeout=30,
        )
    except subprocess.TimeoutExpired as error:
        return {
            "command": command,
            "returncode": 124,
            "stdout": error.stdout or "",
            "stderr": error.stderr or "process timed out after 30 seconds",
        }
    return {
        "command": command,
        "returncode": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
    }


def run_cancelled(
    command: list[str],
    environment: dict[str, str],
    workspace: Path,
) -> tuple[dict[str, Any], dict[str, Any]]:
    marker = workspace / "cancel-child.pid"
    leak = workspace / "cancel-leak.txt"
    process = subprocess.Popen(
        command,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    deadline = time.monotonic() + 10
    while not marker.exists() and process.poll() is None and time.monotonic() < deadline:
        time.sleep(0.02)
    marker_observed = marker.exists()
    if marker_observed and process.poll() is None:
        process.send_signal(signal.SIGINT)
    timed_out = False
    try:
        stdout, stderr = process.communicate(timeout=10)
    except subprocess.TimeoutExpired:
        timed_out = True
        process.kill()
        stdout, stderr = process.communicate()
    time.sleep(3.25)
    child_pid: int | None = None
    child_alive = False
    if marker_observed:
        try:
            child_pid = int(marker.read_text().strip())
            os.kill(child_pid, 0)
            child_alive = True
        except (OSError, ValueError):
            child_alive = False
    cancellation = {
        "marker_observed": marker_observed,
        "child_pid": child_pid,
        "child_alive_after_grace": child_alive,
        "leak_file_created": leak.exists(),
        "process_wait_timed_out": timed_out,
        "cleanup_valid": (
            marker_observed
            and not child_alive
            and not leak.exists()
            and not timed_out
        ),
    }
    return (
        {
            "command": command,
            "returncode": process.returncode,
            "stdout": stdout,
            "stderr": stderr,
        },
        cancellation,
    )


def stock_command(
    binary: Path,
    base_url: str,
    workspace: Path,
    scenario: Scenario,
) -> list[str]:
    supports_websockets = scenario.transport == "websocket"
    provider_name = "OpenAI" if scenario.stateful == "compaction" else "parity"
    provider = (
        "model_providers.parity={ "
        f'name = "{provider_name}", '
        f'base_url = "{base_url}", '
        'env_key = "OPENAI_API_KEY", '
        'wire_api = "responses", '
        "request_max_retries = 0, "
        "stream_max_retries = 0, "
        f"supports_websockets = {str(supports_websockets).lower()} "
        "}"
    )
    command = [
        str(binary),
        "exec",
        "--json",
        "--skip-git-repo-check",
        "--ignore-user-config",
        "--ignore-rules",
        "--ephemeral",
        "--sandbox",
        "danger-full-access",
        "-C",
        str(workspace),
        "-c",
        provider,
        "-c",
        'model_provider="parity"',
        "-c",
        'model="gpt-5.6-sol"',
        "-c",
        'model_reasoning_effort="medium"',
        "-c",
        'model_reasoning_summary="auto"',
        "-c",
        'approval_policy="never"',
        "-c",
        'web_search="disabled"',
        "-c",
        "features.code_mode=true",
        "-c",
        "features.code_mode_only=true",
        "--disable",
        "multi_agent",
        "-c",
        "tools.experimental_request_user_input.enabled=false",
        "-c",
        "agents.enabled=false",
        "-c",
        "skills.include_instructions=false",
        "-c",
        "skills.bundled.enabled=false",
        scenario.prompt,
    ]
    return command


def nanocodex_command(
    binary: Path,
    base_url: str,
    workspace: Path,
    scenario: Scenario,
) -> list[str]:
    command = [
        str(binary),
        "run",
        scenario.prompt,
        "--api-key",
        "parity",
        "--cwd",
        str(workspace),
        "--thinking",
        "medium",
        "--responses-transport",
        scenario.transport,
        "--api-base-url",
        base_url,
        "--store-responses",
        "false",
        "--rollouts",
        "false",
        "--web-search",
        "false",
        "--image-generation",
        "false",
        "--mcp-defaults",
        "false",
        "--log-filter",
        "warn",
    ]
    if scenario.transport == "websocket":
        command.extend(
            [
                "--websocket-url",
                re.sub(r"^http", "ws", f"{base_url}/responses"),
            ]
        )
    return command


def capture(
    command_factory: Callable[[Path, str, Path, Scenario], list[str]],
    binary: Path,
    workspace: Path,
    environment: dict[str, str],
    scenario: Scenario,
) -> dict[str, Any]:
    parity_file = workspace / "parity.txt"
    parity_file.unlink(missing_ok=True)
    (workspace / "cancel-child.pid").unlink(missing_ok=True)
    (workspace / "cancel-leak.txt").unlink(missing_ok=True)
    codex_home = workspace / ".codex-home"
    codex_home.mkdir(exist_ok=True)
    if scenario.image_fixture:
        (workspace / "pixel.png").write_bytes(PIXEL_PNG)
    environment = {**environment, "CODEX_HOME": str(codex_home)}
    server_factory: Callable[[], CaptureServer | WebSocketScenarioServer]
    if scenario.transport == "websocket":
        server_factory = lambda: WebSocketScenarioServer(scenario)
    else:
        server_factory = lambda: CaptureServer(scenario)
    with server_factory() as server:
        command = command_factory(binary, server.base_url, workspace, scenario)
        cancellation = None
        if scenario.stateful == "cancellation":
            process, cancellation = run_cancelled(command, environment, workspace)
            server.release.set()
        else:
            process = run(command, environment)
    requests = server.requests
    transports = server.transports
    paths = getattr(server, "paths", ["/v1/responses"] * len(requests))
    errors = getattr(server, "errors", [])
    return {
        "process": process,
        "requests": requests,
        "request_transports": transports,
        "request_paths": paths,
        "server_errors": errors,
        "cancellation": cancellation,
    }


def normalize(value: Any, workspace: Path, *, request_root: bool = False) -> Any:
    normalized = copy.deepcopy(value)
    if isinstance(normalized, dict):
        normalized.pop("client_metadata", None)
        normalized.pop("internal_chat_message_metadata_passthrough", None)
        normalized.pop("prompt_cache_key", None)
        normalized.pop("previous_response_id", None)
        if "chunk_id" in normalized:
            normalized["chunk_id"] = "<CHUNK_ID>"
        if "wall_time_seconds" in normalized:
            normalized["wall_time_seconds"] = "<SECONDS>"
        if request_root and normalized.get("type") == "response.create":
            normalized.pop("type")
        return {
            key: normalize(item, workspace)
            for key, item in normalized.items()
        }
    if isinstance(normalized, list):
        return [normalize(item, workspace) for item in normalized]
    if isinstance(normalized, str):
        normalized = normalized.replace(str(workspace.resolve()), "<WORKSPACE>")
        normalized = normalized.replace(str(workspace), "<WORKSPACE>")
        normalized = re.sub(
            r"Wall time \d+(?:\.\d+)? seconds",
            "Wall time <SECONDS> seconds",
            normalized,
        )
        normalized = re.sub(
            r'("chunk_id"\s*:\s*)"[^"]+"',
            r'\1"<CHUNK_ID>"',
            normalized,
        )
        normalized = re.sub(
            r'("wall_time_seconds"\s*:\s*)\d+(?:\.\d+)?',
            r'\1"<SECONDS>"',
            normalized,
        )
        for declaration in APPROVAL_ONLY_DECLARATIONS:
            normalized = normalized.replace(declaration, "")
        prefixed_uuid = PREFIXED_UUID.fullmatch(normalized)
        if prefixed_uuid is not None:
            return f"{prefixed_uuid.group('prefix')}_<UUID>"
        if normalized.lstrip().startswith(("{", "[")):
            try:
                nested = json.loads(normalized)
            except json.JSONDecodeError:
                pass
            else:
                if isinstance(nested, (dict, list)):
                    return json.dumps(
                        normalize(nested, workspace),
                        separators=(",", ":"),
                        sort_keys=True,
                    )
        return normalized
    return normalized


def request_diff(
    stock: list[dict[str, Any]],
    nano: list[dict[str, Any]],
    workspace: Path,
) -> str:
    stock_text = json.dumps(
        [normalize(request, workspace, request_root=True) for request in stock],
        indent=2,
        sort_keys=True,
    )
    nano_text = json.dumps(
        [normalize(request, workspace, request_root=True) for request in nano],
        indent=2,
        sort_keys=True,
    )
    return "\n".join(
        difflib.unified_diff(
            stock_text.splitlines(),
            nano_text.splitlines(),
            fromfile="stock-codex",
            tofile="nanocodex",
            lineterm="",
        )
    )


def stable_request_identity(
    requests: list[dict[str, Any]],
    paths: list[str],
    expected_requests: int,
) -> bool:
    if len(requests) != expected_requests or not requests:
        return False
    response_requests = [
        request
        for request, path in zip(requests, paths)
        if not path.endswith("/responses/compact")
    ]
    cache_keys = [request.get("prompt_cache_key") for request in response_requests]
    session_ids = [
        request.get("client_metadata", {}).get("session_id")
        for request in response_requests
    ]
    return (
        isinstance(cache_keys[0], str)
        and bool(cache_keys[0])
        and all(cache_key == cache_keys[0] for cache_key in cache_keys)
        and isinstance(session_ids[0], str)
        and bool(session_ids[0])
        and all(session_id == session_ids[0] for session_id in session_ids)
    )


def contains_input_type(request: dict[str, Any], item_type: str) -> bool:
    input_items = request.get("input")
    return isinstance(input_items, list) and any(
        isinstance(item, dict) and item.get("type") == item_type
        for item in input_items
    )


def previous_response_chain(requests: list[dict[str, Any]]) -> list[str | None]:
    return [request.get("previous_response_id") for request in requests]


def has_prefixed_item_id(item_id: Any) -> bool:
    return (
        isinstance(item_id, str)
        and "_" in item_id
        and all(item_id.split("_", 1))
    )


def may_omit_item_id(item: dict[str, Any]) -> bool:
    item_type = item.get("type")
    return (
        item_type in {"additional_tools", "compaction", "context_compaction"}
        or item_type == "compaction_trigger"
        or (item_type == "message" and item.get("role") == "developer")
    )


def outbound_item_id_policy_valid(capture: dict[str, Any]) -> bool:
    """Stable client IDs survive store:false while provider IDs stay private."""
    requests = capture["requests"]
    if not requests:
        return False
    for request, path in zip(requests, capture["request_paths"]):
        if path.endswith("/responses/compact"):
            continue
        if request.get("store") is not False:
            return False
        input_items = request.get("input")
        if not isinstance(input_items, list):
            return False
        for item in input_items:
            if not isinstance(item, dict):
                return False
            item_type = item.get("type")
            item_id = item.get("id")
            if item_id is None:
                if item_type in ID_CAPABLE_ITEM_TYPES and not may_omit_item_id(item):
                    return False
                continue
            if not has_prefixed_item_id(item_id):
                return False
    return True


def transport_paths_compatible(
    scenario: Scenario,
    stock: dict[str, Any],
    nano: dict[str, Any],
) -> bool:
    if (
        stock["request_transports"] == nano["request_transports"]
        and stock["request_paths"] == nano["request_paths"]
    ):
        return True
    return (
        scenario.stateful == "reconnect_replay"
        and stock["request_transports"]
        == ["websocket", "websocket", "websocket", "https"]
        and nano["request_transports"]
        == ["websocket", "websocket", "websocket", "websocket"]
        and stock["request_paths"] == nano["request_paths"]
        and stock["request_paths"] == ["/v1/responses"] * 4
    )


def shell_output_metadata_valid(capture: dict[str, Any]) -> bool:
    for request in reversed(capture["requests"]):
        for item in request.get("input", []):
            if item.get("type") != "custom_tool_call_output":
                continue
            for content in item.get("output", []):
                text_output = content.get("text")
                if not isinstance(text_output, str) or not text_output.startswith("{"):
                    continue
                try:
                    output = json.loads(text_output)
                except json.JSONDecodeError:
                    continue
                chunk_id = output.get("chunk_id")
                return (
                    isinstance(chunk_id, str)
                    and len(chunk_id) == 6
                    and all(character in "0123456789abcdef" for character in chunk_id)
                    and output.get("original_token_count") == 520
                    and "480 tokens truncated" in output.get("output", "")
                )
    return False


def stateful_behavior_valid(
    scenario: Scenario,
    stock: dict[str, Any],
    nano: dict[str, Any],
) -> bool:
    if scenario.stateful is None:
        return all(
            all(response_id is None for response_id in previous_response_chain(capture["requests"]))
            for capture in (stock, nano)
        )
    if scenario.stateful == "cancellation":
        return (
            stock["cancellation"]["cleanup_valid"]
            and nano["cancellation"]["cleanup_valid"]
            and previous_response_chain(stock["requests"])
            == [None, "resp-warmup"]
            and previous_response_chain(nano["requests"])
            == [None, "resp-warmup"]
        )
    if scenario.stateful == "shell_output_limit":
        return all(
            shell_output_metadata_valid(capture)
            and all(
                response_id is None
                for response_id in previous_response_chain(capture["requests"])
            )
            for capture in (stock, nano)
        )
    if scenario.stateful == "reconnect_replay":
        return all(
            len(capture["requests"]) == 4
            and previous_response_chain(capture["requests"])
            == [None, "resp-warmup", "resp-tool", None]
            and contains_input_type(
                capture["requests"][3],
                "custom_tool_call_output",
            )
            for capture in (stock, nano)
        )
    if scenario.stateful == "compaction":
        return all(
            len(capture["requests"]) == 4
            and previous_response_chain(capture["requests"])
            == [None, "resp-warmup", "resp-tool", None]
            and contains_input_type(capture["requests"][2], "compaction_trigger")
            and contains_input_type(capture["requests"][3], "compaction")
            for capture in (stock, nano)
        )
    return False


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--codex-bin", type=Path, required=True)
    parser.add_argument("--nanocodex-bin", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--check", action="store_true")
    parser.add_argument(
        "--scenario",
        action="append",
        choices=[scenario.name for scenario in SCENARIOS],
        help="Scenario to run; repeat for several. Defaults to all scenarios.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    environment = os.environ.copy()
    environment["OPENAI_API_KEY"] = "parity"
    selected = [
        scenario
        for scenario in SCENARIOS
        if args.scenario is None or scenario.name in args.scenario
    ]
    reports: list[dict[str, Any]] = []
    for scenario in selected:
        with tempfile.TemporaryDirectory(
            prefix=f"nanocodex-request-parity-{scenario.name}-"
        ) as directory:
            workspace = Path(directory)
            stock = capture(
                stock_command,
                args.codex_bin,
                workspace,
                environment,
                scenario,
            )
            nano = capture(
                nanocodex_command,
                args.nanocodex_bin,
                workspace,
                environment,
                scenario,
            )
            diff = request_diff(stock["requests"], nano["requests"], workspace)
            request_counts_valid = (
                len(stock["requests"]) == scenario.expected_requests
                and len(nano["requests"]) == scenario.expected_requests
            )
            request_identity_valid = stable_request_identity(
                stock["requests"],
                stock["request_paths"],
                scenario.expected_requests,
            ) and stable_request_identity(
                nano["requests"],
                nano["request_paths"],
                scenario.expected_requests,
            )
            transport_paths_equal = (
                stock["request_transports"] == nano["request_transports"]
                and stock["request_paths"] == nano["request_paths"]
            )
            paths_compatible = transport_paths_compatible(scenario, stock, nano)
            behavior_valid = stateful_behavior_valid(scenario, stock, nano)
            item_id_policy_valid = all(
                outbound_item_id_policy_valid(capture)
                for capture in (stock, nano)
            )
            if scenario.stateful == "cancellation":
                process_valid = (
                    stock["cancellation"]["cleanup_valid"]
                    and nano["cancellation"]["cleanup_valid"]
                    and not stock["server_errors"]
                    and not nano["server_errors"]
                )
            else:
                process_valid = (
                    not stock["process"]["returncode"]
                    and not nano["process"]["returncode"]
                    and not stock["server_errors"]
                    and not nano["server_errors"]
                )
            reports.append(
                {
                    "name": scenario.name,
                    "stock": stock,
                    "nanocodex": nano,
                    "normalized_requests_equal": not diff,
                    "request_counts_valid": request_counts_valid,
                    "request_identity_valid": request_identity_valid,
                    "transport_paths_equal": transport_paths_equal,
                    "transport_paths_compatible": paths_compatible,
                    "process_valid": process_valid,
                    "stateful_behavior_valid": behavior_valid,
                    "outbound_item_id_policy_valid": item_id_policy_valid,
                    "normalized_diff": diff,
                }
            )
    all_processes_valid = all(
        report["process_valid"]
        for report in reports
    )
    all_counts_valid = all(report["request_counts_valid"] for report in reports)
    all_identities_valid = all(report["request_identity_valid"] for report in reports)
    all_requests_equal = all(report["normalized_requests_equal"] for report in reports)
    all_transport_paths_equal = all(
        report["transport_paths_equal"] for report in reports
    )
    all_transport_paths_compatible = all(
        report["transport_paths_compatible"] for report in reports
    )
    all_stateful_behaviors_valid = all(
        report["stateful_behavior_valid"] for report in reports
    )
    all_outbound_item_id_policies_valid = all(
        report["outbound_item_id_policy_valid"] for report in reports
    )
    report = {
        "schema_version": 4,
        "scenarios": reports,
        "all_processes_valid": all_processes_valid,
        "all_request_counts_valid": all_counts_valid,
        "all_request_identities_valid": all_identities_valid,
        "all_normalized_requests_equal": all_requests_equal,
        "all_transport_paths_equal": all_transport_paths_equal,
        "all_transport_paths_compatible": all_transport_paths_compatible,
        "all_stateful_behaviors_valid": all_stateful_behaviors_valid,
        "all_outbound_item_id_policies_valid": all_outbound_item_id_policies_valid,
        "supported_surface_exclusions": SUPPORTED_SURFACE_EXCLUSIONS,
    }
    if args.output:
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            {
                "scenarios": [
                    {
                        "name": scenario["name"],
                        "stock_returncode": scenario["stock"]["process"]["returncode"],
                        "stock_request_count": len(scenario["stock"]["requests"]),
                        "nanocodex_returncode": scenario["nanocodex"]["process"][
                            "returncode"
                        ],
                        "nanocodex_request_count": len(
                            scenario["nanocodex"]["requests"]
                        ),
                        "normalized_requests_equal": scenario[
                            "normalized_requests_equal"
                        ],
                        "request_counts_valid": scenario["request_counts_valid"],
                        "request_identity_valid": scenario["request_identity_valid"],
                        "transport_paths_equal": scenario["transport_paths_equal"],
                        "transport_paths_compatible": scenario[
                            "transport_paths_compatible"
                        ],
                        "process_valid": scenario["process_valid"],
                        "stateful_behavior_valid": scenario[
                            "stateful_behavior_valid"
                        ],
                        "outbound_item_id_policy_valid": scenario[
                            "outbound_item_id_policy_valid"
                        ],
                        "stock_cancellation": scenario["stock"]["cancellation"],
                        "nanocodex_cancellation": scenario["nanocodex"][
                            "cancellation"
                        ],
                    }
                    for scenario in reports
                ],
                "all_processes_valid": all_processes_valid,
                "all_request_counts_valid": all_counts_valid,
                "all_request_identities_valid": all_identities_valid,
                "all_normalized_requests_equal": all_requests_equal,
                "all_transport_paths_equal": all_transport_paths_equal,
                "all_transport_paths_compatible": all_transport_paths_compatible,
                "all_stateful_behaviors_valid": all_stateful_behaviors_valid,
                "all_outbound_item_id_policies_valid": (
                    all_outbound_item_id_policies_valid
                ),
            },
            indent=2,
        )
    )
    if (
        not all_processes_valid
        or not all_counts_valid
        or not all_identities_valid
        or not all_stateful_behaviors_valid
        or not all_outbound_item_id_policies_valid
    ):
        return 1
    if args.check and (
        not all_requests_equal or not all_transport_paths_compatible
    ):
        for scenario in reports:
            if scenario["normalized_diff"]:
                print(f"\n## {scenario['name']}\n{scenario['normalized_diff']}")
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
