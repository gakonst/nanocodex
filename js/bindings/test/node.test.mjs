import assert from "node:assert/strict";
import { test } from "node:test";
import { WebSocketServer } from "ws";

import { Actions, Agent } from "../node/index.mjs";

test("Node-hosted WASM preserves follow-ons, cache identity, events, and custom tools", async () => {
  const server = await startServer();
  const events = [];
  const agent = await Agent.create({
    apiKey: "test-key",
    websocketUrl: server.url,
    thinking: "none",
    reasoningMode: "pro",
    sessionId: "wasm-session",
    tools: {
      multiply: {
        description: "Multiply two integers.",
        parameters: {
          type: "object",
          properties: { left: { type: "integer" }, right: { type: "integer" } },
          required: ["left", "right"],
          additionalProperties: false,
        },
        handler: ({ left, right }) => left * right,
      },
    },
  });
  const watch = agent.events.watch();
  watch.onEvent((event) => events.push(event));

  const scenario = (async () => {
    const socket = await server.connection;
    assert.equal(socket.request.headers.authorization, "Bearer test-key");
    assert.equal(socket.request.headers["session-id"], "wasm-session");
    const reader = messageReader(socket);

    const warmup = await reader.next();
    assert.equal(warmup.generate, false);
    assert.equal(warmup.reasoning.mode, "pro");
    assert.equal(warmup.reasoning.effort, "none");
    assert.equal(warmup.input[0].tools[0].name, "exec");
    assert.match(warmup.input[0].tools[0].description, /tools\.multiply/);
    sendWarmup(socket, "resp-warmup");

    const generation = await reader.next();
    assert.equal(generation.previous_response_id, "resp-warmup");
    assert.equal(generation.reasoning.effort, "none");
    assert.equal(generation.service_tier, undefined);
    sendCompleted(socket, "resp-tool", [{
      type: "custom_tool_call",
      call_id: "call-exec",
      name: "exec",
      input: "text(await tools.multiply({ left: 6, right: 7 }));",
    }]);

    const continuation = await reader.next();
    assert.equal(continuation.previous_response_id, "resp-tool");
    assert.equal(continuation.reasoning.effort, "none");
    assert.match(JSON.stringify(continuation.input), /42/);
    sendFinal(socket, "resp-first", "42");

    const followOn = await reader.next();
    assert.equal(followOn.previous_response_id, "resp-first");
    assert.equal(followOn.reasoning.effort, "high");
    assert.equal(followOn.service_tier, "priority");
    assert.match(JSON.stringify(followOn.input), /Add one/);
    sendFinal(socket, "resp-second", "43");
  })();

  const first = agent.turn.prompt({ input: "Use multiply for 6 × 7." });
  assert.equal(await first.result(), "42");
  await agent.session.setThinking("high");
  await agent.session.setFastMode(true);
  const second = Actions.turn.prompt(agent, { input: "Add one to that result." });
  assert.equal(await Actions.turn.getResult(second), "43");
  await scenario;
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(server.connections, 1);
  assert.equal(events.filter((event) => event.type === "run.completed").length, 2);
  assert.ok(events.some((event) => event.type === "tool.call" && event.payload.tool === "multiply"));
  watch.off();
  agent.dispose();
  await server.close();
});

test("WASM snapshots resume authoritative history in a fresh agent", async () => {
  const originalServer = await startServer();
  const original = await Agent.create({
    apiKey: "test-key",
    websocketUrl: originalServer.url,
    thinking: "none",
    instructions: "durable wasm instructions",
    sessionId: "original-session",
  });
  const originalScenario = (async () => {
    const socket = await originalServer.connection;
    const reader = messageReader(socket);
    await reader.next();
    sendWarmup(socket, "resp-warmup");
    await reader.next();
    sendFinal(socket, "resp-first", "stored");
  })();
  const first = original.turn.prompt({ input: "remember cobalt" });
  assert.equal(await first.result(), "stored");
  const snapshot = first.snapshot();
  assert.equal(snapshot.version, 1);
  assert.equal(Actions.turn.getSnapshot(first).model, snapshot.model);
  await originalScenario;
  original.dispose();
  await originalServer.close();

  const resumedServer = await startServer();
  const resumed = await Agent.create({
    apiKey: "test-key",
    websocketUrl: resumedServer.url,
    thinking: "none",
    instructions: "durable wasm instructions",
    sessionId: "resumed-session",
    resume: snapshot,
  });
  const resumedScenario = (async () => {
    const socket = await resumedServer.connection;
    assert.equal(socket.request.headers["session-id"], "resumed-session");
    const request = await messageReader(socket).next();
    assert.equal(request.previous_response_id, undefined);
    assert.equal(request.prompt_cache_key, snapshot.prompt_cache_key);
    assert.match(JSON.stringify(request.input), /remember cobalt/);
    assert.match(JSON.stringify(request.input), /what did I ask/);
    sendFinal(socket, "resp-resumed", "cobalt");
  })();
  assert.equal(
    await resumed.turn.prompt({ input: "what did I ask you to remember?" }).result(),
    "cobalt",
  );
  await resumedScenario;

  const spawnedConnection = new Promise((resolve) => {
    resumedServer.websocketServer.once("connection", (socket, request) => {
      socket.request = request;
      resolve(socket);
    });
  });
  const spawned = await resumed.session.spawn();
  const spawnedScenario = (async () => {
    const socket = await spawnedConnection;
    const reader = messageReader(socket);
    const warmup = await reader.next();
    assert.equal(warmup.prompt_cache_key, snapshot.prompt_cache_key);
    sendWarmup(socket, "resp-spawn-warmup");
    await reader.next();
    sendFinal(socket, "resp-spawned", "fresh");
  })();
  assert.equal(await spawned.turn.prompt({ input: "start fresh" }).result(), "fresh");
  await spawnedScenario;
  spawned.dispose();
  resumed.dispose();
  await resumedServer.close();
});

test("independent agents keep their host connections isolated", async () => {
  const leftServer = await startServer();
  const rightServer = await startServer();
  const left = await Agent.create({
    apiKey: "left-key",
    websocketUrl: leftServer.url,
    thinking: "none",
    sessionId: "left-session",
    tools: {
      leftTool: {
        description: "Only the left agent can see this tool.",
        parameters: { type: "object" },
        handler: async () => "left",
      },
    },
  });
  const right = await Agent.create({
    apiKey: "right-key",
    websocketUrl: rightServer.url,
    thinking: "none",
    sessionId: "right-session",
    tools: {
      rightTool: {
        description: "Only the right agent can see this tool.",
        parameters: { type: "object" },
        handler: async () => "right",
      },
    },
  });

  const leftTools = globalThis.nanocodexHost.toolDefinitions("left-session");
  const rightTools = globalThis.nanocodexHost.toolDefinitions("right-session");
  assert.match(leftTools, /leftTool/);
  assert.doesNotMatch(leftTools, /rightTool/);
  assert.match(rightTools, /rightTool/);
  assert.doesNotMatch(rightTools, /leftTool/);

  const serve = async (server, sessionId, message) => {
    const socket = await server.connection;
    assert.equal(socket.request.headers["session-id"], sessionId);
    const reader = messageReader(socket);
    await reader.next();
    sendWarmup(socket, `${sessionId}-warmup`);
    await reader.next();
    sendFinal(socket, `${sessionId}-final`, message);
  };
  const scenarios = Promise.all([
    serve(leftServer, "left-session", "LEFT"),
    serve(rightServer, "right-session", "RIGHT"),
  ]);

  // Prompt the first agent only after the second factory has installed its
  // host. This regresses the old realm-global host overwrite.
  const [leftResult, rightResult] = await Promise.all([
    left.turn.prompt({ input: "left" }).result(),
    right.turn.prompt({ input: "right" }).result(),
  ]);
  assert.equal(leftResult, "LEFT");
  assert.equal(rightResult, "RIGHT");
  await scenarios;

  left.dispose();
  right.dispose();
  await Promise.all([leftServer.close(), rightServer.close()]);
});

async function startServer() {
  const websocketServer = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise((resolve, reject) => {
    websocketServer.once("listening", resolve);
    websocketServer.once("error", reject);
  });
  let resolveConnection;
  const connection = new Promise((resolve) => { resolveConnection = resolve; });
  const state = {
    websocketServer,
    connection,
    connections: 0,
    get url() {
      return `ws://127.0.0.1:${websocketServer.address().port}`;
    },
    close() {
      for (const socket of websocketServer.clients) socket.terminate();
      return new Promise((resolve, reject) => websocketServer.close((error) => error ? reject(error) : resolve()));
    },
  };
  websocketServer.on("connection", (socket, request) => {
    state.connections += 1;
    socket.request = request;
    resolveConnection(socket);
  });
  return state;
}

function messageReader(socket) {
  const messages = [];
  let waiter;
  socket.on("message", (data) => {
    const value = JSON.parse(data.toString("utf8"));
    if (waiter) {
      const resolve = waiter;
      waiter = undefined;
      resolve(value);
    } else {
      messages.push(value);
    }
  });
  return {
    next() {
      if (messages.length) return Promise.resolve(messages.shift());
      return new Promise((resolve) => { waiter = resolve; });
    },
  };
}

function sendWarmup(socket, responseId) {
  socket.send(JSON.stringify({
    type: "response.completed",
    response: { id: responseId, usage: null },
  }));
}

function sendFinal(socket, responseId, text) {
  sendCompleted(socket, responseId, [{
    type: "message",
    role: "assistant",
    content: [{ type: "output_text", text }],
  }]);
}

function sendCompleted(socket, responseId, output) {
  socket.send(JSON.stringify({
    type: "response.completed",
    response: {
      id: responseId,
      status: "completed",
      output,
      usage: {
        input_tokens: 10,
        input_tokens_details: { cached_tokens: 5 },
        output_tokens: 2,
        output_tokens_details: { reasoning_tokens: 1 },
        total_tokens: 12,
      },
    },
  }));
}
