import assert from "node:assert/strict";
import test from "node:test";

import { providerSource, ToolRouter, toolRouterBrand, toolRouterRuntime } from "../runtime/tool-router.mjs";
import { createTools } from "../tools/Tools.mjs";
import { createAttachment } from "../tools/attachment.mjs";

function reverseTarget(connect, endpoint = "wss://managed.test/tools") {
  return { endpoint, transport: { connect } };
}

test("attachment publishes one exact catalog and exchanges ready, call, result, and ack", async (t) => {
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
    runtime_id: socket.frames()[0].runtime_id,
    capabilities: ["turn_metadata"],
    diagnostics: true,
    command_recovery: true,
    turn_lifecycle: true,
    connection_id: socket.frames()[0].connection_id,
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
      timeout_ms: Number.MAX_SAFE_INTEGER,
    }],
  });
  assert.match(socket.frames()[0].connection_id, UUID_V4);
  socket.receive({ type: "ready" });
  const client = await connecting;
  socket.receive({ ...callFrame({ value: "hello" }), turn_id: "session:1:7" });
  await waitFor(() => socket.frames().some(({ type }) => type === "result"));
  assert.equal(context.model, "gpt-6.1-sol");
  assert.equal(context.turnId, "session:1:7");
  assert.deepEqual(lastFrame(socket, "result"), {
    type: "result",
    call_id: "call:1",
    timing: lastFrame(socket, "result").timing,
    outcome: {
      status: "completed",
      output: {
        output: '{"value":"hello"}', success: true,
        structured_result: { value: "hello" }, metadata: null, process_trace: null,
      },
    },
  });
  const timing = lastFrame(socket, "result").timing;
  const phases = ["scheduler_ms", "execution_gate_ms", "execution_ms", "result_encode_ms", "result_queue_ms"];
  assert.deepEqual(Object.keys(timing).sort(), [...phases, "host_elapsed_ms"].sort());
  assert.ok(Object.values(timing).every(value => Number.isFinite(value) && value >= 0));
  assert.ok(phases.reduce((sum, key) => sum + timing[key], 0) <= timing.host_elapsed_ms + 0.01);
  assert.deepEqual(diagnostics(socket).map(frame => frame.stage), ["received", "execution_started", "execution_finished", "result_prepared"]);
  assert.ok(socket.frames().findIndex(frame => frame.stage === "result_prepared") < socket.frames().findIndex(frame => frame.type === "result"));
  t.diagnostic(JSON.stringify({ catalog: socket.frames()[0], diagnostics: diagnostics(socket), result: lastFrame(socket, "result") }));
  socket.receive({ type: "ack", call_id: "call:1" });
  await drain(client, socket);
  await tools.close();
});

test("attachment preserves arbitrary model strings as opaque metadata", async () => {
  const models = ["@cf/zai-org/glm-5.3", "kimi-k3", "mimo-v2.6-pro", "auto", "openrouter/auto", "openai/gpt-6-astra", "@openai/gpt-6-astra", "auto(openai/gpt-6-astra,anthropic/claude)", "", "x y", "x\n", "x\t", "x\0", "x\x7f", "模型🦙", "x".repeat(8192)];
  const received = [];
  const tools = await createTools({ tools: {
    echo: { description: "Echo.", handler: (_, context) => { received.push(context.model); return "ok"; } },
  } });
  const socket = new FakeSocket();
  const connector = createAttachment(tools, reverseTarget(async () => socket), { reconnect: false });
  const connecting = connector.connect();
  await waitFor(() => socket.frames().length === 1);
  socket.receive({ type: "ready" });
  const client = await connecting;
  for (const [index, model] of models.entries()) {
    const call_id = `call:${index}`;
    socket.receive({ ...callFrame({}), call_id, model });
    await waitFor(() => socket.frames().some((frame) => frame.type === "result" && frame.call_id === call_id));
    assert.equal(lastFrame(socket, "result").outcome.status, "completed");
    socket.receive({ type: "ack", call_id });
  }
  assert.deepEqual(received, models);
  await drain(client, socket);
  await tools.close();
});

test("attachment rejects non-string model metadata before dispatch", async () => {
  for (const model of [undefined, null, 1, {}, [], true]) {
    let dispatched = false;
    const fixture = await readyAttachment({ handler: () => { dispatched = true; return "ok"; } });
    fixture.socket.receive({ ...callFrame({}), model });
    await waitFor(() => fixture.socket.closed?.code === 1008);
    assert.match(fixture.socket.closed.reason, /model must/);
    assert.equal(dispatched, false);
    await fixture.tools.close();
  }
});

test("attachment transports large admitted inputs and image results without a local frame cutoff", async () => {
  const data = "A".repeat(3 * 1024 * 1024);
  let calls = 0;
  const fixture = await readyAttachment({ handler: ({ value }) => {
    calls += 1;
    assert.equal(value.length, data.length);
    assert.equal(value, data);
    return { type: "image", data: value };
  } });
  fixture.socket.receive({ ...callFrame({ value: data }), output_byte_budget: 8 * 1024 * 1024 });
  await waitFor(() => fixture.socket.frames().some(({ type }) => type === "result"));
  const result = lastFrame(fixture.socket, "result");
  assert.equal(result.outcome.status, "completed");
  assert.equal(result.outcome.output.structured_result.data.length, data.length);
  assert.equal(result.outcome.output.structured_result.data, data);
  assert.equal(calls, 1);
  fixture.socket.receive({ type: "ack", call_id: "call:1" });
  await drain(fixture.client, fixture.socket);
  await fixture.tools.close();
});

test("attachment does not confuse transport buffering or diagnostic failure with a failed tool call", async (t) => {
  const socket = new FakeSocket();
  socket.bufferedAmount = 3 * 1024 * 1024;
  socket.throwOnType = "diagnostic";
  t.mock.method(console, "info", () => { throw new Error("observer failed"); });
  let calls = 0;
  const fixture = await readyAttachment({ handler: () => { calls += 1; return "queued by transport"; } }, socket);
  socket.receive(callFrame({}));
  await waitFor(() => socket.frames().some(({ type }) => type === "result"));
  assert.equal(lastFrame(socket, "result").outcome.status, "completed");
  assert.equal(calls, 1);
  socket.receive({ type: "ack", call_id: "call:1" });
  await drain(fixture.client, socket);
  await fixture.tools.close();
});

test("independent Tools runtimes publish distinct attachment identifiers", async () => {
  const firstTools = await createTools({ attachmentId: "machine:first" });
  const secondTools = await createTools({ attachmentId: "machine:second" });
  const firstSocket = new FakeSocket();
  const secondSocket = new FakeSocket();
  const firstConnecting = firstTools.attach(reverseTarget(async () => firstSocket)).connect();
  const secondConnecting = secondTools.attach(reverseTarget(async () => secondSocket)).connect();
  await waitFor(() => firstSocket.frames().length === 1 && secondSocket.frames().length === 1);
  assert.match(firstSocket.frames()[0].runtime_id, /^[a-f0-9-]{36}$/);
  assert.notEqual(firstSocket.frames()[0].runtime_id, secondSocket.frames()[0].runtime_id);
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
    timing: lastFrame(fixture.socket, "result").timing,
    outcome: { status: "ambiguous", message: "tool execution was cancelled after dispatch" },
  });
  assert.equal(fixture.socket.frames().some(({ type }) => type === "cancel_ack"), false);
  fixture.socket.receive({ type: "ack", call_id: "call:1" });
  await drain(fixture.client, fixture.socket);
  await fixture.tools.close();
});

test("cancellation for an unknown call is ignored", async () => {
  const fixture = await readyAttachment({ handler: () => "done" });
  fixture.socket.receive({ type: "cancel", call_id: "unknown" });
  await tick();
  assert.equal(fixture.socket.frames().some(({ type }) => type === "result"), false);
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

  const failed = await readyAttachment({ handler: () => "ok" }, new SilentCloseSocket());
  failed.socket.emit("error", { error: new Error("private transport failure") });
  await failed.client.closed();
  assert.equal(failed.client.connected, false);
  assert.equal(failed.socket.closed.code, 1011);
  await failed.tools.close();
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

test("duplicate immutable calls reuse running work and terminal receipts; conflicts fence", async () => {
  let finish, calls = 0;
  const fixture = await readyAttachment({ handler: () => { calls++; return new Promise(resolve => { finish = resolve; }); } });
  const frame = callFrame({id: 1});
  fixture.socket.receive(frame);
  fixture.socket.receive(frame);
  await waitFor(() => lastFrame(fixture.socket, "status"));
  assert.deepEqual(lastFrame(fixture.socket, "status"), {type:"status", call_id:frame.call_id, state:"running"});
  assert.equal(calls, 1);
  finish("once");
  await waitFor(() => lastFrame(fixture.socket, "result"));
  fixture.socket.receive(frame);
  await waitFor(() => fixture.socket.frames().filter(row => row.type === "result").length === 2);
  assert.equal(calls, 1);
  fixture.socket.receive({...frame, input:{id:2}});
  await waitFor(() => fixture.socket.closed?.code === 1008);
  assert.match(fixture.socket.closed.reason, /identity conflicts/);
  await fixture.tools.close();
});

test("admitted deadlines, invalid, and oversized post-dispatch outcomes preserve semantics", async (t) => {
  let calls = 0;
  const expired = await readyAttachment({ handler: () => { calls++; return "unexpected"; } });
  expired.socket.receive({ ...callFrame({}), deadline_at: Date.now() - 1 });
  await waitFor(() => expired.socket.frames().some(({ type }) => type === "result"));
  assert.equal(calls, 0);
  assert.deepEqual(diagnostics(expired.socket).map(frame => frame.stage), ["received", "result_prepared"]);
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

  let signal;
  const long = await readyAttachment({ handler: (_input, context) => {
    signal = context.signal;
    return new Promise((_resolve, reject) => signal.addEventListener("abort", () => reject(signal.reason), { once: true }));
  } });
  t.after(async () => { long.socket.close(1000, "test finished"); await long.tools.close(); });
  t.mock.timers.enable({ apis: ["Date", "setTimeout"], now: 1_000_000 });
  long.socket.receive({ ...callFrame({}), deadline_at: Date.now() + 600_000 });
  await waitFor(() => signal);
  t.mock.timers.tick(300_000);
  await tick();
  assert.equal(signal.aborted, false);
  assert.equal(lastFrame(long.socket, "result"), undefined);
  t.mock.timers.tick(300_000);
  await waitFor(() => lastFrame(long.socket, "result"));
  assert.equal(signal.aborted, true);
  assert.equal(lastFrame(long.socket, "result").outcome.status, "ambiguous");
  long.socket.receive({ type: "ack", call_id: "call:1" });
  await drain(long.client, long.socket);
  await long.tools.close();
});

// Actual control-frame ping/pong, timeout, and browser-default behavior are
// covered by hand-reconnect-journey with a real WebSocket server and executor.

test("old protocol fields reject and duplicate acknowledgements are harmless", async () => {
  const old = await readyAttachment({ handler: () => "ok" });
  old.socket.receive({ type: "ready", protocol_version: 1 });
  await waitFor(() => old.socket.closed?.code === 1008);
  assert.match(old.socket.closed.reason, /unsupported field protocol_version/);
  await old.tools.close();

  const unknown = await readyAttachment({ handler: () => "ok" });
  unknown.socket.receive({ type: "ack", call_id: "unknown" });
  await tick();
  assert.equal(unknown.socket.closed, undefined);
  await drain(unknown.client, unknown.socket);
  await unknown.tools.close();
});

test("ready before catalog publication is rejected", async (t) => {
  const observations = [];
  t.mock.method(console, "info", event => observations.push(event));
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

  const timedOut = new FakeSocket();
  timedOut.readyState = 0;
  const empty = await createTools();
  const timeoutConnector = createAttachment(empty, reverseTarget(async () => timedOut), { handshakeTimeoutMs: 5 });
  await assert.rejects(timeoutConnector.connect(), /handshake timed out/);
  await timeoutConnector.closed();
  assert.equal(timedOut.closed.code, 1012);
  assert.ok(observations.some(event => event.event === "handshake_timeout"));
  assert.equal(observations.some(event => event.event.startsWith("reconnect_")), false);
  await empty.close();
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

test("a replacement socket preserves running work and replays its retained result", async (t) => {
  const observations = [];
  t.mock.method(console, "info", event => observations.push(event));
  const first = new FakeSocket();
  const preReady = new SilentCloseSocket();
  const second = new FakeSocket();
  const sockets = [first, new Error("private failed reconnect URL"), preReady, second];
  let finish, signal;
  let calls = 0;
  const tools = await createTools({ tools: { echo: { handler: (_input, context) => {
    calls++; signal = context.signal;
    return new Promise(resolve => { finish = resolve; });
  } } } });
  const connector = createAttachment(tools, reverseTarget(async () => {
    const socket = sockets.shift();
    if (socket instanceof Error) throw socket;
    return socket;
  }), {
    attachmentId: "stable-host",
    reconnectDelayMs: 1,
  });
  const connecting = connector.connect();
  await waitFor(() => first.frames().some(({ type }) => type === "catalog"));
  first.receive({ type: "ready" });
  const client = await connecting;
  first.receive(callFrame({}));
  await waitFor(() => finish);
  first.emit("error", { error: new Error("private transport failure") });
  assert.equal(first.closed.code, 1011);
  assert.equal(signal.aborted, false);
  await waitForTimer(() => preReady.frames().some(({ type }) => type === "catalog"));
  preReady.emit("error", { error: new Error("private pre-ready error") });
  await waitForTimer(() => second.frames().some(({ type }) => type === "catalog"));
  assert.equal(first.frames()[0].attachment_id, "stable-host");
  assert.notEqual(second.frames()[0].connection_id, first.frames()[0].connection_id);
  assert.match(second.frames()[0].connection_id, UUID_V4);
  assert.deepEqual({ ...second.frames()[0], connection_id: first.frames()[0].connection_id }, first.frames()[0]);
  second.receive({ type: "ready" });
  await waitFor(() => client.connected);
  first.receive({ type: "ready", protocol_version: 1 });
  first.emit("error", { error: new Error("stale") });
  await tick();
  assert.equal(second.closed, undefined);
  finish("old result");
  await tick();
  assert.equal(calls, 1);
  assert.equal(first.frames().some(({ type }) => type === "result"), false);
  assert.equal(lastFrame(second, "result").outcome.output.output, "old result");
  assert.equal(observations.filter(event => event.event === "result_retained").length, 1);
  assert.equal(observations.some(event => event.event === "result_discarded"), false);
  second.receive({type:"recover", call_ids:["call:1", "missing:1"]});
  await tick();
  assert.deepEqual(lastFrame(second, "status"), {type:"status", call_id:"missing:1", state:"missing"});
  assert.equal(second.frames().filter(row => row.type === "result").length, 2);
  second.receive({type:"ack", call_id:"call:1"});
  await drain(client, second);
  assert.deepEqual(observations.filter(event => event.event === "connection_start").map(event => event.attempt), [1, 2, 3, 4]);
  assert.deepEqual(observations.filter(event => event.event === "reconnect_started").map(event => event.attempt), [2, 3, 4]);
  assert.equal(observations.filter(event => event.event === "reconnect_scheduled").length, 3);
  assert.equal(observations.filter(event => event.event === "error").length, 3);
  assert.ok(observations.every(event => event.type === "hand.attachment" && UUID_V4.test(event.client_connection_id)));
  assert.equal(new Set(observations.filter(event => event.event === "connection_start").map(event => event.client_connection_id)).size, 4);
  assert.ok(observations.every(event => Number.isSafeInteger(event.active_calls) && Number.isSafeInteger(event.retained_calls)));
  assert.ok(observations.filter(event => event.event === "reconnect_scheduled").every(event => event.delay_ms >= 0 && Number.isFinite(event.delay_ms)));
  assert.doesNotMatch(JSON.stringify(observations), /private|managed\.test|old result/);
  t.diagnostic(JSON.stringify({ recovery: observations }));
  await tools.close();
});

test("a policy-close is terminal and never reconnects to replace its successor", async (t) => {
  const observations = [];
  t.mock.method(console, "info", event => observations.push(event));
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
  assert.equal(observations.some(event => event.event.startsWith("reconnect_")), false);
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
    type: "call", session_id: "session:1", call_id: "call:1", model: "gpt-6.1-sol",
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
const UUID_V4 = /^[a-f0-9]{8}-[a-f0-9]{4}-4[a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/;
function diagnostics(socket) {
  return socket.frames().filter(frame => frame.type === "diagnostic").map(frame => {
    assert.deepEqual(Object.keys(frame).sort(), ["type", "call_id", "stage", "elapsed_ms"].sort());
    assert.ok(Number.isFinite(frame.elapsed_ms) && frame.elapsed_ms >= 0 && frame.elapsed_ms <= Number.MAX_SAFE_INTEGER);
    return frame;
  });
}

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


test("attachment dispatches nonparallel calls beyond the former call and receipt caps", async () => {
  let release;
  const seen = [];
  const fixture = await readyAttachment({ handler: async ({ index }) => {
    seen.push(index);
    if (index === 0) await new Promise(resolve => { release = resolve; });
    return index;
  } });
  for (let index = 0; index < 160; index++) {
    fixture.socket.receive({ ...callFrame({ index }), call_id: `concurrent:${index}` });
  }
  await waitFor(() => fixture.socket.frames().filter(frame => frame.type === "result").length === 159);
  assert.equal(seen.length, 160);
  assert.equal(fixture.socket.closed, undefined);
  assert.equal(fixture.socket.frames().some(frame => frame.type === "result" && frame.call_id === "concurrent:0"), false);
  release();
  await waitFor(() => fixture.socket.frames().filter(frame => frame.type === "result").length === 160);
  assert.equal(fixture.socket.closed, undefined);
  assert.deepEqual(diagnostics(fixture.socket).filter(frame => frame.call_id === "concurrent:159").map(frame => frame.stage), ["received", "execution_started", "execution_finished", "result_prepared"]);
  for (const frame of fixture.socket.frames().filter(frame => frame.type === "result")) {
    assert.equal(frame.outcome.status, "completed");
    fixture.socket.receive({ type: "ack", call_id: frame.call_id });
  }
  await drain(fixture.client, fixture.socket);
  await fixture.tools.close();
});


test("trusted turn completion reaches tool lifecycle outside its public catalog", async () => {
  const ended = [];
  const { socket, tools, client } = await readyAttachment({ handler: () => "ok",
    endTurn: async (...args) => { ended.push(args); } });
  assert.equal(socket.frames()[0].turn_lifecycle, true);
  assert.deepEqual(socket.frames()[0].tools.map(tool => tool.definition.name), ["echo"]);
  for (const hook_event_name of ["Stop", "Interrupt", "SubagentStop"]) {
    socket.receive({ type: "turn_ended", session_id: "session:1", turn_id: "session:1:7", hook_event_name });
    await waitFor(() => ended.length === ["Stop", "Interrupt", "SubagentStop"].indexOf(hook_event_name) + 1);
  }
  assert.deepEqual(ended, ["Stop", "Interrupt", "SubagentStop"].map(event => ["session:1", "session:1:7", event]));
  await drain(client, socket); await tools.close();
});
