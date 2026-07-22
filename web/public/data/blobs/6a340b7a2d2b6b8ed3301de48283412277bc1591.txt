import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "node:test";
import { WebSocketServer } from "ws";

import { createNodeHost } from "../node/host.mjs";

const require = createRequire(import.meta.url);

test("Node-hosted WASM preserves follow-ons, cache identity, events, and custom tools", async () => {
  const server = await startServer();
  const events = [];
  globalThis.nanocodexHost = createNodeHost({
    onEvent: (eventJson) => events.push(JSON.parse(eventJson)),
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
  const { Nanocodex } = require("../pkg-node/nanocodex.js");
  const agent = new Nanocodex(JSON.stringify({
    api_key: "test-key",
    thinking: "none",
    reasoning_mode: "pro",
    websocket_url: server.url,
    session_id: "wasm-session",
  }));

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
    sendCompleted(socket, "resp-tool", [{
      type: "custom_tool_call",
      call_id: "call-exec",
      name: "exec",
      input: "text(await tools.multiply({ left: 6, right: 7 }));",
    }]);

    const continuation = await reader.next();
    assert.equal(continuation.previous_response_id, "resp-tool");
    assert.match(JSON.stringify(continuation.input), /42/);
    sendFinal(socket, "resp-first", "42");

    const followOn = await reader.next();
    assert.equal(followOn.previous_response_id, "resp-first");
    assert.match(JSON.stringify(followOn.input), /Add one/);
    sendFinal(socket, "resp-second", "43");
  })();

  const first = agent.prompt("Use multiply for 6 × 7.");
  assert.equal(await first.result(), "42");
  const second = agent.prompt("Add one to that result.");
  assert.equal(await second.result(), "43");
  await scenario;
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(server.connections, 1);
  assert.equal(events.filter((event) => event.type === "run.completed").length, 2);
  assert.ok(events.some((event) => event.type === "tool.call" && event.payload.tool === "multiply"));
  await server.close();
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
