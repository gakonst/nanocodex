import assert from "node:assert/strict";
import test from "node:test";

import { Agent as BrowserAgent, Transport as BrowserTransport } from "../browser/index.mjs";
import { Agent as NodeAgent, Transport as NodeTransport } from "../node/index.mjs";
import { createTools } from "../index.mjs";
import { managedBrowserVoiceTransport } from "../managed/internal.mjs";

const origin = "https://managed.example";
const agentId = "0198d3f0-8844-7000-8000-000000000001";
const sessionId = "0198d3f0-8844-7000-8000-000000000002";
const apiKey = `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`;
const requestId = "request-1";
const serverTurnId = "server-turn-1";

test("managed transport requires an explicit create or open-existing identity", () => {
  for (const invalid of [
    {},
    { agent: {} },
    { agent: { create: false } },
    { agent: { id: agentId, create: true } },
    { agent: { id: "not-an-agent" } },
  ]) {
    assert.throws(() => NodeTransport.managed(invalid), /explicit create|requires agent/);
  }
  assert.throws(
    () => NodeTransport.managed({ agent: { create: true }, model: "gpt-5.6-sol" }),
    /does not accept model/,
  );
  assert.doesNotThrow(() => NodeTransport.managed({ agent: { create: true } }));
  assert.doesNotThrow(() => BrowserTransport.managed({ agent: { id: agentId } }));
});

test("Agent.create opens an existing managed identity with the common Turn lifecycle", async () => {
  const requests = [];
  const fetch = async (input, init) => {
    const request = new Request(input, init);
    requests.push(request);
    const path = new URL(request.url).pathname;
    if (request.method === "GET" && path === `/v1/agents/${agentId}`) {
      return Response.json({ agent_id: agentId, session_id: sessionId });
    }
    if (request.method === "POST" && path === `/v1/agents/${agentId}/turns`) {
      assert.equal(request.headers.get("idempotency-key"), requestId);
      assert.deepEqual(await request.json(), { input: "hello" });
      return Response.json({
        turn_id: serverTurnId,
        accepted_cursor: "1",
        terminal_cursor: "2",
        terminal: {
          type: "turn_completed",
          id: serverTurnId,
          final_message: "managed hello",
          usage: null,
          citations: [],
        },
      }, { status: 202 });
    }
    if (request.method === "POST" && path.endsWith("/steer")) {
      assert(path.includes(`/${serverTurnId}/`));
      assert.deepEqual(await request.json(), { input: "more" });
      return Response.json({ turn_id: serverTurnId, state: "steering" });
    }
    if (request.method === "POST" && path.endsWith("/cancel")) {
      assert(path.includes(`/${serverTurnId}/`));
      return Response.json({ turn_id: serverTurnId, state: "cancelling" });
    }
    return Response.json({ error: "not_found" }, { status: 404 });
  };
  const agent = await NodeAgent.create({
    transport: NodeTransport.managed({
      agent: { id: agentId },
      baseUrl: origin,
      apiKey,
      fetch,
    }),
  });

  assert.equal(agent.type, "managed");
  assert.equal(agent.agentId, agentId);
  assert.equal(agent.sessionId, sessionId);
  assert.equal(managedBrowserVoiceTransport(agent).origin, origin);
  assert.deepEqual(Object.keys(agent).sort(), [
    "agentId", "dispose", "events", "extend", "key", "name", "session", "sessionId", "turn", "type", "uid",
  ]);
  assert.deepEqual(Object.keys(agent.session), ["shutdown"]);
  const turn = agent.turn.prompt({ id: requestId, input: "hello" });
  assert.equal(turn.agent, agent);
  assert.equal(await turn.accepted(), requestId);
  await turn.steer({ input: "more" });
  await turn.cancel();
  const result = await turn.result();
  assert.equal(result.finalMessage, "managed hello");
  assert.equal(await result.usage(), null);
  result.dispose();
  await assert.rejects(result.usage(), /disposed/);
  turn.dispose();
  const extended = agent.extend((current) => ({ inspect: () => current.sessionId }));
  assert.equal(extended.inspect(), sessionId);

  await agent.session.shutdown();
  assert.throws(() => agent.turn.prompt({ input: "after shutdown" }), /disposed/);
  assert.equal(requests.some((request) => request.method === "DELETE"), false);
  assert.equal(requests[0].method, "GET", "open-existing verifies ownership before readiness");
});

test("managed create reverse-attaches one Tools recipe and shutdown closes it without deleting", async () => {
  const requests = [];
  const socket = new ManagedToolSocket();
  let handshake;
  const tools = await createTools({
    tools: {
      echo: {
        description: "Echo one value.",
        parameters: { type: "object" },
        handler: ({ value }) => value,
      },
    },
  });
  const agent = await BrowserAgent.create({
    transport: BrowserTransport.managed({
      agent: { create: true },
      baseUrl: origin,
      apiKey,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        requests.push(request);
        const path = new URL(request.url).pathname;
        if (request.method === "POST" && path === "/v1/agents") {
          return Response.json({ agent_id: agentId }, { status: 201 });
        }
        assert.equal(request.method, "GET");
        assert.equal(path, `/v1/agents/${agentId}`);
        return Response.json({ agent_id: agentId, session_id: sessionId });
      },
      toolsTransport: (target, options) => {
        handshake = { target, options };
        return socket;
      },
    }),
    tools,
  });

  try {
    assert.equal(agent.agentId, agentId);
    assert.equal(agent.sessionId, sessionId);
    await waitFor(() => socket.frames.some(({ type }) => type === "catalog"));
    assert.equal(handshake.target.href, `wss://managed.example/v1/agents/${agentId}/tool-host`);
    assert.equal(handshake.options.headers.authorization, `Bearer ${apiKey}`);
    assert.equal(JSON.stringify(BrowserTransport.managed({
      agent: { id: agentId }, baseUrl: origin, apiKey,
    })).includes(apiKey), false);
    assert.deepEqual(socket.frames.slice(0, 1).map(({ type }) => type), ["catalog"]);
    assert.equal(socket.frames[0].tools[0].definition.name, "echo");
  } finally {
    await agent.session.shutdown();
  }
  assert.equal(socket.closed.code, 1000);
  assert.throws(() => tools.attach("wss://managed.example/tools"), /closed/);
  assert.equal(requests.some((request) => request.method === "DELETE"), false);
});

test("managed browser Agent reverse-attaches the host page WebMCP provider under existing auth", async () => {
  const socket = new ManagedToolSocket();
  let closed = false;
  const provider = {
    [Symbol.for("nanocodex.webmcp.provider")]: true,
    sourceId: "webmcp:managed-fixture",
    kind: "webmcp",
    mode: "union",
    deferred: true,
    definitions: () => [{
      type: "function",
      name: "web_current_user",
      description: "Read the current website user.",
      strict: false,
      defer_loading: true,
      parameters: { type: "object", additionalProperties: false },
    }],
    resolve: (name) => name === "web_current_user" ? {
      name,
      parallelSafe: true,
      handler: () => ({ id: "website-session-user" }),
    } : undefined,
    async settled() {},
    close() { closed = true; },
  };
  const agent = await BrowserAgent.create({
    transport: BrowserTransport.managed({
      agent: { id: agentId },
      baseUrl: origin,
      apiKey,
      fetch: async () => Response.json({ agent_id: agentId, session_id: sessionId }),
      toolsTransport: () => socket,
    }),
    webMcp: provider,
  });

  await waitFor(() => socket.frames.some(({ type }) => type === "catalog"));
  assert.equal(socket.frames[0].tools[0].definition.name, "web_current_user");
  assert.equal(socket.frames[0].tools[0].provider, "javascript");
  await agent.session.shutdown();
  assert.equal(closed, true);
  assert.equal(socket.closed.code, 1000);
});

test("cold reverse attachment failure does not block the durable Agent and retries under its lifecycle", async () => {
  const socket = new ManagedToolSocket();
  let attempts = 0;
  const tools = await createTools({ tools: { echo: { handler: () => "local" } } });
  const startedAt = Date.now();
  const agent = await NodeAgent.create({
    transport: NodeTransport.managed({
      agent: { id: agentId },
      baseUrl: origin,
      fetch: async () => Response.json({ agent_id: agentId, session_id: sessionId }),
      toolsTransport: () => {
        attempts += 1;
        if (attempts === 1) throw new Error("tool host is cold");
        return socket;
      },
    }),
    tools,
  });

  try {
    assert.equal(agent.sessionId, sessionId);
    assert(Date.now() - startedAt < 200, "durable readiness does not wait for attachment backoff");
    await waitFor(() => attempts >= 1);
    assert.equal(attempts, 1);
    await waitFor(() => socket.frames.some(({ type }) => type === "catalog"), 1_000);
    assert.equal(attempts, 2);
  } finally {
    await agent.session.shutdown();
  }
});

test("managed reverse attachment does not retry a terminal policy rejection", async () => {
  const socket = new ManagedToolSocket();
  socket.rejectCatalog = true;
  let attempts = 0;
  const tools = await createTools();
  const agent = await NodeAgent.create({
    transport: NodeTransport.managed({
      agent: { id: agentId },
      baseUrl: origin,
      fetch: async () => Response.json({ agent_id: agentId, session_id: sessionId }),
      toolsTransport: () => { attempts += 1; return socket; },
    }),
    tools,
  });
  try {
    await waitFor(() => socket.closed?.code === 1008);
    await new Promise((resolve) => setTimeout(resolve, 20));
    assert.equal(attempts, 1);
  } finally {
    await agent.session.shutdown();
  }
});

test("managed reverse attachment keeps retrying after the capped backoff is reached", async () => {
  const socket = new ManagedToolSocket();
  let attempts = 0;
  const tools = await createTools({ tools: { echo: { handler: () => "local" } } });
  const agent = await NodeAgent.create({
    transport: NodeTransport.managed({
      agent: { id: agentId },
      baseUrl: origin,
      fetch: async () => Response.json({ agent_id: agentId, session_id: sessionId }),
      toolsTransport: () => {
        attempts += 1;
        if (attempts <= 5) throw new Error("still offline");
        return socket;
      },
    }),
    tools,
  });

  try {
    await waitFor(() => socket.frames.some(({ type }) => type === "catalog"), 10_000);
    assert.equal(attempts, 6, "attachment recovery is not capped at five attempts");
  } finally {
    await agent.session.shutdown();
  }
});

test("shutdown interrupts managed attachment backoff and observation abort never cancels a turn", async () => {
  let attempts = 0;
  const methods = [];
  const tools = await createTools();
  const agent = await NodeAgent.create({
    transport: NodeTransport.managed({
      agent: { id: agentId },
      baseUrl: origin,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        methods.push(`${request.method} ${new URL(request.url).pathname}`);
        if (new URL(request.url).pathname.endsWith("/events")) {
          return new Promise((_resolve, reject) => request.signal.addEventListener("abort", () => {
            reject(request.signal.reason ?? new DOMException("aborted", "AbortError"));
          }, { once: true }));
        }
        return Response.json({ agent_id: agentId, session_id: sessionId });
      },
      toolsTransport: () => {
        attempts += 1;
        throw new Error("offline");
      },
    }),
    tools,
  });
  const watcher = agent.events.watch();
  const unwatch = watcher.onEvent(() => {});
  await tick();
  unwatch();
  watcher.off();
  await agent.session.shutdown();
  await new Promise((resolve) => setTimeout(resolve, 300));

  assert.equal(attempts, 1, "shutdown cancels the pending retry delay");
  assert.equal(methods.some((request) => request.includes("/cancel")), false);
  assert.deepEqual(methods.slice(0, 2), [
    `GET /v1/agents/${agentId}`,
    `GET /v1/agents/${agentId}/events`,
  ]);
});

test("managed shutdown cancels and settles every client-started turn", async () => {
  let cancels = 0;
  let eventAborted = false;
  let eventOpened = false;
  const fetch = async (input, init) => {
    const request = new Request(input, init);
    const path = new URL(request.url).pathname;
    if (request.method === "GET" && path.endsWith("/events")) {
      eventOpened = true;
      return new Promise((_resolve, reject) => request.signal.addEventListener("abort", () => {
        eventAborted = true;
        reject(request.signal.reason ?? new DOMException("aborted", "AbortError"));
      }, { once: true }));
    }
    if (request.method === "POST" && path.endsWith("/turns")) {
      return Response.json({
        turn_id: serverTurnId,
        state: "accepted",
        input: "wait",
        accepted_cursor: "1",
        terminal_cursor: null,
        terminal: null,
      }, { status: 202 });
    }
    if (request.method === "POST" && path.endsWith("/cancel")) {
      cancels += 1;
      return Response.json({ turn_id: serverTurnId, state: "cancelling" });
    }
    return Response.json({ agent_id: agentId, session_id: sessionId });
  };
  const agent = await NodeAgent.create({
    transport: NodeTransport.managed({ agent: { id: agentId }, baseUrl: origin, fetch }),
  });
  const turn = agent.turn.prompt({ id: "shutdown-owned", input: "wait" });
  assert.equal(await turn.accepted(), "shutdown-owned");
  const result = turn.result();
  await waitFor(() => eventOpened);

  await agent.session.shutdown();
  await assert.rejects(result, /shut down|abort/i);
  assert.equal(cancels, 1);
  assert.equal(eventAborted, true);
});

test("managed event iterators resolve pending reads with the common Agent event shape", async () => {
  const agent = await NodeAgent.create({
    transport: NodeTransport.managed({
      agent: { id: agentId },
      baseUrl: origin,
      fetch: async (input, init) => {
        const request = new Request(input, init);
        if (new URL(request.url).pathname.endsWith("/events")) {
          const data = {
            cursor: "7",
            created_at: 7,
            turn_id: null,
            type: "event",
            event: {
              protocol_version: 1,
              request_id: "server-private",
              seq: 99,
              type: "assistant.message",
              payload: { text: "hello" },
            },
          };
          return new Response(`id: 7\nevent: message\ndata: ${JSON.stringify(data)}\n\n`, {
            headers: { "content-type": "text/event-stream" },
          });
        }
        return Response.json({ agent_id: agentId, session_id: sessionId });
      },
    }),
  });
  const watcher = agent.events.watch();
  const iterator = watcher[Symbol.asyncIterator]();
  try {
    const pending = iterator.next();
    assert.deepEqual(await pending, {
      done: false,
      value: {
        protocol_version: 1,
        request_id: sessionId,
        seq: 7,
        type: "assistant.message",
        payload: { text: "hello" },
      },
    });
  } finally {
    await iterator.return();
    watcher.off();
    await agent.session.shutdown();
  }
});

test("managed Agent.create rejects local-only policy even when JavaScript bypasses types", async () => {
  const transport = NodeTransport.managed({
    agent: { id: agentId },
    baseUrl: origin,
    fetch: async () => Response.json({ agent_id: agentId, session_id: sessionId }),
  });
  await assert.rejects(
    NodeAgent.create({ transport, model: "gpt-5.6-sol" }),
    /does not accept model/,
  );
  await assert.rejects(
    NodeAgent.create({ transport, tools: {} }),
    /created by createTools/,
  );
});

class ManagedToolSocket {
  readyState = 1;
  frames = [];
  listeners = new Map();
  rejectCatalog = false;

  addEventListener(type, listener) {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  send(encoded) {
    const frame = JSON.parse(encoded);
    this.frames.push(frame);
    if (frame.type === "catalog") queueMicrotask(() => {
      if (this.rejectCatalog) this.close(1008, "catalog rejected");
      else this.receive({ type: "ready" });
    });
    else if (frame.type === "drain") queueMicrotask(() => this.receive({ type: "draining" }));
  }

  receive(frame) {
    for (const listener of this.listeners.get("message") ?? []) {
      listener({ data: JSON.stringify(frame) });
    }
  }

  close(code, reason) {
    this.readyState = 3;
    this.closed = { code, reason };
    for (const listener of this.listeners.get("close") ?? []) listener({ code, reason });
  }
}

function tick() {
  return new Promise((resolve) => setImmediate(resolve));
}

async function waitFor(predicate, timeoutMs = 500) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) throw new Error("timed out waiting for managed transport state");
    await tick();
  }
}
