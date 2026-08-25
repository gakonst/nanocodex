import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { test } from "node:test";
import { WebSocketServer } from "ws";

import { Actions, Agent, Transport } from "../node/index.mjs";
import { createNodeHost } from "../node/host.mjs";
import { createMemoryDurabilityStore } from "../runtime/durability-store.mjs";

const SESSION_IDS = Object.freeze({
  primary: "018f1f9a-7b3c-7a01-8000-000000000001",
  original: "018f1f9a-7b3c-7a02-8000-000000000002",
  resumed: "018f1f9a-7b3c-7a03-8000-000000000003",
  embedded: "018f1f9a-7b3c-7a04-8000-000000000004",
  left: "018f1f9a-7b3c-7a05-8000-000000000005",
  right: "018f1f9a-7b3c-7a06-8000-000000000006",
});

const createWarmAgent = ({ apiKey, websocketUrl, ...options }) => Agent.create({
  ...options,
  transport: Transport.openAi({ apiKey, websocketUrl, websocketWarmup: true }),
});
const PACKAGE_VERSION = JSON.parse(
  await readFile(new URL("../package.json", import.meta.url), "utf8"),
).version;

async function waitForToolDefinition(host, name) {
  const deadline = performance.now() + 1_000;
  while (performance.now() < deadline) {
    const definitions = JSON.parse(host.toolDefinitions());
    if (definitions.some((definition) => definition.name === name)) return definitions;
    await new Promise((resolve) => setImmediate(resolve));
  }
  throw new Error(`MCP discovery did not publish ${name}`);
}

test("Node host opens application sockets through MPP", async () => {
  const socket = new ManagedSocket();
  const endpoints = [];
  const host = createNodeHost({
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
  host.close(1);
});

test("Node host loads and calls deferred Mercator MCP tools", async () => {
  const calls = [];
  const host = createNodeHost({
    mcpServers: {
      mercator: {
        description: "Deterministic Mercator fixture.",
        client: {
          async listTools() {
            return {
              tools: [{
                name: "search_services",
                description: "Search paid services.",
                inputSchema: {
                  type: "object",
                  properties: { query: { type: "string" } },
                  required: ["query"],
                },
              }],
            };
          },
          async callTool(input) {
            calls.push(input);
            return { content: [{ type: "text", text: "node-mercator-ok" }] };
          },
        },
      },
    },
  });

  try {
    await host.ready();
    const definitions = await waitForToolDefinition(
      host,
      "mcp__mercator__search_services",
    );
    assert.deepEqual(definitions.map((definition) => definition.name ?? definition.type), [
      "tool_search",
      "list_mcp_resources",
      "list_mcp_resource_templates",
      "read_mcp_resource",
      "mcp__mercator__search_services",
    ]);
    assert.equal(definitions[4].defer_loading, true);
    const execution = JSON.parse(await host.executeCode(
      "text(await tools.mcp__mercator__search_services({ query: 'weather' }));",
      "node-session",
      "node-exec",
    ));
    assert.equal(execution.success, true);
    assert.match(JSON.stringify(execution.output), /node-mercator-ok/);
    assert.deepEqual(calls, [{
      name: "search_services",
      arguments: { query: "weather" },
    }]);
  } finally {
    await host.dispose();
  }
});

test("Node host preserves structured WebSocket handshake rejection detail", async () => {
  const server = createServer((_request, response) => {
    response.writeHead(429, {
      "content-type": "application/json",
      "retry-after": "3",
    });
    const body = new TextEncoder().encode('{"error":"slow 🤖"}');
    const emoji = body.indexOf(0xf0);
    response.write(body.subarray(0, emoji + 2));
    response.end(body.subarray(emoji + 2));
  });
  await new Promise((resolve, reject) => {
    server.listen(0, "127.0.0.1", resolve);
    server.once("error", reject);
  });
  const endpoint = `ws://127.0.0.1:${server.address().port}`;

  try {
    await assert.rejects(
      createNodeHost().connect(endpoint, "test-key", SESSION_IDS.primary),
      (error) => {
        assert.equal(error.status, 429);
        assert.equal(error.body, '{"error":"slow 🤖"}');
        assert.equal(error.retryAfter, 3);
        return true;
      },
    );
  } finally {
    await new Promise((resolve, reject) => {
      server.close((error) => error ? reject(error) : resolve());
    });
  }
});

test("Node-hosted WASM preserves follow-ons, cache identity, events, and custom tools", async () => {
  const server = await startServer();
  const events = [];
  const agent = await createWarmAgent({
    apiKey: "test-key",
    websocketUrl: server.url,
    thinking: "none",
    reasoningMode: "pro",
    sessionId: SESSION_IDS.primary,
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
    assert.equal(socket.request.headers["user-agent"], `nanocodex-wasm/${PACKAGE_VERSION}`);
    assert.equal(socket.request.headers["session-id"], SESSION_IDS.primary);
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
    assert.equal(followOn.previous_response_id, undefined);
    assert.equal(followOn.reasoning.effort, "high");
    assert.equal(followOn.service_tier, "priority");
    const replay = JSON.stringify(followOn.input);
    assert.match(replay, /Use multiply/);
    assert.match(replay, /42/);
    assert.match(replay, /Add one/);
    sendFinal(socket, "resp-second", "43");
  })();

  const firstTurn = agent.turn.prompt({ input: "Use multiply for 6 × 7." });
  const first = await firstTurn.result();
  assert.equal(first.finalMessage, "42");
  const usage = await first.usage();
  assert.deepEqual(usage, {
    input_tokens: 20,
    cached_input_tokens: 10,
    cache_write_input_tokens: 0,
    output_tokens: 4,
    reasoning_output_tokens: 2,
    total_tokens: 24,
    estimated_cost: {
      usd: "0.000175",
      input_usd: "0.00005",
      cached_input_usd: "0.000005",
      cache_write_input_usd: "0",
      output_usd: "0.00012",
      service_tier: "standard",
    },
    cost_status: "estimated_from_usage",
  });
  assert.strictEqual(await Actions.turn.getUsage(first), usage);
  await agent.session.setThinking("high");
  await agent.session.setFastMode(true);
  const second = await Actions.turn.getResult(
    Actions.turn.prompt(agent, { input: "Add one to that result." }),
  );
  assert.equal(second.finalMessage, "43");
  await scenario;
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(server.connections, 1);
  assert.equal(events.filter((event) => event.type === "run.completed").length, 2);
  assert.equal(
    events.find((event) => event.type === "run.completed")?.payload.estimated_cost.usd,
    "0.000175",
  );
  assert.ok(events.some((event) => event.type === "tool.call" && event.payload.tool === "multiply"));
  watch.off();
  agent.dispose();
  await server.close();
});

test("a durable Node-hosted root runs the canonical in-memory Rust subagent task tree", async () => {
  const server = await startServer();
  const decoyServer = await startServer();
  const events = [];
  const rootToolContexts = [];
  const durabilityId = "node-durable-root-subagents";
  const agent = await createWarmAgent({
    apiKey: "test-key",
    websocketUrl: server.url,
    thinking: "none",
    sessionId: "018f1f9a-7b3c-7a08-8000-000000000008",
    durability: createMemoryDurabilityStore(durabilityId),
    durabilityId,
    tools: [
      {
        name: "rootOnly",
        description: "Only the orchestrator family can see this tool.",
        parameters: { type: "object" },
        handler: (_input, context) => {
          rootToolContexts.push(context);
          return "root";
        },
      },
    ],
  });
  // Make another host realm globally active before the Rust child is built.
  // Child definitions must still inherit their own root's host.
  const decoy = await Agent.create({
    transport: Transport.openAi({ apiKey: "decoy", websocketUrl: decoyServer.url }),
    tools: {
      decoyOnly: {
        description: "Only the decoy family can see this tool.",
        parameters: { type: "object" },
        handler: () => "decoy",
      },
    },
  });
  const watch = agent.events.watch({ includeAllSessions: true });
  watch.onEvent((event) => events.push(event));

  let childSessionId;
  const scenario = (async () => {
    const rootSocket = await server.connection;
    const rootReader = messageReader(rootSocket);
    const rootWarmup = await rootReader.next();
    assert.deepEqual(
      rootWarmup.input[0].tools.map((tool) => tool.name).sort(),
      [
        "close_agent",
        "exec",
        "interrupt_agent",
        "list_agents",
        "send_agent_message",
        "spawn_agent",
        "submit_result",
        "wait_agent",
      ],
    );
    sendWarmup(rootSocket, "root-warmup");

    const rootGeneration = await rootReader.next();
    const childConnection = new Promise((resolve) => {
      server.websocketServer.once("connection", (socket, request) => {
        socket.request = request;
        resolve(socket);
      });
    });
    sendCompleted(rootSocket, "root-spawn", [{
      type: "function_call",
      call_id: "call-spawn",
      name: "spawn_agent",
      arguments: JSON.stringify({
        role: "reviewer",
        task: "Return the word portable.",
        output_schema: {
          type: "object",
          properties: { report: { type: "string" } },
          required: ["report"],
          additionalProperties: false,
        },
      }),
    }]);

    const childSocket = await childConnection;
    childSessionId = childSocket.request.headers["session-id"];
    assert.ok(childSessionId);
    const childReader = messageReader(childSocket);
    const childWarmup = await childReader.next();
    assert.equal(childWarmup.input[0].tools.some((tool) => tool.name === "send_agent_message"), true);
    assert.match(childWarmup.input[0].tools[0].description, /tools\.rootOnly/);
    assert.doesNotMatch(childWarmup.input[0].tools[0].description, /tools\.decoyOnly/);
    sendWarmup(childSocket, "child-warmup");

    const rootSpawned = await rootReader.next();
    assert.equal(rootSpawned.input[0].call_id, "call-spawn");
    assert.deepEqual(JSON.parse(rootSpawned.input[0].output), {
      agent_id: 1,
      role: "reviewer",
      status: { state: "running" },
    });
    sendCompleted(rootSocket, "root-wait", [{
      type: "function_call",
      call_id: "call-wait",
      name: "wait_agent",
      arguments: JSON.stringify({ agent_ids: [1], timeout_ms: 5_000 }),
    }]);

    await childReader.next();
    sendCompleted(childSocket, "child-tool", [{
      type: "custom_tool_call",
      call_id: "call-child-exec",
      name: "exec",
      input: "text(await tools.rootOnly({}));",
    }]);
    const childExecuted = await childReader.next();
    assert.equal(childExecuted.input[0].call_id, "call-child-exec");
    assert.match(JSON.stringify(childExecuted.input[0].output), /root/);
    sendCompleted(childSocket, "child-submit", [{
      type: "function_call",
      call_id: "call-submit",
      name: "submit_result",
      arguments: JSON.stringify({ turn_token: 1, output: { report: "portable" } }),
    }]);
    const childSubmitted = await childReader.next();
    assert.deepEqual(JSON.parse(childSubmitted.input[0].output), { accepted: true });
    sendFinal(childSocket, "child-final", "submitted");

    const rootWaited = await rootReader.next();
    const waited = JSON.parse(rootWaited.input[0].output);
    assert.equal(waited.timed_out, false);
    assert.deepEqual(waited.agents[0].status, {
      state: "completed",
      output: { report: "portable" },
    });
    sendFinal(rootSocket, "root-final", "portable");
  })();

  try {
    const result = await agent.turn.prompt({ input: "Delegate this check." }).result();
    assert.equal(result.finalMessage, "portable");
    await scenario;
    assert.equal(rootToolContexts.length, 1);
    assert.equal(rootToolContexts[0].sessionId, childSessionId);
    assert.deepEqual(rootToolContexts[0].subagent, {
      agentId: "1",
      parentAgentId: null,
      sessionId: childSessionId,
      role: "reviewer",
      task: "Return the word portable.",
    });
    assert.ok(events.some((event) => event.request_id === childSessionId));
  } finally {
    watch.off();
    await agent.session.shutdown();
    assert.throws(
      () => globalThis.nanocodexHost.executeTool(
        "missing",
        "{}",
        childSessionId,
        "after-shutdown",
      ),
      /no Nanocodex host is active/,
    );
    await decoy.session.shutdown();
    await server.close();
    await decoyServer.close();
  }
});

test("WASM snapshots rebind deployed policy while retaining authoritative history", async () => {
  const originalServer = await startServer();
  const original = await createWarmAgent({
    apiKey: "test-key",
    websocketUrl: originalServer.url,
    thinking: "none",
    instructions: "instructions from the old WASM deployment",
    sessionId: SESSION_IDS.original,
    workspace: "/virtual/original-workspace",
    tools: {
      oldDeploymentTool: {
        description: "Only the old deployment exposes this tool.",
        parameters: { type: "object", additionalProperties: false },
        handler: async () => "old",
      },
    },
  });
  const originalScenario = (async () => {
    const socket = await originalServer.connection;
    const reader = messageReader(socket);
    await reader.next();
    sendWarmup(socket, "resp-warmup");
    await reader.next();
    sendFinal(socket, "resp-first", "stored");
  })();
  const first = await original.turn.prompt({ input: "remember cobalt" }).result();
  assert.equal(first.finalMessage, "stored");
  const snapshot = await first.snapshot();
  assert.equal(snapshot.version, 1);
  assert.equal(snapshot.workspace, "/virtual/original-workspace");
  assert.strictEqual(await Actions.turn.getSnapshot(first), snapshot);
  await originalScenario;
  original.dispose();
  await originalServer.close();

  const resumedServer = await startServer();
  const resumed = await createWarmAgent({
    apiKey: "test-key",
    websocketUrl: resumedServer.url,
    thinking: "none",
    instructions: "instructions from the new WASM deployment",
    sessionId: SESSION_IDS.resumed,
    resume: snapshot,
    tools: {
      newDeploymentTool: {
        description: "Only the new deployment exposes this tool.",
        parameters: { type: "object", additionalProperties: false },
        handler: async () => "new",
      },
    },
  });
  const resumedScenario = (async () => {
    const socket = await resumedServer.connection;
    assert.equal(socket.request.headers["session-id"], SESSION_IDS.resumed);
    const request = await messageReader(socket).next();
    assert.equal(request.previous_response_id, undefined);
    assert.equal(request.prompt_cache_key, snapshot.prompt_cache_key);
    const input = JSON.stringify(request.input);
    assert.match(input, /instructions from the new WASM deployment/);
    assert.match(input, /newDeploymentTool/);
    assert.doesNotMatch(input, /instructions from the old WASM deployment/);
    assert.doesNotMatch(input, /oldDeploymentTool/);
    assert.match(input, /remember cobalt/);
    assert.match(input, /what did I ask/);
    sendFinal(socket, "resp-resumed", "cobalt");
  })();
  assert.equal(
    (await resumed.turn.prompt({ input: "what did I ask you to remember?" }).result()).finalMessage,
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
  assert.equal(
    (await spawned.turn.prompt({ input: "start fresh" }).result()).finalMessage,
    "fresh",
  );
  await spawnedScenario;
  spawned.dispose();
  resumed.dispose();
  await resumedServer.close();
});

test("Node can load an application-owned web module and resume Codex rollout history", async () => {
  const server = await startServer();
  const wasm = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const canonicalContext = {
    type: "message",
    role: "user",
    content: [{ type: "input_text", text: "remember amber" }],
  };
  const snapshot = {
    version: 1,
    model: "gpt-5.6-sol",
    lineage_id: "codex-rollout-lineage",
    prompt_cache_key: "codex-rollout-lineage",
    workspace: process.cwd(),
    canonical_context: canonicalContext,
    history: [
      canonicalContext,
      {
        type: "message",
        role: "assistant",
        content: [{ type: "output_text", text: "stored" }],
        status: "completed",
      },
    ],
  };
  const agent = await createWarmAgent({
    apiKey: "test-key",
    module: wasm,
    websocketUrl: server.url,
    thinking: "none",
    sessionId: SESSION_IDS.embedded,
    resume: snapshot,
  });
  const scenario = (async () => {
    const socket = await server.connection;
    const request = await messageReader(socket).next();
    assert.equal(request.previous_response_id, undefined);
    assert.equal(request.prompt_cache_key, snapshot.prompt_cache_key);
    assert.match(JSON.stringify(request.input), /remember amber/);
    assert.match(JSON.stringify(request.input), /what color/);
    sendFinal(socket, "resp-rollout-resumed", "amber");
  })();

  assert.equal(
    (await agent.turn.prompt({ input: "what color did I ask you to remember?" }).result())
      .finalMessage,
    "amber",
  );
  await scenario;
  agent.dispose();
  await server.close();
});

test("independent agents keep their host connections isolated", async () => {
  const leftServer = await startServer();
  const rightServer = await startServer();
  const left = await createWarmAgent({
    apiKey: "left-key",
    websocketUrl: leftServer.url,
    thinking: "none",
    sessionId: SESSION_IDS.left,
    tools: {
      leftTool: {
        description: "Only the left agent can see this tool.",
        parameters: { type: "object" },
        handler: async () => "left",
      },
    },
  });
  const right = await createWarmAgent({
    apiKey: "right-key",
    websocketUrl: rightServer.url,
    thinking: "none",
    sessionId: SESSION_IDS.right,
    tools: {
      rightTool: {
        description: "Only the right agent can see this tool.",
        parameters: { type: "object" },
        handler: async () => "right",
      },
    },
  });

  const serve = async (server, sessionId, message, visibleTool, hiddenTool) => {
    const socket = await server.connection;
    assert.equal(socket.request.headers["session-id"], sessionId);
    const reader = messageReader(socket);
    const warmup = await reader.next();
    assert.match(warmup.input[0].tools[0].description, new RegExp(`tools\\.${visibleTool}`));
    assert.doesNotMatch(warmup.input[0].tools[0].description, new RegExp(`tools\\.${hiddenTool}`));
    sendWarmup(socket, `${sessionId}-warmup`);
    await reader.next();
    sendFinal(socket, `${sessionId}-final`, message);
  };
  const scenarios = Promise.all([
    serve(leftServer, SESSION_IDS.left, "LEFT", "leftTool", "rightTool"),
    serve(rightServer, SESSION_IDS.right, "RIGHT", "rightTool", "leftTool"),
  ]);

  // Prompt the first agent only after the second factory has installed its
  // host. This regresses the old realm-global host overwrite.
  const [leftResult, rightResult] = (await Promise.all([
    left.turn.prompt({ input: "left" }).result(),
    right.turn.prompt({ input: "right" }).result(),
  ])).map((result) => result.finalMessage);
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

class ManagedSocket extends EventTarget {
  constructor() {
    super();
    this.readyState = 1;
    this.sent = [];
  }

  send(message) {
    this.sent.push(message);
  }

  close(code = 1000, reason = "") {
    this.readyState = 3;
    const event = new Event("close");
    Object.defineProperties(event, {
      code: { value: code },
      reason: { value: reason },
    });
    this.dispatchEvent(event);
  }

  message(data) {
    this.dispatchEvent(new MessageEvent("message", { data }));
  }
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
