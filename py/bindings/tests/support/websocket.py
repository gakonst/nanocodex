from __future__ import annotations

import base64
import hashlib
import json
import socket
import struct
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, Self

JsonObject = dict[str, Any]


@dataclass(frozen=True)
class RequestRecord:
    connection_id: int
    request_index: int
    body: JsonObject


class WebSocketConnection:
    def __init__(self, stream: socket.socket) -> None:
        self._stream = stream
        self._send_lock = threading.Lock()

    def recv_json(self) -> JsonObject | None:
        while True:
            frame = self._recv_frame()
            if frame is None:
                return None
            opcode, payload = frame
            if opcode == 0x1:
                return json.loads(payload)
            if opcode == 0x8:
                self._send_frame(0x8, payload)
                return None
            if opcode == 0x9:
                self._send_frame(0xA, payload)

    def send_json(self, value: JsonObject) -> None:
        self._send_frame(
            0x1,
            json.dumps(value, separators=(",", ":")).encode(),
        )

    def close(self) -> None:
        try:
            self._send_frame(0x8, b"")
        except OSError:
            pass
        self.shutdown()

    def shutdown(self) -> None:
        try:
            self._stream.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self._stream.close()

    def _recv_frame(self) -> tuple[int, bytes] | None:
        header = _recv_exact(self._stream, 2)
        if header is None:
            return None
        first, second = header
        opcode = first & 0x0F
        length = second & 0x7F
        if length == 126:
            encoded = _recv_exact(self._stream, 2)
            if encoded is None:
                return None
            length = struct.unpack("!H", encoded)[0]
        elif length == 127:
            encoded = _recv_exact(self._stream, 8)
            if encoded is None:
                return None
            length = struct.unpack("!Q", encoded)[0]
        mask = _recv_exact(self._stream, 4) if second & 0x80 else None
        if second & 0x80 and mask is None:
            return None
        payload = _recv_exact(self._stream, length)
        if payload is None:
            return None
        if mask is not None:
            payload = bytes(
                byte ^ mask[index % 4] for index, byte in enumerate(payload)
            )
        return opcode, payload

    def _send_frame(self, opcode: int, payload: bytes) -> None:
        first = bytes((0x80 | opcode,))
        length = len(payload)
        if length < 126:
            header = first + bytes((length,))
        elif length <= 0xFFFF:
            header = first + bytes((126,)) + struct.pack("!H", length)
        else:
            header = first + bytes((127,)) + struct.pack("!Q", length)
        with self._send_lock:
            self._stream.sendall(header + payload)


Handler = Callable[["MockResponsesServer", WebSocketConnection, RequestRecord], None]


class MockResponsesServer:
    """Small standard-library Responses WebSocket used by installed-wheel tests."""

    def __init__(self, handler: Handler | None = None) -> None:
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        listener.settimeout(0.1)
        self.endpoint = f"ws://127.0.0.1:{listener.getsockname()[1]}"
        self._listener = listener
        self._handler = handler or (
            lambda server, connection, record: server._default_handler(
                connection,
                record,
            )
        )
        self._condition = threading.Condition()
        self._requests: list[RequestRecord] = []
        self._connections: dict[int, WebSocketConnection] = {}
        self._workers: list[threading.Thread] = []
        self._errors: list[BaseException] = []
        self._next_connection_id = 0
        self._next_response_id = 0
        self._stopping = threading.Event()
        self._acceptor = threading.Thread(
            target=self._accept,
            name="mock-responses-accept",
            daemon=True,
        )
        self._acceptor.start()

    @property
    def requests(self) -> list[RequestRecord]:
        with self._condition:
            return list(self._requests)

    @property
    def connection_count(self) -> int:
        with self._condition:
            return self._next_connection_id

    def wait_for_requests(
        self, count: int, timeout: float = 5.0
    ) -> list[RequestRecord]:
        deadline = time.monotonic() + timeout
        with self._condition:
            while len(self._requests) < count and not self._errors:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(
                        f"received {len(self._requests)} of {count} expected requests"
                    )
                self._condition.wait(remaining)
            self._raise_errors()
            return list(self._requests)

    def respond_warmup(
        self,
        connection: WebSocketConnection,
        response_id: str | None = None,
    ) -> str:
        response_id = response_id or self.next_response_id("warmup")
        connection.send_json(
            {
                "type": "response.completed",
                "response": {"id": response_id, "usage": None},
            }
        )
        return response_id

    def respond_final(
        self,
        connection: WebSocketConnection,
        text: str = "done",
        response_id: str | None = None,
        *,
        usage: bool = True,
    ) -> str:
        response_id = response_id or self.next_response_id("final")
        response: JsonObject = {
            "id": response_id,
            "status": "completed",
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text}],
                }
            ],
            "usage": (
                {
                    "input_tokens": 10,
                    "input_tokens_details": {"cached_tokens": 5},
                    "output_tokens": 2,
                    "output_tokens_details": {"reasoning_tokens": 1},
                    "total_tokens": 12,
                }
                if usage
                else None
            ),
        }
        connection.send_json({"type": "response.completed", "response": response})
        return response_id

    def respond_compaction(
        self,
        connection: WebSocketConnection,
        response_id: str | None = None,
    ) -> str:
        response_id = response_id or self.next_response_id("compact")
        connection.send_json(
            {
                "type": "response.output_item.done",
                "item": {
                    "id": f"cmp-{response_id}",
                    "type": "compaction",
                    "encrypted_content": "opaque-python-test-summary",
                },
            }
        )
        connection.send_json(
            {
                "type": "response.completed",
                "response": {
                    "id": response_id,
                    "status": "completed",
                    "output": [],
                    "usage": {
                        "input_tokens": 100,
                        "input_tokens_details": {"cached_tokens": 40},
                        "output_tokens": 20,
                        "output_tokens_details": {"reasoning_tokens": 20},
                        "total_tokens": 120,
                    },
                },
            }
        )
        return response_id

    def next_response_id(self, label: str) -> str:
        with self._condition:
            self._next_response_id += 1
            return f"resp-{label}-{self._next_response_id}"

    def close(self) -> None:
        self._stopping.set()
        try:
            self._listener.close()
        except OSError:
            pass
        with self._condition:
            connections = list(self._connections.values())
        for connection in connections:
            connection.shutdown()
        self._acceptor.join(timeout=2)
        for worker in self._workers:
            worker.join(timeout=2)
        with self._condition:
            self._raise_errors()

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def _accept(self) -> None:
        while not self._stopping.is_set():
            try:
                stream, _ = self._listener.accept()
            except TimeoutError:
                continue
            except OSError:
                if self._stopping.is_set():
                    return
                raise
            with self._condition:
                self._next_connection_id += 1
                connection_id = self._next_connection_id
            worker = threading.Thread(
                target=self._serve,
                args=(connection_id, stream),
                name=f"mock-responses-{connection_id}",
                daemon=True,
            )
            self._workers.append(worker)
            worker.start()

    def _serve(self, connection_id: int, stream: socket.socket) -> None:
        connection: WebSocketConnection | None = None
        try:
            connection = WebSocketConnection(_handshake(stream))
            with self._condition:
                self._connections[connection_id] = connection
                self._condition.notify_all()
            request_index = 0
            while not self._stopping.is_set():
                body = connection.recv_json()
                if body is None:
                    return
                request_index += 1
                record = RequestRecord(connection_id, request_index, body)
                with self._condition:
                    self._requests.append(record)
                    self._condition.notify_all()
                self._handler(self, connection, record)
        except (ConnectionError, OSError):
            if not self._stopping.is_set():
                return
        # Preserve arbitrary handler/test failures for the owning test thread.
        except Exception as error:  # noqa: BLE001
            with self._condition:
                self._errors.append(error)
                self._condition.notify_all()
        finally:
            if connection is not None:
                connection.shutdown()
            else:
                stream.close()
            with self._condition:
                self._connections.pop(connection_id, None)
                self._condition.notify_all()

    def _default_handler(
        self,
        connection: WebSocketConnection,
        record: RequestRecord,
    ) -> None:
        body = record.body
        if body.get("generate") is False:
            self.respond_warmup(connection)
        elif body.get("input", [])[-1:] == [{"type": "compaction_trigger"}]:
            self.respond_compaction(connection)
        else:
            prompts = user_texts(body)
            self.respond_final(connection, prompts[-1] if prompts else "done")

    def _raise_errors(self) -> None:
        if self._errors:
            raise AssertionError("mock Responses server failed") from self._errors[0]


def user_texts(body: JsonObject) -> list[str]:
    texts: list[str] = []
    for item in body.get("input", []):
        if item.get("role") != "user":
            continue
        for content in item.get("content", []):
            if content.get("type") in {"input_text", "output_text"}:
                text = content.get("text")
                if isinstance(text, str):
                    texts.append(text)
    return texts


def _handshake(stream: socket.socket) -> socket.socket:
    request = bytearray()
    while b"\r\n\r\n" not in request:
        chunk = stream.recv(4096)
        if not chunk:
            raise ConnectionError("client closed before WebSocket handshake")
        request.extend(chunk)
    headers: dict[str, str] = {}
    for line in request.decode("latin-1").split("\r\n")[1:]:
        if ":" in line:
            name, value = line.split(":", 1)
            headers[name.strip().lower()] = value.strip()
    key = headers["sec-websocket-key"]
    accept = base64.b64encode(
        hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()
    ).decode()
    stream.sendall(
        (
            "HTTP/1.1 101 Switching Protocols\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {accept}\r\n"
            "\r\n"
        ).encode()
    )
    return stream


def _recv_exact(stream: socket.socket, length: int) -> bytes | None:
    output = bytearray()
    while len(output) < length:
        chunk = stream.recv(length - len(output))
        if not chunk:
            return None
        output.extend(chunk)
    return bytes(output)
