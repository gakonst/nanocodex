import assert from "node:assert/strict";
import { test } from "node:test";

import { createCloudflareEventSocket } from "../cloudflare/event-socket.mjs";

const encoder = new TextEncoder();

class MemoryStorage {
  events = [];
  eventQueryRows = [];
  meta = { total_bytes: 0, stream_error: null };

  sql = {
    exec: (sql, ...args) => {
      const statement = sql.replace(/\s+/g, " ").trim();
      let rows = [];
      if (statement.startsWith("CREATE TABLE")) {
        // Schema setup is idempotent.
      } else if (statement.startsWith("INSERT OR IGNORE INTO nanocodex_cloudflare_event_meta")) {
        // The in-memory meta row exists from construction.
      } else if (statement.startsWith("SELECT total_bytes, stream_error")) {
        rows = [{ ...this.meta }];
      } else if (statement.startsWith("INSERT INTO nanocodex_cloudflare_events")) {
        const [event_json, created_at] = args;
        const cursor = String(this.events.length + 1);
        this.events.push({ cursor, event_json, created_at });
        rows = [{ cursor }];
      } else if (statement.startsWith("UPDATE nanocodex_cloudflare_event_meta SET total_bytes")) {
        this.meta.total_bytes += args[0];
      } else if (statement.startsWith("UPDATE nanocodex_cloudflare_event_meta SET stream_error")) {
        this.meta.stream_error = args[0];
      } else if (statement.startsWith("SELECT CAST(COALESCE(MAX(cursor)")) {
        rows = [{ cursor: this.events.at(-1)?.cursor ?? "0" }];
      } else if (statement.startsWith("SELECT CAST(cursor AS TEXT)")) {
        const after = BigInt(args[0]);
        rows = this.events.filter((event) => BigInt(event.cursor) > after).slice(0, 1);
        this.eventQueryRows.push(rows.length);
      } else {
        throw new Error(`unexpected SQL: ${statement}`);
      }
      return { toArray: () => rows };
    },
  };

  transactionSync(callback) { return callback(); }
}

class FakeSocket {
  attachment;
  closed;
  operations = [];
  readyState = 1;
  sent = [];
  tags = [];

  close(code, reason) {
    this.operations.push({ type: "close", code, reason });
    this.closed = { code, reason };
    this.readyState = 3;
  }

  send(message) {
    if (this.readyState !== 1) throw new Error("socket is closed");
    this.operations.push({ type: "send", message });
    this.sent.push(message);
  }

  serializeAttachment(value) {
    this.operations.push({ type: "attachment", value });
    this.attachment = value;
  }

  deserializeAttachment() { return this.attachment; }
}

class FakeWebSocketPair {
  constructor() {
    this[0] = new FakeSocket();
    this[1] = new FakeSocket();
  }
}

class FakeResponse {
  static json(value, init) {
    return new FakeResponse(JSON.stringify(value), {
      ...init,
      headers: { "content-type": "application/json", ...init?.headers },
    });
  }

  constructor(body, init = {}) {
    this.body = body;
    this.headers = new Headers(init.headers);
    this.status = init.status ?? 200;
    this.webSocket = init.webSocket;
  }
}

class MemoryContext {
  evictOnNextSocketRead = false;
  sockets = [];

  constructor(storage = new MemoryStorage()) {
    this.storage = storage;
  }

  acceptWebSocket(socket, tags) {
    socket.tags = tags;
    this.sockets.push(socket);
  }

  getWebSockets(tag) {
    if (this.evictOnNextSocketRead) {
      this.evictOnNextSocketRead = false;
      throw new Error("simulated Durable Object eviction");
    }
    return this.sockets.filter((socket) => socket.readyState === 1 && socket.tags.includes(tag));
  }
}

function withCloudflareWebSockets(callback) {
  const originalPair = globalThis.WebSocketPair;
  const OriginalResponse = globalThis.Response;
  Object.defineProperty(globalThis, "WebSocketPair", {
    configurable: true,
    value: FakeWebSocketPair,
  });
  Object.defineProperty(globalThis, "Response", {
    configurable: true,
    value: FakeResponse,
  });
  try {
    return callback();
  } finally {
    if (originalPair === undefined) delete globalThis.WebSocketPair;
    else Object.defineProperty(globalThis, "WebSocketPair", { configurable: true, value: originalPair });
    Object.defineProperty(globalThis, "Response", { configurable: true, value: OriginalResponse });
  }
}

function connect(events, cursor = "0") {
  return events.connect(new Request(
    `https://agent.internal/events?cursor=${cursor}`,
    { headers: { upgrade: "websocket" } },
  ));
}

function agentEvent(seq, payload = {}) {
  return {
    protocol_version: 1,
    request_id: "session-1",
    seq,
    type: seq === 1 ? "turn.started" : "turn.completed",
    payload,
  };
}

function decodedFrames(socket) {
  return socket.sent.map((message) => JSON.parse(message));
}

function eventCursors(socket) {
  return decodedFrames(socket).filter((frame) => frame.event).map((frame) => frame.cursor);
}

test("Cloudflare Agent events persist once and replay to every hibernatable client", () => {
  withCloudflareWebSockets(() => {
    const context = new MemoryContext();
    const events = createCloudflareEventSocket(context);
    const first = agentEvent(1, { input: "hello" });
    events.publish(first);

    const connected = connect(events);
    assert.equal(connected.status, 101);
    assert.equal(context.sockets.length, 1);
    assert.deepEqual(JSON.parse(context.sockets[0].sent[0]), { cursor: "1", event: first });
    assert.deepEqual(context.sockets[0].attachment, { version: 1, cursor: "1" });

    const second = agentEvent(2);
    events.publish(second);
    assert.deepEqual(JSON.parse(context.sockets[0].sent[1]), { cursor: "2", event: second });

    const restored = createCloudflareEventSocket(context);
    connect(restored, "1");
    assert.equal(context.sockets.length, 2);
    assert.deepEqual(JSON.parse(context.sockets[1].sent[0]), { cursor: "2", event: second });

    const invalid = restored.connect(new Request(
      "https://agent.internal/events?cursor=1&cursor=2",
      { headers: { upgrade: "websocket" } },
    ));
    assert.equal(invalid.status, 400);
  });
});

test("restored sockets replay a commit that was evicted before live fan-out", () => {
  withCloudflareWebSockets(() => {
    const context = new MemoryContext();
    const events = createCloudflareEventSocket(context);
    events.publish(agentEvent(1));
    connect(events);
    const socket = context.sockets[0];
    assert.deepEqual(eventCursors(socket), ["1"]);

    context.evictOnNextSocketRead = true;
    assert.throws(
      () => events.publish(agentEvent(2)),
      /simulated Durable Object eviction/,
    );
    assert.equal(context.storage.events.length, 2);
    assert.deepEqual(eventCursors(socket), ["1"]);

    const restored = createCloudflareEventSocket(context);
    assert.deepEqual(eventCursors(socket), ["1", "2"]);
    restored.publish(agentEvent(3));
    assert.deepEqual(eventCursors(socket), ["1", "2", "3"]);
    assert.deepEqual(socket.attachment, { version: 1, cursor: "3" });
  });
});

test("far-behind replay uses an event-count page and reconnect continuation", () => {
  withCloudflareWebSockets(() => {
    const storage = new MemoryStorage();
    const producer = createCloudflareEventSocket(new MemoryContext(storage));
    for (let seq = 1; seq <= 18; seq += 1) producer.publish(agentEvent(seq));

    const context = new MemoryContext(storage);
    const events = createCloudflareEventSocket(context);
    assert.equal(connect(events).status, 101);
    const firstPage = context.sockets[0];
    const frames = decodedFrames(firstPage);
    assert.deepEqual(eventCursors(firstPage), Array.from({ length: 16 }, (_, index) => String(index + 1)));
    assert.deepEqual(frames.at(-1), {
      type: "replay_paused",
      cursor: "16",
      latest_cursor: "18",
    });
    assert.deepEqual(firstPage.attachment, { version: 1, cursor: "16", replayPaused: true });
    assert.equal(firstPage.closed?.code, 1013);
    assert.ok(storage.eventQueryRows.every((rows) => rows <= 1));

    const pauseSend = firstPage.operations.findIndex((operation) => operation.type === "send"
      && JSON.parse(operation.message).type === "replay_paused");
    const pauseCheckpoint = firstPage.operations.findIndex((operation) => operation.type === "attachment"
      && operation.value.replayPaused === true);
    assert.ok(pauseSend >= 0 && pauseSend < pauseCheckpoint);

    assert.equal(connect(events, frames.at(-1).cursor).status, 101);
    const continuation = context.sockets[1];
    assert.deepEqual(eventCursors(continuation), ["17", "18"]);
    assert.equal(decodedFrames(continuation).some((frame) => frame.type === "replay_paused"), false);
    assert.equal(continuation.readyState, 1);
  });
});

test("replay byte pages stop before materializing or sending the retained tail", () => {
  withCloudflareWebSockets(() => {
    const storage = new MemoryStorage();
    const producer = createCloudflareEventSocket(new MemoryContext(storage));
    const chunk = "x".repeat(800 * 1024);
    for (let seq = 1; seq <= 3; seq += 1) producer.publish(agentEvent(seq, { chunk }));

    const context = new MemoryContext(storage);
    const events = createCloudflareEventSocket(context);
    connect(events);
    const firstPage = context.sockets[0];
    const frames = decodedFrames(firstPage);
    assert.deepEqual(eventCursors(firstPage), ["1", "2"]);
    assert.deepEqual(frames.at(-1), {
      type: "replay_paused",
      cursor: "2",
      latest_cursor: "3",
    });
    assert.ok(
      firstPage.sent.reduce((bytes, frame) => bytes + encoder.encode(frame).byteLength, 0)
        <= 2 * 1024 * 1024 + 1024,
    );
    assert.ok(storage.eventQueryRows.every((rows) => rows <= 1));

    connect(events, frames.at(-1).cursor);
    assert.deepEqual(eventCursors(context.sockets[1]), ["3"]);
  });
});
