import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { test } from "node:test";
import { WebSocketServer } from "ws";

import { Actions, Agent, ChatGptSubscription, Subagents, Transport } from "../node/index.mjs";
import { createMemoryChatGptSubscriptionStore } from "../index.mjs";
import { createNodeHost } from "../node/host.mjs";
import { createMemoryDurabilityStore } from "../runtime/durability-store.mjs";
import { createWorkspace } from "../runtime/workspace.mjs";
import { createTools } from "../tools/Tools.mjs";

const SESSION_IDS = Object.freeze({
  primary: "018f1f9a-7b3c-7a01-8000-000000000001",
  original: "018f1f9a-7b3c-7a02-8000-000000000002",
  resumed: "018f1f9a-7b3c-7a03-8000-000000000003",
  embedded: "018f1f9a-7b3c-7a04-8000-000000000004",
  left: "018f1f9a-7b3c-7a05-8000-000000000005",
  right: "018f1f9a-7b3c-7a06-8000-000000000006",
});

const createWarmAgent = ({ apiKey, websocketUrl, ...options }) => Agent.create({
  model: "gpt-5.6-sol", // Legacy fixtures exercise none/pro reasoning and Sol pricing.
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

test("Node host owns one Tools lifecycle and validates Tools-owned MCP policy", async () => {
  const mcp = {
    fixture: {
      client: {
        async listTools() { return { tools: [] }; },
        async close() {},
      },
    },
  };
  const tools = await createTools({ mcp });
  assert.throws(
    () => createNodeHost({ tools, toolMode: "direct" }),
    /remote MCP requires Code Mode/,
  );
  assert.throws(
    () => createNodeHost({ tools, mcpServers: mcp }),
    /MCP is already configured in Tools/,
  );
  const host = createNodeHost({ tools });
  assert.throws(
    () => createNodeHost({ tools }),
    /already belongs to an Agent host/,
  );
  await host.dispose();
  assert.throws(
    () => tools.attach("wss://managed.test/tools"),
    /Tools runtime is closed/,
  );

  const workspace = createWorkspace({ backend: {
    async list() { return []; },
    async readFile() { return new Uint8Array(); },
    async writeFile() {},
    async remove() {},
    async mkdir() {},
  } });
  const workspaceTools = await createTools({ workspace });
  assert.throws(
    () => createNodeHost({ tools: workspaceTools, filesystem: {} }),
    /workspace is already configured in Tools/,
  );
  await workspaceTools.close();
});

test("Node host disposal completes later owners after a tool cleanup failure", async () => {
  const events = [];
  const tools = await createTools({ tools: { failing: {
    handler() {},
    async dispose() {
      events.push("tool");
      throw new Error("tool cleanup failed");
    },
  } } });
  const host = createNodeHost({ tools, onDispose: () => events.push("host") });
  await assert.rejects(host.dispose(), /tool cleanup failed/);
  assert.deepEqual(events.sort(), ["host", "tool"]);
});

test("Node host disposal is one promise across reentrant onDispose", async () => {
  let host;
  host = createNodeHost({ onDispose: () => host.dispose() });
  const closing = host.dispose();
  assert.equal(host.dispose(), closing);
  await closing;
});

test("Node host disposal closes MCP that resolves after disposal starts", async () => {
  let aborted = false;
  const host = createNodeHost({
    mcpServers: {
      delayed: {
        client: {
          listTools(_params, { signal }) {
            signal.addEventListener("abort", () => { aborted = true; }, { once: true });
            return new Promise(() => {});
          },
        },
      },
    },
  });
  await host.dispose();
  assert.equal(aborted, true);
  assert.doesNotMatch(host.toolDefinitions(), /tool_search|mcp__delayed__/);
});

test("Node host readiness preserves MCP construction failures", async () => {
  const host = createNodeHost({ mcpServers: {} });
  await assert.rejects(host.ready());
  await assert.rejects(host.dispose());
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
      "mcp__mercator__search_services",
    ]);
    assert.equal(definitions[1].defer_loading, true);
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
  assert.equal(agent.agentId, SESSION_IDS.primary);
  assert.equal(agent.sessionId, SESSION_IDS.primary);
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
    assert.match(warmup.input[0].tools[0].description, /multiply\(args:/);
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
      usd: "0.000124",
      input_usd: "0.00004",
      cached_input_usd: "0.000004",
      cache_write_input_usd: "0",
      output_usd: "0.00008",
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
    "0.000124",
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
  const eventAgentIds = [];
  const rootToolContexts = [];
  const durabilityId = "node-durable-root-subagents";
  const durability = createMemoryDurabilityStore(durabilityId);
  const agent = await createWarmAgent({
    apiKey: "test-key",
    websocketUrl: server.url,
    thinking: "none",
    sessionId: "018f1f9a-7b3c-7a08-8000-000000000008",
    durability,
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
  watch.onEvent((event, _encodedLength, _encodedEvent, agentId) => {
    events.push(event);
    eventAgentIds.push(agentId);
  });

  let childSessionId;
  const scenario = (async () => {
    const rootSocket = await server.connection;
    const rootProviderSessionId = rootSocket.request.headers["session-id"];
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
        "wait",
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
    assert.equal(childSocket.request.headers["session-id"], rootProviderSessionId);
    childSessionId = childSocket.request.headers["thread-id"];
    assert.ok(childSessionId);
    assert.notEqual(childSessionId, rootProviderSessionId);
    const childReader = messageReader(childSocket);
    const childWarmup = await childReader.next();
    assert.equal(childWarmup.input[0].tools.some((tool) => tool.name === "send_agent_message"), true);
    assert.match(childWarmup.input[0].tools[0].description, /rootOnly/);
    assert.doesNotMatch(childWarmup.input[0].tools[0].description, /decoyOnly/);
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
    assert.equal(waited.agents[0].parent_agent_id, null);
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
    const childEvents = events.filter((event) => event.request_id === childSessionId);
    const childAgentIds = events.flatMap((event, index) =>
      event.request_id === childSessionId ? [eventAgentIds[index]] : []);
    assert.equal(childAgentIds.length, childEvents.length);
    assert.ok(childAgentIds.every((agentId) => agentId === 1));
    assert.ok(eventAgentIds.every((agentId, index) =>
      events[index].request_id === childSessionId ? agentId === 1 : agentId === undefined));
    assert.deepEqual(
      childEvents
        .filter((event) => event.type === "run.started" || event.type === "run.completed")
        .map((event) => event.type),
      ["run.started", "run.completed"],
    );
    assert.deepEqual(
      childEvents
        .filter((event) => event.type === "tool.call")
        .map((event) => event.payload.tool),
      ["exec", "rootOnly", "submit_result"],
    );
    assert.deepEqual(
      childEvents.map((event) => event.seq),
      Array.from({ length: childEvents.length }, (_value, index) => index + 1),
    );
    assert.ok(events.some((event) =>
      event.request_id === agent.sessionId
      && event.type === "tool.call"
      && event.payload.tool === "spawn_agent"));
    assert.ok(events.some((event) =>
      event.request_id === agent.sessionId
      && event.type === "tool.call"
      && event.payload.tool === "wait_agent"));
    const childState = durability.load(childSessionId);
    assert.notEqual(childState.revision, "0");
    assert.match(childState.payload, /nanocodex_durable_state/);
    assert.notDeepEqual(childState, durability.load(durabilityId));
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

test("Node host invokes canonical subagent handlers without a root model turn", async () => {
  const server = await startServer();
  const agent = await createWarmAgent({
    apiKey: "test-key",
    websocketUrl: server.url,
    model: "gpt-6-astra",
    thinking: "low",
    additionalInstructions: "Use the caller's memory tools.",
    sessionId: "018f1f9a-7b3c-7a09-8000-000000000009",
    tools: [{
      name: "find_threads",
      description: "Find one memory thread.",
      parameters: { type: "object" },
      handler: () => ({ thread_id: "thread-memory" }),
    }, ...Subagents.create({ maxConcurrency: 1 })],
  });

  try {
    const startedPromise = Subagents.spawn(agent, {
      role: "memory-search",
      task: "Use find_threads and return its thread ID.",
      model: "luna",
      thinking: "low",
      outputSchema: {
        type: "object",
        properties: { answer: { type: "string" } },
        required: ["answer"],
        additionalProperties: false,
      },
    });
    const childSocket = await bounded(server.connection, "child connection");
    const childReader = messageReader(childSocket);
    const childWarmup = await bounded(childReader.next(), "child warmup");
    assert.equal(childWarmup.model, "gpt-5.6-luna");
    assert.doesNotMatch(childWarmup.input[1].content[0].text, /GPT-6 Astra/);
    assert.match(childWarmup.input[1].content[0].text, /Use the caller's memory tools\.$/);
    sendWarmup(childSocket, "direct-child-warmup");
    const started = await bounded(startedPromise, "direct spawn");
    assert.deepEqual(started, {
      agent_id: 1,
      role: "memory-search",
      status: { state: "running" },
    });
    await bounded(childReader.next(), "child task");
    sendCompleted(childSocket, "direct-find", [{
      type: "function_call",
      call_id: "direct-find-call",
      name: "find_threads",
      arguments: "{}",
    }]);
    const found = await bounded(childReader.next(), "find_threads output");
    assert.deepEqual(JSON.parse(found.input[0].output), { thread_id: "thread-memory" });
    sendCompleted(childSocket, "direct-submit", [{
      type: "function_call",
      call_id: "direct-submit-call",
      name: "submit_result",
      arguments: JSON.stringify({
        turn_token: 1,
        output: { answer: "thread-memory" },
      }),
    }]);
    const submitted = await bounded(childReader.next(), "submit_result output");
    assert.deepEqual(JSON.parse(submitted.input[0].output), { accepted: true });
    sendFinal(childSocket, "direct-final", "submitted");

    const waited = await bounded(Subagents.wait(agent, {
      agentIds: [started.agent_id],
      timeoutMs: 5_000,
    }), "direct wait");
    assert.equal(waited.timed_out, false);
    assert.deepEqual(waited.agents[0].status, {
      state: "completed",
      output: { answer: "thread-memory" },
    });
    const directory = await Subagents.list(agent, { includeCompleted: true });
    assert.deepEqual(directory.agents, [{
      agent_id: started.agent_id,
      role: "memory-search",
      task: "Use find_threads and return its thread ID.",
      parent_agent_id: null,
      status: { state: "completed", output: { answer: "thread-memory" } },
      can_message: true,
      can_manage: true,
    }]);

    const sendPromise = Subagents.send(agent, {
      agentId: started.agent_id,
      message: "Confirm that result one more time.",
      purpose: "question",
    });
    const messageTurn = await bounded(childReader.next(), "direct message turn");
    assert.match(JSON.stringify(messageTurn), /Confirm that result one more time/);
    assert.deepEqual(await bounded(sendPromise, "direct send"), {
      message_id: 1,
      thread_id: 1,
      from: { kind: "root" },
      to_agent_id: started.agent_id,
      disposition: "started",
    });
    sendCompleted(childSocket, "direct-message-submit", [{
      type: "function_call",
      call_id: "direct-message-submit-call",
      name: "submit_result",
      arguments: JSON.stringify({
        turn_token: 2,
        output: { answer: "thread-memory-confirmed" },
      }),
    }]);
    const messageSubmitted = await bounded(childReader.next(), "message submit_result output");
    assert.deepEqual(JSON.parse(messageSubmitted.input[0].output), { accepted: true });
    sendFinal(childSocket, "direct-message-final", "submitted");
    const messageWait = await bounded(Subagents.wait(agent, {
      agentIds: [started.agent_id],
      timeoutMs: 5_000,
    }), "direct message wait");
    assert.deepEqual(messageWait.agents[0].status, {
      state: "completed",
      output: { answer: "thread-memory-confirmed" },
    });
    const childClosed = new Promise((resolve) => childSocket.once("close", resolve));
    const closed = await Subagents.close(agent, started.agent_id);
    assert.deepEqual(closed.agents[0].status, {
      state: "closed",
    });
    await bounded(childClosed, "direct child socket close");
    await assert.rejects(
      Subagents.wait(agent, { agentIds: [started.agent_id], timeoutMs: 0 }),
      /timeoutMs must be greater than zero/,
    );
    const nonRoot = await agent.session.spawn();
    assert.deepEqual(await Subagents.list(nonRoot, { includeCompleted: true }), { agents: [] });
    await assert.rejects(
      Subagents.wait(nonRoot, { agentIds: [started.agent_id], timeoutMs: 1 }),
      /unknown agent_id/,
    );
    const siblingChildConnection = new Promise((resolve) =>
      server.websocketServer.once("connection", resolve));
    const siblingStartedPromise = Subagents.spawn(nonRoot, {
      role: "independent-root-child",
      task: "Wait until the owning root closes this child.",
      outputSchema: true,
    });
    const siblingChildSocket = await bounded(siblingChildConnection, "sibling child connection");
    const siblingChildReader = messageReader(siblingChildSocket);
    await bounded(siblingChildReader.next(), "sibling child warmup");
    sendWarmup(siblingChildSocket, "sibling-child-warmup");
    const siblingStarted = await bounded(siblingStartedPromise, "sibling direct spawn");
    assert.deepEqual((await Subagents.list(nonRoot)).agents.map(({ agent_id }) => agent_id), [
      siblingStarted.agent_id,
    ]);
    assert.equal(
      (await Subagents.list(agent, { includeCompleted: true })).agents.some(
        ({ role }) => role === "independent-root-child",
      ),
      false,
    );
    const siblingChildClosed = new Promise((resolve) => siblingChildSocket.once("close", resolve));
    await Subagents.close(nonRoot, siblingStarted.agent_id);
    await bounded(siblingChildClosed, "sibling child socket close");

    const disposedChildConnection = new Promise((resolve) =>
      server.websocketServer.once("connection", resolve));
    const disposedChildStarted = Subagents.spawn(nonRoot, {
      role: "dispose-cleanup-child",
      task: "Remain active until the owning agent is disposed.",
      outputSchema: true,
    });
    const disposedChildSocket = await bounded(disposedChildConnection, "disposed child connection");
    const disposedChildReader = messageReader(disposedChildSocket);
    await bounded(disposedChildReader.next(), "disposed child warmup");
    sendWarmup(disposedChildSocket, "disposed-child-warmup");
    await bounded(disposedChildStarted, "disposed child spawn");
    const disposedChildClosed = new Promise((resolve) => disposedChildSocket.once("close", resolve));
    nonRoot.dispose();
    await bounded(disposedChildClosed, "disposed root child close");
    assert.equal(server.connections, 3);
  } finally {
    await agent.session.shutdown();
    await server.close();
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
    assert.equal(socket.request.headers["session-id"], SESSION_IDS.original);
    assert.equal(socket.request.headers["thread-id"], SESSION_IDS.resumed);
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

test("Node Astra sends its model prompt with additive host rules and preserves replacements", async () => {
  const astraPrompt = await readFile(
    new URL("../../../crates/nanocodex-oai-api/prompts/astra.md", import.meta.url),
    "utf8",
  );
  for (const instructions of [undefined, "Caller-owned base instructions."]) {
    const server = await startServer();
    const agent = await createWarmAgent({
      apiKey: "test-key",
      websocketUrl: server.url,
      model: "gpt-6-astra",
      thinking: "low",
      instructions,
      additionalInstructions: "Use the caller's workspace.",
    });
    try {
      const scenario = (async () => {
        const socket = await bounded(server.connection, "Astra connection");
        const reader = messageReader(socket);
        const warmup = await bounded(reader.next(), "Astra warmup");
        assert.equal(warmup.model, "gpt-6-astra");
        assert.equal(warmup.reasoning.summary, undefined);
        assert.equal(warmup.input[1].content[0].text,
          `${instructions ?? astraPrompt}\n\nUse the caller's workspace.`);
        sendWarmup(socket, "astra-warmup");
        await bounded(reader.next(), "Astra turn");
        sendFinal(socket, "astra-final", "done");
      })();
      const result = await bounded(agent.turn.prompt({ input: "hello" }).result(), "Astra result");
      assert.equal(result.finalMessage, "done");
      await scenario;
    } finally {
      await agent.session.shutdown();
      await server.close();
    }
  }
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
    assert.match(warmup.input[0].tools[0].description, new RegExp(visibleTool));
    assert.doesNotMatch(warmup.input[0].tools[0].description, new RegExp(hiddenTool));
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

test("WASM advertises and resumes Code Mode cells through function wait", async () => {
  const server = await startServer();
  let release;
  const blocked = new Promise((resolve) => { release = resolve; });
  const agent = await Agent.create({
    transport: Transport.openAi({ apiKey: "test-key", websocketUrl: server.url, websocketWarmup: true }),
    sessionId: "018f1f9a-7b3c-7a07-8000-000000000007",
    tools: { delayed: { async handler() { await blocked; return "cell-completed-marker"; } } },
  });
  try {
    const scenario = (async () => {
      const socket = await server.connection;
      const reader = messageReader(socket);
      const warmup = await reader.next();
      const specs = warmup.input[0].tools;
      const wait = specs.find((tool) => tool.name === "wait");
      assert.deepEqual(wait.parameters.required, ["cell_id"]);
      assert.match(specs.find((tool) => tool.name === "exec").description, /yield_control/);
      assert.equal(warmup.model, "gpt-6-astra");
      assert.equal(warmup.reasoning.effort, "low");
      sendWarmup(socket, "cell-warmup");
      await reader.next();
      sendCompleted(socket, "cell-start", [{
        type: "custom_tool_call", name: "exec", call_id: "exec-cell",
        input: '// @exec: {"yield_time_ms":0}\ntext("first-cell-chunk"); text(await tools.delayed({}));',
      }]);
      const yielded = await reader.next();
      const output = yielded.input.find((item) => item.type === "custom_tool_call_output");
      const encoded = JSON.stringify(output);
      const cellId = encoded.match(/Script running with cell ID ([a-f0-9-]+:\d+)/)[1];
      assert.match(encoded, /first-cell-chunk/);
      release();
      sendCompleted(socket, "cell-wait", [{
        type: "function_call", name: "wait", call_id: "wait-cell",
        arguments: JSON.stringify({ cell_id: cellId }),
      }]);
      const completed = await reader.next();
      const result = completed.input.find((item) => item.type === "function_call_output");
      assert.equal(result.call_id, "wait-cell");
      assert.match(JSON.stringify(result), /Script completed/);
      assert.match(JSON.stringify(result), /cell-completed-marker/);
      assert.doesNotMatch(JSON.stringify(result), /first-cell-chunk/);
      sendFinal(socket, "cell-final", "done");
    })();
    const [turn] = await bounded(Promise.all([
      agent.turn.prompt({ input: "Run delayed in a yielding cell, then wait for it." }).result(),
      scenario,
    ]), "WASM exec/wait continuation");
    assert.equal(turn.finalMessage, "done");
  } finally {
    release();
    await agent.session.shutdown();
    await server.close();
  }
});

for (const [options, effort] of [
  [{ model: "gpt-5.6-luna" }, "medium"],
  [{ model: "gpt-6-astra", thinking: "high" }, "high"],
]) test(`WASM resolves catalog effort and preserves explicit effort: ${JSON.stringify(options)}`, async () => {
  const server = await startServer();
  const agent = await createWarmAgent({ ...options, apiKey: "test-key", websocketUrl: server.url });
  try {
    const scenario = (async () => {
      const socket = await server.connection;
      const reader = messageReader(socket);
      const request = await reader.next();
      assert.equal(request.model, options.model);
      assert.equal(request.reasoning.effort, effort);
      sendWarmup(socket, "defaults-warmup");
      await reader.next();
      sendFinal(socket, "defaults-final", "done");
    })();
    await bounded(Promise.all([agent.turn.prompt({ input: "Reply done." }).result(), scenario]), "model defaults");
  } finally {
    await agent.session.shutdown();
    await server.close();
  }
});

test("WASM eligible context windows retain Code Mode state, notes, cache identity and durable window identity", async (t) => {
  const sessionId = "018f1f9a-7b3c-7a08-8000-000000000008";
  const subscription = await ChatGptSubscription.open({
    id: "context-window-subscription",
    store: createMemoryChatGptSubscriptionStore("context-window-subscription"),
    seed: {
      accessToken: `e30.${Buffer.from(JSON.stringify({ exp: Math.floor(Date.now() / 1000) + 3600, "https://api.openai.com/auth": { chatgpt_account_id: "context-account", chatgpt_plan_type: "plus" } })).toString("base64url")}.signature`,
      refreshToken: "test-refresh", accountId: "context-account",
    },
  });
  const http = [];
  let note = "";
  t.mock.method(globalThis, "fetch", async (url, init) => {
    const path = new URL(url).pathname;
    const input = JSON.parse(init.body);
    http.push({ path, input, headers: init.headers });
    if (path.endsWith("/thread_hint")) return Response.json({ text: "Progress is in notes/progress." });
    if (path.endsWith("/write_file")) { note = input.content; return Response.json({ encrypted_output: "opaque-note-result" }); }
    if (path.endsWith("/read_file")) return Response.json({ text: note });
    throw new Error(`unexpected HTTP path: ${path}`);
  });
  const durability = createMemoryDurabilityStore("context-window-durable");
  let windowId;
  let cacheKey;
  const metadata = (request) => JSON.parse(request.client_metadata["x-codex-turn-metadata"]);
  try {
    for (const reopened of [false, true]) {
      const server = await startServer();
      const agent = await Agent.create({
        sessionId, durability, durabilityId: "context-window-durable",
        transport: Transport.chatGpt({ subscription, websocketUrl: server.url, websocketWarmup: true }),
        tools: { echo: { handler: (input) => input } },
      });
      try {
        const scenario = (async () => {
          const reader = messageReader(await server.connection);
          const socket = await server.connection;
          const warmup = await reader.next();
          const specs = warmup.input[0].tools;
          assert.ok(specs.some((tool) => tool.name === "new_context"));
          assert.equal(specs.find((tool) => tool.name === "history").tools.length, 4);
          assert.equal(specs.find((tool) => tool.name === "notes").tools.length, 5);
          let first = warmup;
          if (warmup.generate === false) {
            sendWarmup(socket, `context-warmup-${reopened}`);
            first = await reader.next();
          }
          if (reopened) {
            assert.equal(metadata(first).context_window_id, windowId);
            assert.equal(first.prompt_cache_key, cacheKey);
            sendCompleted(socket, "notes-read", [{ type: "function_call", namespace: "notes", name: "read_file", call_id: "read-note", arguments: '{"path":"progress"}' }]);
            assert.match(JSON.stringify((await reader.next()).input), /durable-notes-marker/);
          } else {
            windowId = metadata(first).context_window_id;
            cacheKey = first.prompt_cache_key;
            sendCompleted(socket, "store-cell", [{ type: "custom_tool_call", name: "exec", call_id: "store-before-reset", input: 'store("retained", "live-cell-marker");' }]);
            await reader.next();
            sendCompleted(socket, "notes-write", [{ type: "function_call", namespace: "notes", name: "write_file", call_id: "write-note", arguments: '{"path":"progress","content":"durable-notes-marker"}' }]);
            assert.match(JSON.stringify((await reader.next()).input), /opaque-note-result/);
            sendCompleted(socket, "context-reset", [{ type: "function_call", name: "new_context", call_id: "reset", arguments: "{}" }]);
            const reset = await reader.next();
            assert.equal(reset.prompt_cache_key, cacheKey);
            assert.equal(metadata(reset).window_number, 1);
            assert.notEqual(metadata(reset).context_window_id, windowId);
            windowId = metadata(reset).context_window_id;
            assert.doesNotMatch(JSON.stringify(reset.input), /original-user-marker/);
            sendCompleted(socket, "load-cell", [{ type: "custom_tool_call", name: "exec", call_id: "load-after-reset", input: 'text(load("retained"));' }]);
            assert.match(JSON.stringify((await reader.next()).input), /live-cell-marker/);
          }
          sendFinal(socket, `context-final-${reopened}`, "done");
        })();
        await bounded(Promise.all([agent.turn.prompt({ input: reopened ? "Read saved notes." : "original-user-marker" }).result(), scenario]), "WASM context window lifecycle");
      } finally {
        await agent.session.shutdown();
        await server.close();
      }
    }
    assert.ok(http.some((call) => call.path.endsWith("/read_file")));
    for (const call of http) {
      assert.deepEqual(call.input.context, { session_id: sessionId, current_agent_name: "/root" });
      assert.equal(call.headers.get("chatgpt-account-id"), "context-account");
    }
  } finally { subscription.dispose(); }
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

async function bounded(promise, stage) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_resolve, reject) => {
        timer = setTimeout(() => reject(new Error(`timed out waiting for ${stage}`)), 5_000);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
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
