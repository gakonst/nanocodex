import assert from "node:assert/strict";
import { test } from "node:test";

import { createBrowserHost as createProductionBrowserHost } from "../browser/host.mjs";

const createBrowserHost = (options = {}) => createProductionBrowserHost({
  codeEvaluator: evaluateInTestRealm,
  ...options,
});

async function evaluateInTestRealm(source, environment) {
  const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
  const script = new AsyncFunction(
    "tools", "ALL_TOOLS", "text", "image", "generatedImage", "store", "load", "exit",
    "require", "console", source,
  );
  await script(
    environment.tools,
    environment.toolDefinitions,
    environment.text,
    environment.image,
    environment.generatedImage,
    environment.store,
    environment.load,
    environment.exit,
    environment.require,
    environment.console,
  );
}

test("browser Code Mode fails closed when an evaluator Worker is unavailable", async () => {
  assert.equal(typeof globalThis.Worker, "undefined");
  const host = createProductionBrowserHost({ WebSocketImpl: FakeWebSocket });
  const execution = JSON.parse(await host.executeCode("while (true) {}"));
  assert.equal(execution.success, false);
  assert.match(execution.output, /requires a child Worker or an explicit codeEvaluator/);
});

test("browser host carries ordered frames and application tools", async () => {
  const events = [];
  const host = createBrowserHost({
    WebSocketImpl: FakeWebSocket,
    onEvent: (event) => events.push(event),
    tools: {
      double: {
        description: "Double a number.",
        parameters: { type: "object" },
        handler: ({ value }) => value * 2,
      },
      numericText: {
        parameters: { type: "object" },
        handler: () => "42",
      },
    },
  });
  const connecting = host.connect("ws://example.test", "not-forwarded", "session");
  const socket = FakeWebSocket.instances.at(-1);
  socket.open();
  assert.equal(JSON.parse(await connecting).status, 101);
  socket.message('{"type":"one"}');
  socket.message('{"type":"two"}');
  assert.equal(JSON.parse(await host.next(1, 10)).text, '{"type":"one"}');
  assert.equal(JSON.parse(await host.next(1, 10)).text, '{"type":"two"}');

  const execution = JSON.parse(await host.executeCode(
    "text(await tools.double({ value: 21 })); text(await tools.numericText({}));",
    "session",
    "call-exec",
  ));
  assert.equal(execution.success, true);
  assert.match(JSON.stringify(execution.output), /42/);
  assert.equal(execution.nested_calls[0].name, "double");
  assert.equal(execution.nested_calls[0].call_id, "call-exec/code-1");
  assert.equal(execution.nested_calls[0].structured_result, 42);
  assert.equal(execution.nested_calls[1].structured_result, "42");
  assert.equal(Number.isSafeInteger(execution.nested_calls[0].started_after_ns), true);
  assert.ok(execution.nested_calls[0].started_after_ns >= 0);
  assert.equal(JSON.parse(host.toolDefinitions())[0].name, "double");
  host.emitEvent("event");
  assert.deepEqual(events, ["event"]);
});

test("browser host directly dispatches tools without dynamic code evaluation", async () => {
  const host = createBrowserHost({
    toolMode: "direct",
    tools: {
      runtimeInfo: {
        parameters: { type: "object", additionalProperties: false },
        handler: (_input, context) => ({ runtime: "worker", call_id: context.callId }),
      },
    },
  });
  assert.equal(host.toolMode(), "direct");
  const result = JSON.parse(await host.executeTool(
    "runtimeInfo",
    "{}",
    "session-1",
    "call-1",
  ));
  assert.equal(result.success, true);
  assert.deepEqual(JSON.parse(result.output), { runtime: "worker", call_id: "call-1" });
  assert.deepEqual(result.structured_result, { runtime: "worker", call_id: "call-1" });
});

test("browser host gives inherited tools the Rust-owned subagent descriptor", async () => {
  const host = createBrowserHost({
    toolMode: "direct",
    tools: {
      identity: {
        parameters: { type: "object", additionalProperties: false },
        handler: (_input, context) => context.subagent ?? null,
      },
    },
  });
  const descriptor = {
    agentId: "7",
    parentAgentId: "2",
    sessionId: "child-session",
    role: "world-resident:fern",
    task: "Act as Fern.",
  };
  host.bindSubagentSession("child-session", descriptor);

  const child = JSON.parse(await host.executeTool(
    "identity", "{}", "child-session", "call-child",
  ));
  const root = JSON.parse(await host.executeTool(
    "identity", "{}", "root-session", "call-root",
  ));
  const nested = JSON.parse(await host.executeCode(
    "text(await tools.identity({}));",
    "child-session",
    "call-code",
  ));

  assert.deepEqual(child.structured_result, descriptor);
  assert.equal(root.structured_result, null);
  assert.deepEqual(nested.nested_calls[0].structured_result, descriptor);

  host.releaseSession("child-session");
  const released = JSON.parse(await host.executeTool(
    "identity", "{}", "child-session", "call-released",
  ));
  assert.equal(released.structured_result, null);
});

test("browser host never flattens remote MCP tools into direct mode", () => {
  assert.throws(
    () => createBrowserHost({ mcp: { fixture: { client: {} } }, toolMode: "direct" }),
    /remote MCP requires Code Mode/,
  );
});

test("browser host readiness installs deferred MCP without waiting for discovery", async () => {
  let releaseDiscovery;
  let reportDiscoveryStarted;
  const discovery = new Promise((resolve) => { releaseDiscovery = resolve; });
  const discoveryStarted = new Promise((resolve) => { reportDiscoveryStarted = resolve; });
  const host = createBrowserHost({
    mcp: {
      fixture: {
        client: {
          close: async () => {},
          listTools: () => {
            reportDiscoveryStarted();
            return discovery;
          },
        },
      },
    },
  });
  let ready = false;
  const readiness = host.ready().then(() => { ready = true; });
  const tools = { tools: [{ name: "lookup", inputSchema: { type: "object" } }] };
  try {
    await Promise.race([discoveryStarted, readiness]);
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(ready, true);
    assert.match(host.toolDefinitions(), /tool_search/);
    assert.doesNotMatch(host.toolDefinitions(), /mcp__fixture__lookup/);

    releaseDiscovery(tools);
    await new Promise((resolve) => setImmediate(resolve));
    assert.match(host.toolDefinitions(), /mcp__fixture__lookup/);
  } finally {
    releaseDiscovery(tools);
    await readiness.catch(() => {});
    await host.dispose();
  }
});

test("browser host reports non-JSON tool results as failures", async () => {
  const host = createBrowserHost({
    tools: {
      bigint: {
        parameters: { type: "object" },
        handler: () => 1n,
      },
    },
  });
  const execution = JSON.parse(await host.executeCode(
    "try { await tools.bigint({}); } catch (error) { text(error.message); }",
    "session",
    "call-exec",
  ));

  assert.equal(execution.success, true);
  assert.equal(execution.nested_calls[0].success, false);
  assert.match(execution.nested_calls[0].structured_result, /JSON-serializable/);

  const direct = JSON.parse(await host.executeTool("bigint", "{}"));
  assert.equal(direct.success, false);
  assert.match(direct.output, /JSON-serializable/);
});

test("browser host cancellation is scoped to one session", async () => {
  const started = new Map();
  const host = createBrowserHost({
    tools: {
      blocked: {
        parameters: { type: "object" },
        handler(_input, context) {
          started.get(context.sessionId)?.();
          return new Promise((_resolve, reject) => {
            context.signal.addEventListener(
              "abort",
              () => reject(new Error(`${context.sessionId} cancelled`)),
              { once: true },
            );
          });
        },
      },
    },
  });
  const startA = new Promise((resolve) => started.set("session-a", resolve));
  const startB = new Promise((resolve) => started.set("session-b", resolve));
  const callA = host.executeTool("blocked", "{}", "session-a", "call-a");
  const callB = host.executeTool("blocked", "{}", "session-b", "call-b");
  await Promise.all([startA, startB]);
  host.cancelCode("session-a");
  const resultA = JSON.parse(await callA);
  assert.equal(resultA.success, false);
  assert.match(resultA.output, /session-a cancelled/);
  const stillPending = await Promise.race([
    callB.then(() => false),
    new Promise((resolve) => setTimeout(() => resolve(true), 10)),
  ]);
  assert.equal(stillPending, true);
  host.cancelCode("session-b");
  assert.match(JSON.parse(await callB).output, /session-b cancelled/);
});

test("browser host opens application sockets through MPP", async () => {
  const socket = new FakeWebSocket("wss://paid.test");
  socket.readyState = FakeWebSocket.OPEN;
  const endpoints = [];
  const host = createBrowserHost({
    mpp: {
      async ws(endpoint) {
        endpoints.push(endpoint);
        return socket;
      },
    },
  });

  assert.equal(JSON.parse(await host.connect("wss://paid.test", "mpp-managed", "session")).status, 101);
  assert.deepEqual(endpoints, ["wss://paid.test"]);
  socket.message('{"type":"paid"}');
  assert.equal(JSON.parse(await host.next(1, 10)).text, '{"type":"paid"}');
  assert.equal(JSON.parse(await host.send(1, "request")).ok, true);
  assert.deepEqual(socket.sent.map(JSON.parse), [{ mpp: "message", data: "request" }]);
  socket.close(3008, "requested voucher amount exceeds local maxDeposit");
  assert.deepEqual(JSON.parse(await host.next(1, 10)), {
    kind: "error",
    detail: "MPP WebSocket payment flow failed with code 3008: requested voucher amount exceeds local maxDeposit",
    reconnectable: false,
  });
});

test("browser host does not require a global constructor for host-owned sockets", async () => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "WebSocket");
  try {
    Object.defineProperty(globalThis, "WebSocket", {
      configurable: true,
      value: undefined,
      writable: true,
    });

    const paidSocket = new FakeWebSocket("wss://paid.test");
    paidSocket.readyState = FakeWebSocket.OPEN;
    const paid = createBrowserHost({
      mpp: {
        async ws() {
          return paidSocket;
        },
      },
    });
    await paid.connect("wss://paid.test", "mpp-managed", "paid-session");
    assert.equal(JSON.parse(await paid.send(1, "request")).ok, true);

    const directSocket = new FakeWebSocket("wss://direct.test");
    const direct = createBrowserHost({
      createWebSocket() {
        return directSocket;
      },
    });
    const connecting = direct.connect(
      "wss://direct.test",
      "host-managed",
      "direct-session",
    );
    directSocket.open();
    await connecting;
    assert.equal(JSON.parse(await direct.send(1, "request")).ok, true);
  } finally {
    if (descriptor) Object.defineProperty(globalThis, "WebSocket", descriptor);
    else delete globalThis.WebSocket;
  }
});

test("browser host awaits Worker upgrades and preserves handshake metadata", async () => {
  const socket = new FakeWebSocket("wss://api.openai.test/v1/responses");
  socket.readyState = FakeWebSocket.OPEN;
  let request;
  const host = createBrowserHost({
    async createWebSocket(endpoint, sessionId, received) {
      await Promise.resolve();
      request = { endpoint, sessionId, received };
      return {
        socket,
        status: 101,
        requestId: "request-1",
        serverModel: "gpt-test",
        reasoningIncluded: true,
        turnState: "turn-state-1",
      };
    },
  });

  const connected = JSON.parse(await host.connect(
    "wss://api.openai.test/v1/responses",
    "secret-token",
    "session-1",
    { accountId: "account-1", fedramp: true, turnState: "turn-state-0" },
  ));

  assert.deepEqual(request, {
    endpoint: "wss://api.openai.test/v1/responses",
    sessionId: "session-1",
    received: {
      accountId: "account-1",
      authorization: "bearer",
      bearerToken: "secret-token",
      fedramp: true,
      turnState: "turn-state-0",
    },
  });
  assert.deepEqual(connected, {
    handle: 1,
    status: 101,
    request_id: "request-1",
    server_model: "gpt-test",
    reasoning_included: true,
    turn_state: "turn-state-1",
  });
});

test("browser host preconnects once and gives the exact socket to the first model call", async () => {
  const socket = new FakeWebSocket("wss://api.openai.test/v1/responses");
  socket.readyState = FakeWebSocket.OPEN;
  const requests = [];
  const host = createBrowserHost({
    createWebSocket(endpoint, sessionId, request) {
      requests.push({ endpoint, sessionId, request });
      return socket;
    },
  });

  await host.preconnect("wss://api.openai.test/v1/responses", "session-1");
  const connected = JSON.parse(await host.connect(
    "wss://api.openai.test/v1/responses",
    "host-managed",
    "session-1",
  ));

  assert.equal(connected.status, 101);
  assert.deepEqual(requests, [{
    endpoint: "wss://api.openai.test/v1/responses",
    sessionId: "session-1",
    request: { authorization: "preconnect" },
  }]);
});

test("disposing a host owns a taken preconnect through handshake completion", async () => {
  const handshake = deferred();
  const socket = new FakeWebSocket("wss://api.openai.test/v1/responses");
  socket.readyState = FakeWebSocket.OPEN;
  const host = createBrowserHost({ createWebSocket: () => handshake.promise });

  void host.preconnect(socket.url, "session-1").catch(() => {});
  const connecting = host.connect(socket.url, "host-managed", "session-1");
  await host.dispose();
  handshake.resolve(socket);

  await assert.rejects(connecting, /disposed during WebSocket connection/);
  assert.equal(socket.readyState, 3);
  await assert.rejects(
    host.connect(socket.url, "host-managed", "session-1"),
    /already disposed/,
  );
});

test("disposing a host rejects never-resolving socket factories and preconnects", async () => {
  const directHandshake = deferred();
  const direct = createBrowserHost({ createWebSocket: () => directHandshake.promise });
  const directConnect = direct.connect("wss://direct.test", "secret", "session-1");
  await direct.dispose();
  await assert.rejects(directConnect, /disposed during WebSocket connection/);

  const preconnectHandshake = deferred();
  const preconnected = createBrowserHost({
    createWebSocket: () => preconnectHandshake.promise,
  });
  const preconnect = preconnected.preconnect("wss://preconnect.test", "session-2");
  await preconnected.dispose();
  await assert.rejects(preconnect, /disposed during WebSocket connection/);
});

test("disposing a host rejects a never-resolving MPP socket factory", async () => {
  const handshake = deferred();
  const host = createBrowserHost({ mpp: { ws: () => handshake.promise } });
  const connecting = host.connect("wss://paid.test", "mpp-managed", "session-1");

  await host.dispose();
  await assert.rejects(connecting, /disposed during WebSocket connection/);
});

test("disposing a host closes a CONNECTING socket before a late open", async () => {
  const socket = new FakeWebSocket("wss://api.openai.test/v1/responses");
  const host = createBrowserHost({ createWebSocket: () => socket });

  const connecting = host.connect(socket.url, "host-managed", "session-1");
  await Promise.resolve();
  const rejected = assert.rejects(connecting, /disposed during WebSocket connection/);
  await host.dispose();

  await rejected;
  assert.equal(socket.readyState, 3);
  socket.open();
  assert.deepEqual(JSON.parse(await host.send(1, "must-not-send")), {
    ok: false,
    reconnectable: true,
    error: "WebSocket is no longer open",
  });
  assert.deepEqual(socket.sent, []);
});

test("host disposal isolates all close failures and completes every cleanup", async () => {
  const failures = [
    new Error("connecting close failed"),
    new Error("first established close failed"),
    new Error("second established close failed"),
    new Error("preconnect close failed"),
    new Error("code cleanup failed"),
    new Error("onDispose failed"),
  ];
  const sockets = new Map([
    ["wss://connecting.test", failingSocket("wss://connecting.test", failures[0])],
    ["wss://first.test", failingSocket("wss://first.test", failures[1])],
    ["wss://second.test", failingSocket("wss://second.test", failures[2], true)],
    ["wss://preconnect.test", failingSocket("wss://preconnect.test", failures[3], true)],
  ]);
  sockets.get("wss://connecting.test").readyState = 0;
  let mcpAborted = false;
  const host = createBrowserHost({
    createWebSocket: (endpoint) => sockets.get(endpoint),
    mcp: {
      cleanup: {
        client: {
          listTools(_params, { signal }) {
            signal.addEventListener("abort", () => { mcpAborted = true; }, { once: true });
            return new Promise(() => {});
          },
        },
      },
    },
    onDispose: async () => { throw failures[5]; },
    tools: {
      cleanup: {
        handler() {},
        dispose() { throw failures[4]; },
      },
    },
  });

  await host.ready();
  await host.connect("wss://first.test", "secret", "session-1");
  await host.connect("wss://second.test", "secret", "session-2");
  const connecting = host.connect("wss://connecting.test", "secret", "session-3");
  void connecting.catch(() => {});
  await Promise.resolve();
  await host.preconnect("wss://preconnect.test", "session-4");

  const disposal = host.dispose();
  assert.strictEqual(host.dispose(), disposal);
  await assert.rejects(disposal, (error) => {
    assert.equal(error instanceof AggregateError, true);
    assert.deepEqual(error.errors, failures);
    return true;
  });
  await assert.rejects(connecting, /disposed during WebSocket connection/);
  assert.equal(mcpAborted, true);
  assert.strictEqual(host.dispose(), disposal);
});

test("host disposal owns post-open sockets until direct and MPP registration", async () => {
  const unhandled = [];
  const onUnhandled = (error) => { unhandled.push(error); };
  process.on("unhandledRejection", onUnhandled);
  try {
    for (const [kind, asynchronous] of [["direct", false], ["mpp", true]]) {
      const closeError = new Error(`${kind} close failed`);
      const socket = new FakeWebSocket(`wss://${kind}.test`);
      socket.readyState = FakeWebSocket.OPEN;
      let closeCalls = 0;
      socket.close = () => {
        closeCalls += 1;
        socket.readyState = 3;
        if (asynchronous) return Promise.reject(closeError);
        throw closeError;
      };

      let host;
      let disposal;
      const triggerDisposal = () => {
        if (disposal) return;
        disposal = host.dispose();
        void disposal.catch(() => {});
      };
      let opened;
      if (kind === "direct") {
        opened = {
          get socket() {
            triggerDisposal();
            return socket;
          },
        };
        host = createBrowserHost({ createWebSocket: () => opened });
      } else {
        Object.defineProperty(socket, "addEventListener", {
          configurable: true,
          get() {
            triggerDisposal();
            return FakeWebSocket.prototype.addEventListener.bind(socket);
          },
        });
        host = createBrowserHost({ mpp: { ws: () => socket } });
      }

      const connecting = host.connect(socket.url, "secret", `session-${kind}`);
      await assert.rejects(connecting, /disposed during WebSocket connection/);
      assert.ok(disposal);
      assert.strictEqual(host.dispose(), disposal);
      await assert.rejects(disposal, (error) => error === closeError);
      assert.equal(closeCalls, 1);
      await new Promise((resolve) => setImmediate(resolve));
    }
    assert.deepEqual(unhandled, []);
  } finally {
    process.off("unhandledRejection", onUnhandled);
  }
});

test("reentrant host disposal returns one promise and performs each cleanup once", async () => {
  const socket = new FakeWebSocket("wss://reentrant.test");
  socket.readyState = FakeWebSocket.OPEN;
  const calls = { close: 0, tool: 0, onDispose: 0 };
  const reentrant = [];
  let host;
  socket.close = () => {
    calls.close += 1;
    reentrant.push(host.dispose());
    socket.readyState = 3;
  };
  host = createBrowserHost({
    createWebSocket: () => socket,
    onDispose() {
      calls.onDispose += 1;
      reentrant.push(host.dispose());
    },
    tools: {
      cleanup: {
        handler() {},
        dispose() {
          calls.tool += 1;
          reentrant.push(host.dispose());
        },
      },
    },
  });
  await host.connect(socket.url, "secret", "session-reentrant");

  const disposal = host.dispose();
  assert.strictEqual(host.dispose(), disposal);
  await disposal;

  assert.deepEqual(calls, { close: 1, tool: 1, onDispose: 1 });
  assert.equal(reentrant.length, 3);
  assert.equal(reentrant.every((nested) => nested === disposal), true);
  assert.strictEqual(host.dispose(), disposal);
});

test("browser host never exposes its host-managed credential marker", async () => {
  const socket = new FakeWebSocket("wss://chatgpt.test/backend-api/codex/responses");
  socket.readyState = FakeWebSocket.OPEN;
  let request;
  const host = createBrowserHost({
    hostAuth: true,
    createWebSocket(_endpoint, _sessionId, received) {
      request = received;
      return socket;
    },
  });

  await host.connect(socket.url, "host-managed", "session-1", {
    authorization: "bearer",
    bearerToken: "metadata-must-not-override-auth",
  });
  assert.deepEqual(request, { authorization: "host_managed" });
});

test("an API key equal to the old host marker remains a bearer credential", async () => {
  const socket = new FakeWebSocket("wss://api.openai.test/v1/responses");
  socket.readyState = FakeWebSocket.OPEN;
  let request;
  const host = createBrowserHost({
    createWebSocket(_endpoint, _sessionId, received) {
      request = received;
      return socket;
    },
  });

  await host.connect(socket.url, "host-managed", "session-1", {
    authorization: "host_managed",
  });
  assert.deepEqual(request, {
    authorization: "bearer",
    bearerToken: "host-managed",
  });
});

test("browser host rejects failed upgrades without consuming handles", async () => {
  const closed = new FakeWebSocket("wss://closed.test");
  closed.readyState = 3;
  const opened = new FakeWebSocket("wss://opened.test");
  opened.readyState = FakeWebSocket.OPEN;
  const results = [
    Promise.reject(new Error("upgrade denied")),
    {},
    closed,
    opened,
  ];
  const host = createBrowserHost({
    createWebSocket() {
      return results.shift();
    },
  });

  await assert.rejects(
    host.connect("wss://example.test", "secret", "session"),
    /upgrade denied/,
  );
  await assert.rejects(
    host.connect("wss://example.test", "secret", "session"),
    /must return a WebSocket or a connection descriptor/,
  );
  await assert.rejects(
    host.connect("wss://example.test", "secret", "session"),
    /closed during connection/,
  );
  assert.equal(
    JSON.parse(await host.connect("wss://example.test", "secret", "session")).handle,
    1,
  );
});

test("browser host settles a pre-opened socket exactly once", async () => {
  const first = new FakeWebSocket("wss://first.test");
  first.readyState = FakeWebSocket.OPEN;
  const second = new FakeWebSocket("wss://second.test");
  second.readyState = FakeWebSocket.OPEN;
  const sockets = [first, second];
  const host = createBrowserHost({
    createWebSocket() {
      return sockets.shift();
    },
  });

  assert.equal(
    JSON.parse(await host.connect(first.url, "secret", "session")).handle,
    1,
  );
  first.open();
  assert.equal(
    JSON.parse(await host.connect(second.url, "secret", "session")).handle,
    2,
  );
});

test("browser host bounds queued receives and buffered sends", async () => {
  const host = createBrowserHost({
    WebSocketImpl: FakeWebSocket,
    maxQueuedMessages: 1,
    maxQueuedBytes: 1_024,
    maxBufferedSendBytes: 4,
  });
  const connecting = host.connect("ws://example.test", "not-forwarded", "session");
  const socket = FakeWebSocket.instances.at(-1);
  socket.open();
  await connecting;

  socket.message("first");
  socket.message("second");
  assert.match(JSON.parse(await host.next(1, 10)).detail, /receive queue exceeded/);
  assert.equal(socket.closedCode, 1009);

  const secondHost = createBrowserHost({
    WebSocketImpl: FakeWebSocket,
    maxBufferedSendBytes: 4,
  });
  const secondConnecting = secondHost.connect("ws://example.test", "not-forwarded", "session");
  const secondSocket = FakeWebSocket.instances.at(-1);
  secondSocket.open();
  await secondConnecting;
  const send = JSON.parse(await secondHost.send(1, "12345"));
  assert.equal(send.ok, false);
  assert.match(send.error, /buffered WebSocket sends exceeded/);
});

test("browser host keeps zero-argument tool calls wire-complete", async () => {
  const host = createBrowserHost({
    WebSocketImpl: FakeWebSocket,
    tools: {
      runtimeInfo: {
        description: "Describe the runtime.",
        parameters: { type: "object", additionalProperties: false },
        handler: () => ({ runtime: "browser" }),
      },
    },
  });

  const execution = JSON.parse(
    await host.executeCode("text(await tools.runtimeInfo());"),
  );
  assert.equal(execution.success, true);
  assert.equal(execution.nested_calls[0].input, null);
  assert.deepEqual(JSON.parse(execution.nested_calls[0].output), {
    runtime: "browser",
  });
});

test("browser host passes session context and emits generated images", async () => {
  let context;
  const host = createBrowserHost({
    WebSocketImpl: FakeWebSocket,
    tools: {
      makeImage: {
        handler: (_input, received) => {
          context = received;
          return { image_url: "data:image/png;base64,a" };
        },
      },
    },
  });

  const execution = JSON.parse(await host.executeCode(
    "generatedImage(await tools.makeImage({ prompt: 'demo' }));",
    "session-image",
    "call-image",
  ));
  assert.equal(execution.success, true);
  assert.equal(context.sessionId, "session-image");
  assert.equal(context.parentCallId, "call-image");
  assert.equal(context.callId, "call-image/code-1");
  assert.equal(execution.output[1].type, "input_image");
});

test("Code Mode snapshots definitions, inputs, outputs, and handlers at its boundary", async () => {
  const parameters = {
    type: "object",
    properties: { value: { type: "integer" } },
  };
  const configuration = {
    inspect: {
      description: "Inspect without mutating the recorded call.",
      parameters,
      handler(input) {
        input.value = 99;
        return [{ type: "input_text", text: "original output" }];
      },
    },
  };
  const host = createBrowserHost({
    WebSocketImpl: FakeWebSocket,
    tools: configuration,
  });

  parameters.properties.value.type = "string";
  configuration.inspect.handler = () => "replacement";
  configuration.extra = {
    description: "Added too late.",
    parameters: { type: "object" },
    handler: () => "extra",
  };

  const definitions = JSON.parse(host.toolDefinitions());
  assert.equal(definitions.length, 1);
  assert.equal(definitions[0].parameters.properties.value.type, "integer");

  const execution = JSON.parse(await host.executeCode(
    [
      "const output = await tools.inspect({ value: 7 });",
      "output[0].text = 'mutated after return';",
      "text(output);",
    ].join("\n"),
    "session-snapshot",
    "call-snapshot",
  ));
  assert.equal(execution.success, true);
  assert.deepEqual(execution.nested_calls[0].input, { value: 7 });
  assert.deepEqual(execution.nested_calls[0].output, [{
    type: "input_text",
    text: "original output",
  }]);
});

class FakeWebSocket {
  static OPEN = 1;
  static instances = [];

  constructor(url) {
    this.url = url;
    this.readyState = 0;
    this.bufferedAmount = 0;
    this.listeners = new Map();
    this.sent = [];
    FakeWebSocket.instances.push(this);
  }

  addEventListener(type, listener) {
    const listeners = this.listeners.get(type) || [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  open() {
    this.readyState = FakeWebSocket.OPEN;
    this.emit("open", {});
  }

  message(data) {
    this.emit("message", { data });
  }

  send(message) { this.sent.push(message); }
  close(code, reason = "") {
    this.readyState = 3;
    this.closedCode = code;
    this.emit("close", { code, reason });
  }

  emit(type, event) {
    for (const listener of this.listeners.get(type) || []) listener(event);
  }
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((next, fail) => {
    resolve = next;
    reject = fail;
  });
  return { promise, reject, resolve };
}

function failingSocket(url, error, asynchronous = false) {
  const socket = new FakeWebSocket(url);
  socket.readyState = FakeWebSocket.OPEN;
  socket.close = () => {
    socket.readyState = 3;
    if (asynchronous) return Promise.reject(error);
    throw error;
  };
  return socket;
}
