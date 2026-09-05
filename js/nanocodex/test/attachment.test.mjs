import assert from "node:assert/strict";
import test from "node:test";

import { providerSource, ToolRouter, toolRouterBrand, toolRouterRuntime } from "../runtime/tool-router.mjs";
import { createTools } from "../tools/Tools.mjs";
import { createAttachment } from "../tools/attachment.mjs";

function reverseTarget(connect, endpoint = "wss://managed.test/tools") {
  return { endpoint, transport: { connect } };
}

test("attachment publishes one exact catalog and exchanges ready, call, result, and ack", async () => {
  const socket = new FakeSocket();
  let context;
  const tools = await createTools({ tools: {
    echo: {
      description: "Echo one value.",
      strict: true,
      parameters: { type: "object", properties: { value: { type: "string" } }, required: ["value"], additionalProperties: false },
      outputSchema: { type: "object", properties: { value: { type: "string" } }, required: ["value"], additionalProperties: false },
      supportsParallelToolCalls: true,
      handler: ({ value }, received) => { context = received; return { value }; },
    },
  } });
  const connector = createAttachment(tools, reverseTarget(async (target) => {
    assert.equal(target, "wss://managed.test/tools");
    return socket;
  }), { reconnect: false });
  const connecting = connector.connect();
  await waitFor(() => socket.frames().length === 1);
  assert.deepEqual(socket.frames()[0], {
    type: "catalog",
    tools: [{
      provider: "javascript",
      remote_name: "echo",
      definition: {
        type: "function",
        name: "echo",
        description: "Echo one value.",
        strict: true,
        parameters: { type: "object", properties: { value: { type: "string" } }, required: ["value"], additionalProperties: false },
        output_schema: { type: "object", properties: { value: { type: "string" } }, required: ["value"], additionalProperties: false },
      },
      parallel_safe: true,
      timeout_ms: 120_000,
    }],
  });
  socket.receive({ type: "ready" });
  const client = await connecting;
  socket.receive(callFrame({ value: "hello" }));
  await waitFor(() => socket.frames().some(({ type }) => type === "result"));
  assert.equal(context.model, "gpt-5.6-sol");
  assert.deepEqual(lastFrame(socket, "result"), {
    type: "result",
    call_id: "call:1",
    outcome: {
      status: "completed",
      output: {
        output: '{"value":"hello"}', success: true,
        structured_result: { value: "hello" }, metadata: null, process_trace: null,
      },
    },
  });
  socket.receive({ type: "ack", call_id: "call:1" });
  await drain(client, socket);
  await tools.close();
});

test("catalog preserves provider, remote name, summary, and timeout metadata", async () => {
  const tools = await createTools({ tools: {
    local: {
      description: "Local.", provider: "local-provider", remoteName: "remote-local",
      summary: "search metadata", timeoutMs: 9_000, handler: () => "ok",
    },
  } });
  const socket = new FakeSocket();
  const connector = createAttachment(tools, reverseTarget(async () => socket), { reconnect: false, provider: "fallback-provider" });
  const connecting = connector.connect();
  await waitFor(() => socket.frames().length === 1);
  assert.deepEqual(socket.frames()[0].tools[0], {
    provider: "local-provider", remote_name: "remote-local",
    definition: {
      type: "function", name: "local", description: "Local.", strict: false,
      parameters: { type: "object", additionalProperties: true },
    },
    parallel_safe: false, summary: "search metadata", timeout_ms: 9_000,
  });
  socket.receive({ type: "ready" });
  await drain(await connecting, socket);
  await tools.close();
});

test("Tools publishes its non-secret user-machine snapshot with each attachment", async () => {
  const tools = await createTools({
    attachmentId: "laptop",
    machines: [{
      id: "laptop",
      name: "George's laptop",
      workspace: "/Users/george/project",
      capabilities: ["filesystem", "native-shell"],
    }],
  });
  const socket = new FakeSocket();
  const connector = tools.attach(reverseTarget(async () => socket));
  const connecting = connector.connect();
  await waitFor(() => socket.frames().length === 1);
  assert.deepEqual(socket.frames()[0], {
    type: "catalog",
    tools: [],
    attachment_id: "laptop",
    machines: [{
      id: "laptop",
      name: "George's laptop",
      workspace: "/Users/george/project",
      capabilities: ["filesystem", "native-shell"],
    }],
  });
  socket.receive({ type: "ready" });
  await drain(await connecting, socket);
  await tools.close();
});

test("independent Tools runtimes publish distinct attachment identifiers", async () => {
  const firstTools = await createTools({ attachmentId: "machine:first" });
  const secondTools = await createTools({ attachmentId: "machine:second" });
  const firstSocket = new FakeSocket();
  const secondSocket = new FakeSocket();
  const firstConnecting = firstTools.attach(reverseTarget(async () => firstSocket)).connect();
  const secondConnecting = secondTools.attach(reverseTarget(async () => secondSocket)).connect();
  await waitFor(() => firstSocket.frames().length === 1 && secondSocket.frames().length === 1);
  assert.equal(firstSocket.frames()[0].attachment_id, "machine:first");
  assert.equal(secondSocket.frames()[0].attachment_id, "machine:second");
  firstSocket.receive({ type: "ready" });
  secondSocket.receive({ type: "ready" });
  const [firstClient, secondClient] = await Promise.all([firstConnecting, secondConnecting]);
  await Promise.all([drain(firstClient, firstSocket), drain(secondClient, secondSocket)]);
  await Promise.all([firstTools.close(), secondTools.close()]);
});

test("machine metadata is bounded and cannot carry arbitrary fields", async () => {
  const machine = {
    id: "laptop",
    name: "Laptop",
    workspace: "/workspace",
    capabilities: [],
  };
  for (const [machines, message] of [
    [[{ ...machine, token: "secret" }], /unsupported field token/],
    [[{ ...machine, capabilities: ["filesystem", "filesystem"] }], /must be unique/],
    [[machine, { ...machine, id: "desktop" }], /at most 1/],
    [[{ ...machine, capabilities: Array.from({ length: 65 }, (_, index) => `capability:${index}`) }], /safe identifiers/],
  ]) {
    await assert.rejects(createTools({ attachmentId: "laptop", machines }), message);
  }
  for (const attachmentId of ["", "unsafe id", "é", "x".repeat(124), 1]) {
    await assert.rejects(
      createTools({ attachmentId }),
      /attachmentId must be a safe identifier of at most 123 bytes/,
    );
  }
  await assert.rejects(createTools({ machines: [machine] }), /id equals attachmentId/);
  await assert.rejects(
    createTools({ attachmentId: "desktop", machines: [machine] }),
    /id equals attachmentId/,
  );
});

test("in-flight cancellation uses an ordinary ambiguous result and receipt ack path", async () => {
  let admitted;
  const fixture = await readyAttachment({
    handler: (_input, { signal }) => new Promise((_resolve, reject) => {
      admitted = true;
      signal.addEventListener("abort", () => reject(signal.reason), { once: true });
    }),
  });
  fixture.socket.receive(callFrame({ value: 1 }));
  await waitFor(() => admitted);
  fixture.socket.receive({ type: "cancel", call_id: "call:1" });
  await waitFor(() => fixture.socket.frames().some(({ type }) => type === "result"));
  assert.deepEqual(lastFrame(fixture.socket, "result"), {
    type: "result", call_id: "call:1",
    outcome: { status: "ambiguous", message: "tool execution was cancelled after dispatch" },
  });
  assert.equal(fixture.socket.frames().some(({ type }) => type === "cancel_ack"), false);
  fixture.socket.receive({ type: "ack", call_id: "call:1" });
  await drain(fixture.client, fixture.socket);
  await fixture.tools.close();
});

test("pre-dispatch cancellation is idempotent and remains authoritative", async () => {
  let calls = 0;
  const fixture = await readyAttachment({ handler: () => { calls++; return "unexpected"; } });
  fixture.socket.receive({ type: "cancel", call_id: "call:1" });
  await waitFor(() => fixture.socket.frames().some(({ type }) => type === "result"));
  fixture.socket.receive(callFrame({}));
  await waitFor(() => fixture.socket.frames().filter(({ type }) => type === "result").length === 2);
  assert.equal(calls, 0);
  assert.equal(lastFrame(fixture.socket, "result").outcome.status, "cancelled");
  fixture.socket.receive({ type: "ack", call_id: "call:1" });
  await drain(fixture.client, fixture.socket);
  await fixture.tools.close();
});

test("graceful drain waits for dispatched calls and their acknowledgements", async () => {
  let finish;
  const fixture = await readyAttachment({ handler: () => new Promise((resolve) => { finish = resolve; }) });
  fixture.socket.receive(callFrame({}));
  await tick();
  const closing = fixture.client.close();
  assert.deepEqual(lastFrame(fixture.socket, "drain"), { type: "drain" });
  fixture.socket.receive({ type: "draining" });
  await tick();
  assert.equal(fixture.socket.closed, undefined);
  finish("done");
  await waitFor(() => fixture.socket.frames().some(({ type }) => type === "result"));
  assert.equal(fixture.socket.closed, undefined);
  fixture.socket.receive({ type: "ack", call_id: "call:1" });
  await closing;
  assert.deepEqual(fixture.socket.closed, { code: 1000, reason: "tool attachment drained" });
  await fixture.tools.close();
});

test("a call already crossing the socket is accepted until the draining barrier", async () => {
  const fixture = await readyAttachment({ handler: () => "crossed" });
  const closing = fixture.client.close();
  await waitFor(() => fixture.socket.frames().some(({ type }) => type === "drain"));
  fixture.socket.receive(callFrame({}));
  fixture.socket.receive({ type: "draining" });
  await waitFor(() => fixture.socket.frames().some(({ type }) => type === "result"));
  assert.equal(lastFrame(fixture.socket, "result").outcome.status, "completed");
  fixture.socket.receive({ type: "ack", call_id: "call:1" });
  await closing;
  await fixture.tools.close();
});

test("a call already racing the drain handshake completes without fencing", async () => {
  let calls = 0;
  const fixture = await readyAttachment({ handler: () => { calls++; return "done"; } });
  const closing = fixture.client.close();
  fixture.socket.receive(callFrame({}));
  await waitFor(() => fixture.socket.frames().some(({ type }) => type === "result"));
  assert.equal(calls, 1);
  assert.equal(fixture.socket.closed, undefined);
  fixture.socket.receive({ type: "draining" });
  fixture.socket.receive({ type: "ack", call_id: "call:1" });
  await closing;
  assert.equal(fixture.socket.closed.code, 1000);
  await fixture.tools.close();
});

test("Tools close drains owned attachments before disposing their admitted tools", async () => {
  let finish;
  let admitted = false;
  let disposed = false;
  const socket = new FakeSocket();
  const tools = await createTools({ tools: { echo: {
    description: "Echo.",
    parameters: { type: "object", additionalProperties: true },
    handler: () => {
      admitted = true;
      return new Promise((resolve) => { finish = resolve; });
    },
    dispose: () => { disposed = true; },
  } } });
  const connector = tools.attach(reverseTarget(async () => socket));
  const connecting = connector.connect();
  await waitFor(() => socket.frames().some(({ type }) => type === "catalog"));
  socket.receive({ type: "ready" });
  await connecting;
  socket.receive(callFrame({}));
  await waitFor(() => admitted);

  const closing = tools.close();
  await waitFor(() => socket.frames().some(({ type }) => type === "drain"));
  socket.receive({ type: "draining" });
  await tick();
  assert.equal(disposed, false);
  finish("done");
  await waitFor(() => socket.frames().some(({ type }) => type === "result"));
  assert.equal(disposed, false);
  socket.receive({ type: "ack", call_id: "call:1" });
  await closing;
  assert.equal(disposed, true);
});

test("a throwing drain send still closes once and settles every waiter", async () => {
  const socket = new ThrowingDrainSocket();
  const fixture = await readyAttachment({ handler: () => "ok" }, socket);
  const first = fixture.client.close();
  const second = fixture.client.close();
  assert.equal(first, second);
  await first;
  assert.deepEqual(socket.frames().map(({ type }) => type), ["catalog", "drain"]);
  assert.equal(socket.closed.code, 1011);
  assert.match(socket.closed.reason, /drain failed/);
  await fixture.tools.close();
});

test("logical close settles when an injected socket emits no close event", async () => {
  const socket = new SilentCloseSocket();
  const fixture = await readyAttachment({ handler: () => "ok" }, socket);
  const closing = fixture.client.close();
  fixture.socket.receive({ type: "draining" });
  await closing;
  assert.deepEqual(socket.closed, { code: 1000, reason: "tool attachment drained" });
  await fixture.tools.close();
});

test("connector close does not wait for provider settlement", async () => {
  let settle;
  const settled = new Promise((resolve) => { settle = resolve; });
  const router = new ToolRouter([providerSource("pending", {
    definitions: () => [],
    resolve: () => undefined,
    settled: () => settled,
  })]);
  const owner = { [toolRouterBrand]: true, [toolRouterRuntime]: router };
  const connector = createAttachment(owner, reverseTarget(async () => new FakeSocket()), { reconnect: false });
  const connecting = connector.connect();
  await connector.close();
  settle();
  await assert.rejects(connecting, /connector is closed/);
  await router.reset();
});

test("duplicate calls are idempotent and changed immutable identity rejects the socket", async () => {
  let finish;
  let calls = 0;
  const fixture = await readyAttachment({ handler: () => { calls++; return new Promise((resolve) => { finish = resolve; }); } });
  const first = callFrame({ id: 1 });
  fixture.socket.receive(first);
  fixture.socket.receive(first);
  await tick();
  assert.equal(calls, 1);
  fixture.socket.receive({ ...first, input: { id: 2 } });
  await waitFor(() => fixture.socket.closed?.code === 1008);
  assert.match(fixture.socket.closed.reason, /different immutable fields/);
  finish?.("late");
  await fixture.tools.close();
});

test("expired, invalid, and oversized post-dispatch outcomes preserve semantics", async () => {
  let calls = 0;
  const expired = await readyAttachment({ handler: () => { calls++; return "unexpected"; } });
  expired.socket.receive({ ...callFrame({}), deadline_at: Date.now() - 1 });
  await waitFor(() => expired.socket.frames().some(({ type }) => type === "result"));
  assert.equal(calls, 0);
  assert.equal(lastFrame(expired.socket, "result").outcome.status, "unavailable");
  expired.socket.receive({ type: "ack", call_id: "call:1" });
  await drain(expired.client, expired.socket);
  await expired.tools.close();

  const invalid = await readyAttachment({ handler: () => 1n });
  invalid.socket.receive(callFrame({}));
  await waitFor(() => invalid.socket.frames().some(({ type }) => type === "result"));
  assert.equal(lastFrame(invalid.socket, "result").outcome.status, "ambiguous");
  invalid.socket.receive({ type: "ack", call_id: "call:1" });
  await drain(invalid.client, invalid.socket);
  await invalid.tools.close();

  const oversized = await readyAttachment({ handler: () => "too large" });
  oversized.socket.receive({ ...callFrame({}), output_byte_budget: 1 });
  await waitFor(() => oversized.socket.frames().some(({ type }) => type === "result"));
  assert.equal(lastFrame(oversized.socket, "result").outcome.status, "ambiguous");
  oversized.socket.receive({ type: "ack", call_id: "call:1" });
  await drain(oversized.client, oversized.socket);
  await oversized.tools.close();
});

test("heartbeat uses exact ping and pong nonce frames", async () => {
  const socket = new FakeSocket();
  const tools = await createTools();
  const connector = createAttachment(tools, reverseTarget(async () => socket), { reconnect: false, heartbeatMs: 5 });
  const connecting = connector.connect();
  await waitFor(() => socket.frames().some(({ type }) => type === "catalog"));
  socket.receive({ type: "ready" });
  const client = await connecting;
  await waitForTimer(() => socket.frames().some(({ type }) => type === "ping"));
  const ping = lastFrame(socket, "ping");
  assert.deepEqual(Object.keys(ping), ["type", "nonce"]);
  socket.receive({ type: "pong", nonce: ping.nonce });
  await tick();
  await drain(client, socket);
  await tools.close();
});

test("graceful drain keeps the attachment lease alive until calls are acknowledged", async () => {
  let finish;
  const socket = new FakeSocket();
  const tools = await createTools({ tools: {
    echo: { handler: () => new Promise((resolve) => { finish = resolve; }) },
  } });
  const connector = createAttachment(tools, reverseTarget(async () => socket), {
    reconnect: false,
    heartbeatMs: 5,
  });
  const connecting = connector.connect();
  await waitFor(() => socket.frames().some(({ type }) => type === "catalog"));
  socket.receive({ type: "ready" });
  const client = await connecting;
  socket.receive(callFrame({}));
  await waitFor(() => finish !== undefined);
  const closing = client.close();
  socket.receive({ type: "draining" });
  await waitForTimer(() => socket.frames().some(({ type }) => type === "ping"));
  const ping = lastFrame(socket, "ping");
  socket.receive({ type: "pong", nonce: ping.nonce });
  finish("done");
  await waitFor(() => socket.frames().some(({ type }) => type === "result"));
  socket.receive({ type: "ack", call_id: "call:1" });
  await closing;
  await tools.close();
});

test("old protocol fields and unknown acknowledgements reject via close code and reason", async () => {
  const old = await readyAttachment({ handler: () => "ok" });
  old.socket.receive({ type: "ready", protocol_version: 1 });
  await waitFor(() => old.socket.closed?.code === 1008);
  assert.match(old.socket.closed.reason, /unsupported field protocol_version/);
  await old.tools.close();

  const unknown = await readyAttachment({ handler: () => "ok" });
  unknown.socket.receive({ type: "ack", call_id: "unknown" });
  await waitFor(() => unknown.socket.closed?.code === 1008);
  assert.match(unknown.socket.closed.reason, /retained terminal result/);
  await unknown.tools.close();
});

test("ready before catalog publication is rejected", async () => {
  const socket = new FakeSocket();
  socket.readyState = 0;
  const tools = await createTools();
  const connector = createAttachment(tools, reverseTarget(async () => socket), { reconnect: false });
  const connecting = connector.connect();
  await waitFor(() => socket.listeners.has("message"));
  socket.receive({ type: "ready" });
  await assert.rejects(connecting, /rejected.*catalog handshake/);
  assert.equal(socket.closed.code, 1008);
  await tools.close();
});

test("result send failure is a reconnectable transport close, not a policy rejection", async () => {
  const socket = new FakeSocket();
  const fixture = await readyAttachment({ handler: () => "done" }, socket);
  socket.throwOnType = "result";
  socket.receive(callFrame({}));
  await fixture.client.closed();
  assert.equal(socket.closed.code, 1011);
  await fixture.tools.close();
});

test("a replaced socket owns a fresh immutable catalog and ignores stale callbacks", async () => {
  const first = new FakeSocket();
  const second = new FakeSocket();
  const sockets = [first, second];
  const tools = await createTools({ tools: { echo: { handler: () => "ok" } } });
  const connector = createAttachment(tools, reverseTarget(async () => sockets.shift()), {
    attachmentId: "stable-host",
    reconnectDelayMs: 1,
  });
  const connecting = connector.connect();
  await waitFor(() => first.frames().some(({ type }) => type === "catalog"));
  first.receive({ type: "ready" });
  const client = await connecting;
  first.close(1012, "replace");
  await waitFor(() => second.frames().some(({ type }) => type === "catalog"));
  assert.equal(first.frames()[0].attachment_id, "stable-host");
  assert.deepEqual(second.frames()[0], first.frames()[0]);
  second.receive({ type: "ready" });
  await waitFor(() => client.connected);
  first.receive({ type: "ready", protocol_version: 1 });
  first.emit("error", { error: new Error("stale") });
  await tick();
  assert.equal(second.closed, undefined);
  await drain(client, second);
  await tools.close();
});

test("a policy-close is terminal and never reconnects to replace its successor", async () => {
  const first = new FakeSocket();
  const second = new FakeSocket();
  let connections = 0;
  const tools = await createTools();
  const connector = createAttachment(tools, reverseTarget(async () => {
    connections++;
    return connections === 1 ? first : second;
  }), { reconnectDelayMs: 1 });
  const connecting = connector.connect();
  await waitFor(() => first.frames().some(({ type }) => type === "catalog"));
  first.receive({ type: "ready" });
  const client = await connecting;
  first.close(1008, "Hosted Tools attachment replaced");
  await client.closed();
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(connections, 1);
  assert.deepEqual(second.frames(), []);
  await tools.close();
});

test("attachment target and public one-argument API are enforced", async () => {
  const tools = await createTools();
  for (const target of ["https://managed.test/tools", "wss://user:secret@managed.test/tools", "wss://managed.test/tools#secret", "ws://managed.test/tools"]) {
    assert.throws(() => tools.attach(target), /tool attachment target|plaintext ws/);
  }
  assert.throws(() => tools.attach("wss://managed.test", { reconnect: false }), /only a target/);
  assert.throws(() => tools.attach({ url: "wss://managed.test" }), /unsupported tool attachment target field/);
  await tools.close();
});

test("plaintext attachments allow only loopback and canonical local Nanocodex hosts", async () => {
  const tools = await createTools();
  for (const target of [
    "ws://localhost:5173/tools",
    "ws://127.0.0.1:5173/tools",
    "ws://[::1]:5173/tools",
    "ws://nanocodex.localhost:5173/tools",
    "ws://passkey-fix-a1b2c3.nanocodex.localhost:20735/tools",
  ]) {
    assert.doesNotThrow(() => tools.attach(target));
  }
  for (const target of [
    "ws://nested.instance.nanocodex.localhost:5173/tools",
    "ws://-instance.nanocodex.localhost:5173/tools",
    "ws://instance-.nanocodex.localhost:5173/tools",
    "ws://other.localhost:5173/tools",
    "ws://nanocodex.other.localhost:5173/tools",
    "ws://instance.nanocodex.localhost.example:5173/tools",
  ]) {
    assert.throws(() => tools.attach(target), /plaintext ws/);
  }
  await tools.close();
});

test("attachment waits for providers and dispatches through the socket catalog snapshot", async () => {
  let settle;
  let entries = [];
  const settled = new Promise((resolve) => { settle = resolve; });
  const router = new ToolRouter();
  router.addSource(providerSource("late", {
    id: "late", kind: "cloud",
    definitions: () => entries.map(({ definition }) => definition),
    resolve: (name) => entries.find(({ definition }) => definition.name === name)?.tool,
    settled: () => settled,
  }));
  const owner = { [toolRouterBrand]: true, [toolRouterRuntime]: router };
  const socket = new FakeSocket();
  const connector = createAttachment(owner, reverseTarget(async () => socket), { reconnect: false });
  const connecting = connector.connect();
  await tick();
  assert.deepEqual(socket.frames(), []);
  entries = [{
    definition: { type: "function", name: "late", description: "Late.", strict: false, parameters: { type: "object" } },
    tool: { name: "late", parallelSafe: false, handler: () => "published" },
  }];
  settle();
  await waitFor(() => socket.frames().some(({ type }) => type === "catalog"));
  socket.receive({ type: "ready" });
  const client = await connecting;
  await router.detachSource("late");
  socket.receive({ ...callFrame({}), name: "late" });
  await waitFor(() => socket.frames().some(({ type }) => type === "result"));
  assert.equal(lastFrame(socket, "result").outcome.output.output, "published");
  socket.receive({ type: "ack", call_id: "call:1" });
  await drain(client, socket);
});

test("Node-style text buffers work and binary messages are protocol rejection", async () => {
  const socket = new NodeStyleSocket();
  const tools = await createTools();
  const connector = createAttachment(tools, reverseTarget(async () => socket), { reconnect: false });
  const connecting = connector.connect();
  await waitFor(() => socket.frames().some(({ type }) => type === "catalog"));
  socket.receive({ type: "ready" });
  await connecting;
  socket.receive({ type: "pong", nonce: "binary" }, true);
  await waitFor(() => socket.closed?.code === 1008);
  await tools.close();
});

async function readyAttachment(tool, socket = new FakeSocket()) {
  const tools = await createTools({ tools: { echo: {
    description: "Echo.", parameters: { type: "object", additionalProperties: true }, ...tool,
  } } });
  const connector = createAttachment(tools, reverseTarget(async () => socket), { reconnect: false });
  const connecting = connector.connect();
  await waitFor(() => socket.frames().some(({ type }) => type === "catalog"));
  socket.receive({ type: "ready" });
  return { socket, tools, connector, client: await connecting };
}

function callFrame(input) {
  return {
    type: "call", session_id: "session:1", call_id: "call:1", model: "gpt-5.6-sol",
    name: "echo", input, output_token_budget: 10_000, output_byte_budget: 128 * 1024,
    deadline_at: Date.now() + 30_000,
  };
}

async function drain(client, socket) {
  const closing = client.close();
  await waitFor(() => socket.frames().some(({ type }) => type === "drain"));
  socket.receive({ type: "draining" });
  await closing;
}
function lastFrame(socket, type) { return socket.frames().filter((frame) => frame.type === type).at(-1); }

class FakeSocket {
  readyState = 1;
  sent = [];
  listeners = new Map();
  send(value) {
    const frame = JSON.parse(value);
    if (frame.type === this.throwOnType) throw new Error(`send ${frame.type} failed`);
    this.sent.push(value);
  }
  close(code, reason) { this.closed = { code, reason }; this.readyState = 3; this.emit("close", { code, reason }); }
  addEventListener(type, listener) { const list = this.listeners.get(type) ?? []; list.push(listener); this.listeners.set(type, list); }
  receive(frame) { this.emit("message", { data: JSON.stringify(frame) }); }
  emit(type, event) { for (const listener of this.listeners.get(type) ?? []) listener(event); }
  frames() { return this.sent.map((value) => JSON.parse(value)); }
}

class ThrowingDrainSocket extends FakeSocket {
  send(value) {
    super.send(value);
    if (JSON.parse(value).type === "drain") throw new Error("send drain failed");
  }
}

class SilentCloseSocket extends FakeSocket {
  close(code, reason) { this.closed = { code, reason }; this.readyState = 3; }
}

class NodeStyleSocket {
  readyState = 1;
  sent = [];
  listeners = new Map();
  send(value) { this.sent.push(value); }
  close(code, reason) { this.closed = { code, reason }; this.readyState = 3; this.emit("close", code, reason); }
  on(type, listener) { const list = this.listeners.get(type) ?? []; list.push(listener); this.listeners.set(type, list); }
  receive(frame, binary = false) { this.emit("message", Buffer.from(JSON.stringify(frame)), binary); }
  emit(type, ...args) { for (const listener of this.listeners.get(type) ?? []) listener(...args); }
  frames() { return this.sent.map((value) => JSON.parse(value)); }
}

const tick = () => new Promise((resolve) => setImmediate(resolve));
async function waitFor(predicate) {
  for (let index = 0; index < 200; index++) { if (predicate()) return; await tick(); }
  throw new Error("condition did not become true");
}
async function waitForTimer(predicate) {
  for (let index = 0; index < 100; index++) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  throw new Error("timer condition did not become true");
}
