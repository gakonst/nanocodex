import assert from "node:assert/strict";
import { test } from "node:test";

import { openHostManagedWebSocket } from "../browser/hostManagedWebSocket.mjs";

class FakeWebSocket {
  static nextMessage = { type: "nanocodex.proxy.ready" };
  static openedUrl;

  constructor(url) {
    FakeWebSocket.openedUrl = String(url);
    this.listeners = new Map();
    this.closed = false;
    queueMicrotask(() => this.emit("message", {
      data: JSON.stringify(FakeWebSocket.nextMessage),
    }));
  }

  addEventListener(type, listener) {
    const listeners = this.listeners.get(type) ?? new Set();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(type, listener) {
    this.listeners.get(type)?.delete(listener);
  }

  emit(type, event) {
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }

  close() { this.closed = true; }
}

test("host-managed sockets bind the session and consume the proxy readiness frame", async () => {
  FakeWebSocket.nextMessage = { type: "nanocodex.proxy.ready" };
  const socket = await openHostManagedWebSocket(
    "wss://nanocodex.example/api/responses",
    "session-1",
    { WebSocketImpl: FakeWebSocket },
  );

  assert.equal(
    FakeWebSocket.openedUrl,
    "wss://nanocodex.example/api/responses?session_id=session-1&thread_id=session-1",
  );
  assert.equal(socket.closed, false);
});

test("host-managed socket rejection preserves retry metadata", async () => {
  FakeWebSocket.nextMessage = {
    type: "nanocodex.proxy.rejected",
    status: 429,
    error: "session_rate_limit_exceeded",
    retryAfter: "60",
  };

  await assert.rejects(
    openHostManagedWebSocket(
      "wss://nanocodex.example/api/responses",
      "session-1",
      { WebSocketImpl: FakeWebSocket },
    ),
    (error) => {
      assert.equal(error.status, 429);
      assert.equal(error.body, "session_rate_limit_exceeded");
      assert.equal(error.retryAfter, 60);
      return true;
    },
  );
});
